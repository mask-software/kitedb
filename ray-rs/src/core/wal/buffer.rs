//! Linear WAL buffer for single-file format with dual-region support
//!
//! Ported from src/core/wal-buffer.ts
//!
//! The WAL uses a linear buffer design within the database file.
//! Records append until the active region is full, then require a checkpoint
//! to reclaim space.
//!
//! Dual-Region Mode (for background checkpointing):
//! - Primary region: 75% of WAL space (normal writes)
//! - Secondary region: 25% of WAL space (writes during checkpoint)
//!
//! Optimization: Uses page-level write batching to reduce I/O amplification.
//! Instead of writing each small record individually (causing read-modify-write
//! for each ~100 byte record on a 4KB page), we buffer writes in memory and
//! flush entire pages at once.

use std::collections::{HashMap, HashSet};

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::*;

use super::record::{parse_wal_record, read_wal_record, ParsedWalRecord, WalRecord, WalRecordAt};

/// WAL region split ratio: primary gets 75%, secondary gets 25%
const PRIMARY_REGION_RATIO: f64 = 0.75;

/// WAL buffer for single-file database format
pub struct WalBuffer {
  /// Base offset in file (start of WAL area)
  base_offset: u64,
  /// Total size of WAL area in bytes
  capacity: u64,
  /// Current head position (write pointer, relative to base)
  head: u64,
  /// Current tail position (oldest valid record, relative to base)
  tail: u64,
  /// Page size for batching
  page_size: usize,
  /// Pending page writes (page_offset -> page_data)
  /// page_offset is absolute file offset
  pending_writes: HashMap<u64, Vec<u8>>,

  // Dual-region support for background checkpointing
  /// Size of primary region (75%)
  primary_region_size: u64,
  /// Start offset of secondary region (relative to base)
  secondary_region_start: u64,
  /// Size of secondary region (25%)
  secondary_region_size: u64,
  /// Active region: 0=primary, 1=secondary
  active_region: u8,
  /// Primary region write position (relative to base)
  primary_head: u64,
  /// Secondary region write position (relative to base)
  secondary_head: u64,
}

impl WalBuffer {
  /// Create a new WAL buffer
  pub fn new(base_offset: u64, capacity: u64, page_size: usize) -> Self {
    let primary_region_size = (capacity as f64 * PRIMARY_REGION_RATIO) as u64;
    let secondary_region_start = primary_region_size;
    let secondary_region_size = capacity - primary_region_size;

    Self {
      base_offset,
      capacity,
      head: 0,
      tail: 0,
      page_size,
      pending_writes: HashMap::new(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region: 0,
      primary_head: 0,
      secondary_head: secondary_region_start,
    }
  }

  /// Create from existing header state
  pub fn from_header(header: &DbHeaderV1) -> Self {
    let base_offset = header.wal_start_page * header.page_size as u64;
    let capacity = header.wal_page_count * header.page_size as u64;

    let primary_region_size = (capacity as f64 * PRIMARY_REGION_RATIO) as u64;
    let secondary_region_start = primary_region_size;
    let secondary_region_size = capacity - primary_region_size;

    // Initialize from V2 header fields
    let active_region = header.active_wal_region;
    let mut primary_head = header.wal_primary_head;
    let mut secondary_head = header.wal_secondary_head;

    // Headers written before the region fields existed carry only wal_head, a
    // primary-region position. Every header with the region fields records
    // wal_primary_head == wal_head while the primary region is active, and a
    // wal_head in the secondary region while it is active (a cut, which may
    // leave the primary region empty, or a retired primary region). So
    // wal_head stands in for a missing primary head only if it can be one:
    // taking a secondary position as the primary head would replay stale bytes
    // of an earlier WAL cycle, or put the head past the primary region.
    if primary_head == 0
      && header.wal_head > 0
      && (active_region == 0 || header.wal_head < secondary_region_start)
    {
      primary_head = header.wal_head;
    }

    // Initialize secondaryHead to its start position if not set
    if secondary_head == 0 {
      secondary_head = secondary_region_start;
    }

    // If we were writing to secondary and the header update was interrupted,
    // wal_head may be ahead of wal_secondary_head. Use wal_head as fallback.
    if active_region == 1
      && secondary_head <= secondary_region_start
      && header.wal_head >= secondary_region_start
    {
      secondary_head = header.wal_head;
    }

    Self {
      base_offset,
      capacity,
      head: header.wal_head,
      tail: header.wal_tail,
      page_size: header.page_size as usize,
      pending_writes: HashMap::new(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region,
      primary_head,
      secondary_head,
    }
  }

  /// Get the base offset in the file
  pub fn base_offset(&self) -> u64 {
    self.base_offset
  }

  /// Get the capacity
  pub fn capacity(&self) -> u64 {
    self.capacity
  }

  /// Get current head position (relative to base)
  pub fn head(&self) -> u64 {
    self.head
  }

  /// Get current tail position (relative to base)
  pub fn tail(&self) -> u64 {
    self.tail
  }

  /// Get primary region head (relative to base)
  pub fn primary_head(&self) -> u64 {
    self.primary_head
  }

  /// Get secondary region head (relative to base)
  pub fn secondary_head(&self) -> u64 {
    self.secondary_head
  }

  /// Whether the secondary region contains records from a background cut.
  pub fn has_secondary_records(&self) -> bool {
    self.secondary_head > self.secondary_region_start
  }

  /// Get active region (0=primary, 1=secondary)
  pub fn active_region(&self) -> u8 {
    self.active_region
  }

  /// Get primary region size in bytes
  pub fn primary_region_size(&self) -> u64 {
    self.primary_region_size
  }

  /// Get secondary region size in bytes
  pub fn secondary_region_size(&self) -> u64 {
    self.secondary_region_size
  }

  /// Check if buffer is empty
  pub fn is_empty(&self) -> bool {
    self.head == self.tail
  }

  /// Get used space (for active region in dual-region mode)
  pub fn used(&self) -> u64 {
    if self.active_region == 0 {
      // Primary region: simple linear usage
      self.primary_head - self.tail
    } else {
      // Secondary region: usage is just secondary head minus start
      self.secondary_head - self.secondary_region_start
    }
  }

  /// Get free space in active region
  pub fn free(&self) -> u64 {
    if self.active_region == 0 {
      // Primary region available space
      self.primary_region_size.saturating_sub(self.primary_head)
    } else {
      // Secondary region available space
      self
        .secondary_region_size
        .saturating_sub(self.secondary_head - self.secondary_region_start)
    }
  }

  /// Get usage ratio (0.0 - 1.0) for active region
  ///
  /// This measures how much of the linear region is consumed, which is what
  /// bounds further appends. Checkpoints keep it meaningful by rewinding the
  /// primary region to offset 0 instead of leaving a retired prefix behind.
  pub fn usage_ratio(&self) -> f64 {
    if self.active_region == 0 {
      self.primary_head as f64 / self.primary_region_size as f64
    } else {
      (self.secondary_head - self.secondary_region_start) as f64 / self.secondary_region_size as f64
    }
  }

  /// Check if we can fit a record of given size in active region
  pub fn can_fit(&self, size: usize) -> bool {
    let aligned_size = align_up(size, WAL_RECORD_ALIGNMENT) as u64;
    aligned_size <= self.free()
  }

  // ========================================================================
  // Dual-Region Methods (for background checkpointing)
  // ========================================================================

  /// Switch writes to secondary region (called when starting background checkpoint)
  pub fn switch_to_secondary(&mut self) {
    if self.active_region == 1 {
      return; // Already in secondary
    }
    self.active_region = 1;
    // Update head to track active position
    self.head = self.secondary_head;
  }

  /// The records a background checkpoint cut must copy into the secondary
  /// region, built and concatenated: those of every transaction in `open`
  /// that has no COMMIT or ROLLBACK in the primary region, from its last
  /// BEGIN on, in WAL order.
  ///
  /// Copied, each such transaction lies wholly in the secondary region, which
  /// is all of the WAL the checkpoint keeps, so it stays replayable when it
  /// commits after the cut. Replay keeps only a transaction's records after
  /// its last BEGIN, so a recovery that merges both regions applies the
  /// copies once.
  ///
  /// Fails with `WalBufferFull` if they do not fit in the secondary region,
  /// and with `InvalidWal` if a transaction in `open` has neither a BEGIN nor
  /// a COMMIT or ROLLBACK record in the primary region (its records cannot be
  /// read, so they cannot be copied).
  pub fn open_transaction_records(
    &mut self,
    open: &HashSet<TxId>,
    pager: &mut FilePager,
  ) -> Result<Vec<u8>> {
    let mut carried = Vec::new();
    if open.is_empty() {
      return Ok(carried);
    }

    let records = self.scan_region(0, pager)?;
    let unseen = transactions_without_boundaries(&records, open);
    if !unseen.is_empty() {
      return Err(KiteError::InvalidWal(format!(
        "no BEGIN record found for open transactions {unseen:?}"
      )));
    }
    let mut last_begin: HashMap<TxId, usize> = HashMap::new();
    for (index, record) in records.iter().enumerate() {
      if !open.contains(&record.txid) {
        continue;
      }
      match record.record_type {
        WalRecordType::Begin => {
          last_begin.insert(record.txid, index);
        }
        WalRecordType::Commit | WalRecordType::Rollback => {
          last_begin.remove(&record.txid);
        }
        _ => {}
      }
    }
    for (index, record) in records.into_iter().enumerate() {
      if last_begin
        .get(&record.txid)
        .is_some_and(|begin| index >= *begin)
      {
        let record = WalRecord::new(record.record_type, record.txid, record.payload);
        carried.extend_from_slice(&record.build());
      }
    }

    if carried.len() as u64 > self.secondary_region_size {
      return Err(KiteError::WalBufferFull);
    }
    Ok(carried)
  }

  /// Append records from [`Self::open_transaction_records`] to the empty
  /// secondary region, which a background checkpoint cut has just made
  /// active. They are buffered, like any write. On error the buffer is left
  /// as it was.
  pub fn carry_into_secondary(&mut self, records: &[u8], pager: &mut FilePager) -> Result<()> {
    if self.active_region != 1 || self.has_secondary_records() {
      return Err(KiteError::Internal(
        "open transactions are carried only into an empty active secondary region".to_string(),
      ));
    }
    if records.is_empty() {
      return Ok(());
    }

    // Flush earlier writes first so an error below drops only the copies.
    self.flush(pager)?;
    let prior = self.region_state();
    if let Err(error) = self.write_record_bytes_batch(records, pager) {
      self.pending_writes.clear();
      self.restore_region_state(prior);
      return Err(error);
    }
    Ok(())
  }

  /// Switch writes back to primary region (called after checkpoint completes)
  /// If reset_primary is true, resets the primary region (checkpoint completed)
  pub fn switch_to_primary(&mut self, reset_primary: bool) {
    if self.active_region == 0 && !reset_primary {
      return; // Already in primary and no reset needed
    }
    self.active_region = 0;
    if reset_primary {
      // Reset primary head (checkpoint completed, WAL is cleared)
      self.primary_head = 0;
      self.tail = 0;
    }
    // Update head to track active position
    self.head = self.primary_head;
  }

  /// Retire every primary-region record once a checkpoint snapshot covers them.
  ///
  /// Records written to the secondary region after the checkpoint cut stay in
  /// place and become the whole live WAL: `tail..head` spans exactly the
  /// secondary records. The primary region is fenced (`tail == primary_head
  /// == primary_region_size`) so nothing appends to it until
  /// [`Self::compact_secondary_into_primary`] rewrites the retained records at
  /// its start. No bytes are written, so the WAL named by the previous header
  /// stays intact until a header for this state is durable.
  pub fn retire_primary_region(&mut self) {
    self.primary_head = self.primary_region_size;
    self.tail = self.secondary_region_start;
    self.active_region = 1;
    self.head = self.secondary_head;
  }

  /// Whether this buffer is in the state produced by
  /// [`Self::retire_primary_region`]: the WAL lives in the secondary region
  /// while no checkpoint is cutting it.
  pub fn is_primary_retired(&self) -> bool {
    self.active_region == 1 && self.tail == self.secondary_region_start
  }

  /// Rewrite the secondary region's records at the start of the primary region,
  /// then make the primary region active again.
  ///
  /// The rewritten bytes are flushed and synced before this returns. The
  /// caller must ensure the header a crash would recover from names only the
  /// secondary records (for example, a header recording
  /// [`Self::retire_primary_region`] is durable in both slots, or is the newest
  /// slot and the next header goes to the other one), and must then persist a
  /// header for the new state. Until then the secondary records remain the
  /// crash fallback.
  ///
  /// Precondition: the primary records are already retired (see
  /// [`Self::is_primary_retired`]); only the secondary records are kept.
  ///
  /// On error the region state is left unchanged, so it still matches the
  /// durable header that names the secondary records.
  pub fn compact_secondary_into_primary(&mut self, pager: &mut FilePager) -> Result<()> {
    // Flush earlier writes first so an error below drops only the pages this
    // rewrite buffered.
    self.flush(pager)?;
    let retained = self.region_state();
    let result = self
      .merge_secondary_into_primary(pager)
      .and_then(|()| self.flush(pager))
      .and_then(|()| pager.sync());
    if result.is_err() {
      self.pending_writes.clear();
      self.restore_region_state(retained);
    }
    result
  }

  /// Capture the positions and active region, so a transition whose header
  /// fails to persist can be rolled back to match the durable header.
  pub fn region_state(&self) -> WalRegionState {
    WalRegionState {
      head: self.head,
      tail: self.tail,
      primary_head: self.primary_head,
      secondary_head: self.secondary_head,
      active_region: self.active_region,
    }
  }

  /// Restore positions captured by [`Self::region_state`]. Pending writes are
  /// not touched.
  pub fn restore_region_state(&mut self, state: WalRegionState) {
    self.head = state.head;
    self.tail = state.tail;
    self.primary_head = state.primary_head;
    self.secondary_head = state.secondary_head;
    self.active_region = state.active_region;
  }

  /// Record this buffer's positions and active region in `header`.
  pub fn store_in_header(&self, header: &mut DbHeaderV1) {
    header.wal_head = self.head;
    header.wal_tail = self.tail;
    header.wal_primary_head = self.primary_head;
    header.wal_secondary_head = self.secondary_head;
    header.active_wal_region = self.active_region;
  }

  /// Merge secondary records into a fresh primary region (buffered, not
  /// flushed). Checkpoint completion uses
  /// [`Self::compact_secondary_into_primary`], which adds the flush, sync, and
  /// error rollback that make the rewrite safe to install.
  pub fn merge_secondary_into_primary(&mut self, pager: &mut FilePager) -> Result<()> {
    let has_secondary_records = self.secondary_head > self.secondary_region_start;
    let secondary_records = if has_secondary_records {
      self.scan_region(1, pager)?
    } else {
      Vec::new()
    };

    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.tail = 0;
    self.active_region = 0;
    self.head = 0;

    for record in secondary_records {
      let wal_record = WalRecord::new(record.record_type, record.txid, record.payload);
      let record_bytes = wal_record.build();
      self.write_record_bytes_to_primary(&record_bytes, pager)?;
    }

    Ok(())
  }

  /// Leave a background checkpoint's cut by appending the secondary region's
  /// records to the primary region, after its own, and making the primary
  /// region active again. Records of transactions carried at the cut are then
  /// there twice; replay keeps only those after a transaction's last BEGIN.
  ///
  /// Only bytes past the primary head are written, and they are synced before
  /// this returns, so a header naming the cut stays a valid crash fallback
  /// until the caller installs a header for the merged state.
  ///
  /// Returns `false`, changing nothing, if the records do not fit in the
  /// primary region, or if the primary region holds bytes up to its head that
  /// do not parse as records (they would be overwritten while a durable
  /// header still names them; see [`Self::trim_to_valid_records`]). The cut
  /// state is a valid WAL in both cases: replay reads both regions in place.
  /// On error the region state is left unchanged.
  pub fn merge_cut_into_primary(&mut self, pager: &mut FilePager) -> Result<bool> {
    // Flush earlier writes first so an error below drops only the merge's.
    self.flush(pager)?;
    let (_, primary_end) = self.scan_region_to_end(0, pager)?;
    if primary_end != self.primary_head {
      return Ok(false);
    }
    let mut merged = Vec::new();
    for record in self.scan_region(1, pager)? {
      merged.extend_from_slice(
        &WalRecord::new(record.record_type, record.txid, record.payload).build(),
      );
    }
    if self.primary_head + merged.len() as u64 > self.primary_region_size {
      return Ok(false);
    }

    let cut = self.region_state();
    if !merged.is_empty() {
      let written = self
        .buffer_write(self.file_offset(self.primary_head), &merged, pager)
        .and_then(|()| self.flush(pager))
        .and_then(|()| pager.sync());
      if let Err(error) = written {
        self.pending_writes.clear();
        self.restore_region_state(cut);
        return Err(error);
      }
    }
    self.primary_head += merged.len() as u64;
    self.secondary_head = self.secondary_region_start;
    self.active_region = 0;
    self.head = self.primary_head;
    Ok(true)
  }

  /// Bytes the records of both regions take: what
  /// [`Self::merge_cut_into_primary`] would leave in the primary region.
  #[cfg(test)]
  pub fn merged_cut_size(&mut self, pager: &mut FilePager) -> Result<u64> {
    let (_, primary_end) = self.scan_region_to_end(0, pager)?;
    let (_, secondary_end) = self.scan_region_to_end(1, pager)?;
    Ok(primary_end + (secondary_end - self.secondary_region_start))
  }

  /// Move each head back to the end of the last record that parses in its
  /// region, so new records never land after unreadable bytes, where replay
  /// (which stops at the first bad record) would never reach them. A crash
  /// part-way through a commit can leave such bytes: the header naming them
  /// became durable, a WAL page did not.
  ///
  /// Only the regions in use are trimmed: the primary region (unless
  /// retired), and the secondary region while it is active. Returns whether a
  /// head moved; the caller then persists a header naming the trimmed WAL
  /// before writing more records.
  pub fn trim_to_valid_records(&mut self, pager: &mut FilePager) -> Result<bool> {
    let mut trimmed = false;
    if !self.is_primary_retired() {
      let (_, primary_end) = self.scan_region_to_end(0, pager)?;
      trimmed |= primary_end != self.primary_head;
      self.primary_head = primary_end;
    }
    if self.active_region == 1 {
      let (_, secondary_end) = self.scan_region_to_end(1, pager)?;
      trimmed |= secondary_end != self.secondary_head;
      self.secondary_head = secondary_end;
    }
    self.head = if self.active_region == 0 {
      self.primary_head
    } else {
      self.secondary_head
    };
    Ok(trimmed)
  }

  /// Fail if a region's records stop before its head at a record whose CRC
  /// checks but whose type this version does not know (a newer version wrote
  /// it). It is not torn, so trimming or compacting the region, which keeps
  /// only the records before it, would drop it and every record after it for
  /// good. Writable opens check this before rewriting anything; replay stops
  /// at such a record either way.
  pub fn check_record_types(&mut self, pager: &mut FilePager) -> Result<()> {
    for (region, head) in [(0, self.primary_head), (1, self.secondary_head)] {
      let (_, end) = self.scan_region_to_end(region, pager)?;
      if end >= head {
        continue;
      }
      let bytes = self.read_at_offset(self.file_offset(end), (head - end) as usize, pager)?;
      if let WalRecordAt::UnknownType(record_type) = read_wal_record(&bytes, 0) {
        return Err(KiteError::InvalidWal(format!(
          "WAL record of unknown type {record_type} at offset {end}, probably written by a \
           newer version; open the database read-only, or with that version"
        )));
      }
    }
    Ok(())
  }

  /// Whether the secondary region holds every transaction it commits whole
  /// (each COMMIT follows a BEGIN of its transaction there), and a BEGIN or
  /// ROLLBACK of each transaction in `open`. Then the secondary region alone
  /// replays every transaction committed or still open since the cut.
  pub fn secondary_holds_whole_transactions(
    &mut self,
    open: &HashSet<TxId>,
    pager: &mut FilePager,
  ) -> Result<bool> {
    let mut begun = HashSet::new();
    let mut rolled_back = HashSet::new();
    for record in self.scan_region(1, pager)? {
      match record.record_type {
        WalRecordType::Begin => {
          begun.insert(record.txid);
        }
        WalRecordType::Commit if !begun.contains(&record.txid) => return Ok(false),
        WalRecordType::Rollback => {
          rolled_back.insert(record.txid);
        }
        _ => {}
      }
    }
    Ok(
      open
        .iter()
        .all(|txid| begun.contains(txid) || rolled_back.contains(txid)),
    )
  }

  /// Scan records from a specific region
  /// region: 0 for primary, 1 for secondary
  pub fn scan_region(&mut self, region: u8, pager: &mut FilePager) -> Result<Vec<ParsedWalRecord>> {
    self
      .scan_region_to_end(region, pager)
      .map(|(records, _)| records)
  }

  /// Scan a region's records up to the first that does not parse. Also
  /// returns where they end (relative to the WAL start).
  fn scan_region_to_end(
    &mut self,
    region: u8,
    pager: &mut FilePager,
  ) -> Result<(Vec<ParsedWalRecord>, u64)> {
    let (start, end) = if region == 0 {
      (self.tail, self.primary_head)
    } else {
      (self.secondary_region_start, self.secondary_head)
    };
    if start >= end {
      return Ok((Vec::new(), end));
    }

    // Read the region once rather than each record's pages separately.
    let bytes = self.read_at_offset(self.file_offset(start), (end - start) as usize, pager)?;
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
      match parse_wal_record(&bytes, offset) {
        Some(record) => {
          offset = record.record_end;
          records.push(record);
        }
        None => break, // Invalid record
      }
    }
    Ok((records, start + offset as u64))
  }

  /// Write record bytes specifically to primary region (used during merge)
  fn write_record_bytes_to_primary(
    &mut self,
    record_bytes: &[u8],
    pager: &mut FilePager,
  ) -> Result<u64> {
    let record_size = record_bytes.len();
    let aligned_size = align_up(record_size, WAL_RECORD_ALIGNMENT);

    // Check if fits in primary region
    if self.primary_head + aligned_size as u64 > self.primary_region_size {
      return Err(KiteError::WalBufferFull);
    }

    // Calculate file offset
    let file_offset = self.file_offset(self.primary_head);

    // Buffer the write
    self.buffer_write(file_offset, record_bytes, pager)?;

    // Update primary head
    self.primary_head += aligned_size as u64;
    self.head = self.primary_head;

    Ok(self.primary_head)
  }

  /// Calculate the file offset for a buffer-relative position
  pub fn file_offset(&self, buffer_pos: u64) -> u64 {
    self.base_offset + buffer_pos
  }

  /// Reserve space for a record, returning the write position
  /// Returns None if buffer is full
  pub fn reserve(&mut self, size: usize) -> Option<u64> {
    let aligned_size = align_up(size, WAL_RECORD_ALIGNMENT) as u64;

    if !self.can_fit(aligned_size as usize) {
      return None;
    }

    if self.active_region == 0 {
      // Primary region (linear, no wrap)
      let write_pos = self.primary_head;
      if self.primary_head + aligned_size > self.primary_region_size {
        return None;
      }
      self.primary_head += aligned_size;
      self.head = self.primary_head;
      Some(write_pos)
    } else {
      // Secondary region (no wrap)
      let write_pos = self.secondary_head;
      self.secondary_head += aligned_size;
      self.head = self.secondary_head;
      Some(write_pos)
    }
  }

  /// Write a WAL record to the buffer
  /// Returns the new head position
  ///
  /// Note: Records are buffered in memory. Call flush() to write to disk.
  pub fn write_record(&mut self, record: &WalRecord, pager: &mut FilePager) -> Result<u64> {
    let record_bytes = record.build();
    self.write_record_bytes(&record_bytes, pager)
  }

  /// Write prebuilt record bytes in a single batch
  /// The buffer must contain a sequence of padded records (alignment-sized).
  pub fn write_record_bytes_batch(
    &mut self,
    record_bytes: &[u8],
    pager: &mut FilePager,
  ) -> Result<u64> {
    if record_bytes.is_empty() {
      return Ok(self.head);
    }

    if record_bytes.len() % WAL_RECORD_ALIGNMENT != 0 {
      return Err(KiteError::Internal(
        "WAL batch bytes must be alignment-sized".to_string(),
      ));
    }

    if !self.can_fit(record_bytes.len()) {
      return Err(KiteError::WalBufferFull);
    }

    if self.active_region == 0 {
      if self.primary_head + record_bytes.len() as u64 > self.primary_region_size {
        return Err(KiteError::WalBufferFull);
      }

      let file_offset = self.file_offset(self.primary_head);
      self.buffer_write(file_offset, record_bytes, pager)?;
      self.primary_head += record_bytes.len() as u64;
      self.head = self.primary_head;
    } else {
      if self.secondary_head + record_bytes.len() as u64
        > self.secondary_region_start + self.secondary_region_size
      {
        return Err(KiteError::WalBufferFull);
      }

      let file_offset = self.file_offset(self.secondary_head);
      self.buffer_write(file_offset, record_bytes, pager)?;
      self.secondary_head += record_bytes.len() as u64;
      self.head = self.secondary_head;
    }

    Ok(self.head)
  }

  /// Write raw record bytes to the active region
  fn write_record_bytes(&mut self, record_bytes: &[u8], pager: &mut FilePager) -> Result<u64> {
    let record_size = record_bytes.len();
    let aligned_size = align_up(record_size, WAL_RECORD_ALIGNMENT);

    if !self.can_fit(aligned_size) {
      return Err(KiteError::WalBufferFull);
    }

    if self.active_region == 0 {
      // Primary region (linear, no wrap)
      if self.primary_head + aligned_size as u64 > self.primary_region_size {
        return Err(KiteError::WalBufferFull);
      }

      // Calculate file offset
      let file_offset = self.file_offset(self.primary_head);

      // Buffer the write
      self.buffer_write(file_offset, record_bytes, pager)?;

      // Update head
      self.primary_head += aligned_size as u64;
      self.head = self.primary_head;
    } else {
      // Secondary region (no wrap-around)
      let file_offset = self.file_offset(self.secondary_head);

      // Buffer the write
      self.buffer_write(file_offset, record_bytes, pager)?;

      // Update head
      self.secondary_head += aligned_size as u64;
      self.head = self.secondary_head;
    }

    Ok(self.head)
  }

  /// Buffer a write for later flushing (page-level batching)
  /// This reduces I/O amplification by accumulating writes to the same page
  fn buffer_write(&mut self, offset: u64, data: &[u8], pager: &mut FilePager) -> Result<()> {
    let page_size = self.page_size as u64;
    let start_page = offset / page_size;
    let end_page = (offset + data.len() as u64 - 1) / page_size;

    let mut data_offset = 0usize;

    for page_idx in start_page..=end_page {
      let page_file_offset = page_idx * page_size;

      // Get or create the page buffer
      let page_buffer = match self.pending_writes.entry(page_file_offset) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
          // First write to this page - load existing content
          let page_num = (page_file_offset / page_size) as u32;
          let existing = pager.read_page(page_num)?;
          entry.insert(existing)
        }
      };

      let page_start = page_file_offset;
      let page_end = page_start + page_size;

      let write_start = offset.max(page_start);
      let write_end = (offset + data.len() as u64).min(page_end);
      let write_len = (write_end - write_start) as usize;

      let page_write_offset = (write_start - page_start) as usize;

      page_buffer[page_write_offset..page_write_offset + write_len]
        .copy_from_slice(&data[data_offset..data_offset + write_len]);

      data_offset += write_len;
    }

    Ok(())
  }

  /// Read bytes from a specific file offset
  /// If there are pending writes, reads from the buffered data
  fn read_at_offset(&self, offset: u64, length: usize, pager: &mut FilePager) -> Result<Vec<u8>> {
    let page_size = self.page_size as u64;
    let start_page = offset / page_size;
    let end_page = (offset + length as u64 - 1) / page_size;

    // For reads within a single page
    if start_page == end_page {
      let page_file_offset = start_page * page_size;
      let page_offset = (offset - page_file_offset) as usize;

      // Check for pending writes first
      if let Some(pending_page) = self.pending_writes.get(&page_file_offset) {
        return Ok(pending_page[page_offset..page_offset + length].to_vec());
      }

      let page_num = start_page as u32;
      let page = pager.read_page(page_num)?;
      return Ok(page[page_offset..page_offset + length].to_vec());
    }

    // For reads spanning multiple pages
    let mut result = vec![0u8; length];
    let mut result_offset = 0;

    for page_idx in start_page..=end_page {
      let page_file_offset = page_idx * page_size;
      let page_start = page_file_offset;
      let page_end = page_start + page_size;

      let read_start = offset.max(page_start);
      let read_end = (offset + length as u64).min(page_end);
      let read_len = (read_end - read_start) as usize;

      let page_read_offset = (read_start - page_start) as usize;

      // Check for pending writes first
      let page_data = if let Some(pending) = self.pending_writes.get(&page_file_offset) {
        pending.clone()
      } else {
        let page_num = page_idx as u32;
        pager.read_page(page_num)?
      };

      result[result_offset..result_offset + read_len]
        .copy_from_slice(&page_data[page_read_offset..page_read_offset + read_len]);

      result_offset += read_len;
    }

    Ok(result)
  }

  /// Flush all pending writes to disk
  /// This writes all buffered pages in a single batch
  pub fn flush(&mut self, pager: &mut FilePager) -> Result<()> {
    let page_size = self.page_size as u64;

    for (&page_file_offset, data) in &self.pending_writes {
      let page_num = (page_file_offset / page_size) as u32;
      pager.write_page(page_num, data)?;
    }

    self.pending_writes.clear();
    Ok(())
  }

  /// Flush and sync to disk
  pub fn sync(&mut self, pager: &mut FilePager) -> Result<()> {
    self.flush(pager)?;
    pager.sync()?;
    Ok(())
  }

  /// Check if there are pending writes
  pub fn has_pending_writes(&self) -> bool {
    !self.pending_writes.is_empty()
  }

  /// Advance tail after checkpoint
  pub fn advance_tail(&mut self, new_tail: u64) {
    self.tail = new_tail;
  }

  /// Reset the buffer (after checkpoint)
  pub fn reset(&mut self) {
    self.head = 0;
    self.tail = 0;
    self.pending_writes.clear();
    // Also reset dual-region state
    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.active_region = 0;
  }

  /// Clear pending writes without flushing
  pub fn discard_pending(&mut self) {
    self.pending_writes.clear();
  }

  /// Scan all valid records from tail to head
  pub fn scan_records(&mut self, pager: &mut FilePager) -> Result<Vec<ParsedWalRecord>> {
    let mut records = Vec::new();

    if self.is_empty() {
      return Ok(records);
    }

    if self.head < self.tail {
      return Err(KiteError::InvalidWal(
        "WAL head cannot be behind tail in linear mode".to_string(),
      ));
    }

    let mut pos = self.tail;

    while pos < self.head {
      // Read the record header
      let file_offset = self.file_offset(pos);
      let header_bytes = self.read_at_offset(file_offset, 8, pager)?;

      let rec_len = read_u32(&header_bytes, 0) as usize;

      if rec_len == 0 {
        break;
      }

      // Calculate total record size with alignment
      let pad_len = padding_for(rec_len, WAL_RECORD_ALIGNMENT);
      let total_len = rec_len + pad_len;

      // Read full record
      let record_bytes = self.read_at_offset(file_offset, total_len, pager)?;

      // Parse the record
      match parse_wal_record(&record_bytes, 0) {
        Some(record) => {
          records.push(record);
          pos += total_len as u64;
        }
        None => break, // Invalid record
      }
    }

    Ok(records)
  }

  /// Get statistics about the WAL buffer
  pub fn stats(&self) -> WalBufferStats {
    WalBufferStats {
      capacity: self.capacity,
      used: self.used(),
      free: self.free(),
      head: self.head,
      tail: self.tail,
      pending_pages: self.pending_writes.len(),
      primary_head: self.primary_head,
      secondary_head: self.secondary_head,
      active_region: self.active_region,
    }
  }

  /// Get records for recovery (from both regions if checkpoint was in progress)
  pub fn records_for_recovery(&mut self, pager: &mut FilePager) -> Result<Vec<ParsedWalRecord>> {
    // Scan primary region first
    let mut records = self.scan_region(0, pager)?;

    // If checkpoint was in progress (secondary region has data), include those too
    if self.secondary_head > self.secondary_region_start {
      let secondary_records = self.scan_region(1, pager)?;
      records.extend(secondary_records);
    }

    Ok(records)
  }
}

/// Transactions in `txids` with no BEGIN, COMMIT, or ROLLBACK among `records`.
fn transactions_without_boundaries(
  records: &[ParsedWalRecord],
  txids: &HashSet<TxId>,
) -> HashSet<TxId> {
  let mut unseen = txids.clone();
  for record in records {
    if matches!(
      record.record_type,
      WalRecordType::Begin | WalRecordType::Commit | WalRecordType::Rollback
    ) {
      unseen.remove(&record.txid);
    }
  }
  unseen
}

/// Saved WAL positions; see [`WalBuffer::region_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalRegionState {
  head: u64,
  tail: u64,
  primary_head: u64,
  secondary_head: u64,
  active_region: u8,
}

/// WAL buffer statistics
#[derive(Debug, Clone)]
pub struct WalBufferStats {
  pub capacity: u64,
  pub used: u64,
  pub free: u64,
  pub head: u64,
  pub tail: u64,
  pub pending_pages: usize,
  pub primary_head: u64,
  pub secondary_head: u64,
  pub active_region: u8,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::pager::create_pager;
  use crate::core::wal::record::build_create_node_payload;
  use tempfile::NamedTempFile;

  fn create_test_pager() -> (FilePager, tempfile::NamedTempFile) {
    let temp_file = NamedTempFile::new().expect("expected value");
    let mut pager = create_pager(temp_file.path(), 4096).expect("expected value");
    // Pre-allocate some pages for WAL
    pager.allocate_pages(10).expect("expected value");
    (pager, temp_file)
  }

  #[test]
  fn test_wal_buffer_new() {
    let buffer = WalBuffer::new(4096, 1024 * 1024, 4096);
    assert!(buffer.is_empty());
    assert_eq!(buffer.capacity(), 1024 * 1024);
    assert_eq!(buffer.used(), 0);
  }

  #[test]
  fn test_wal_buffer_reserve() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);

    // Reserve some space
    let pos = buffer.reserve(100).expect("expected value");
    assert_eq!(pos, 0);
    assert!(!buffer.is_empty());

    // Reserve more
    let pos2 = buffer.reserve(100).expect("expected value");
    assert!(pos2 > pos);
  }

  #[test]
  fn test_wal_buffer_full() {
    // With dual-region, primary gets 75% = 384 bytes
    // Each 100-byte reservation aligns to 104 bytes
    // 3 reservations = 312 bytes, 4th would be 416 > 384 (but need to leave 1 byte)
    let mut buffer = WalBuffer::new(4096, 512, 4096);

    // Fill up primary region
    buffer.reserve(100).expect("expected value"); // 104 bytes
    buffer.reserve(100).expect("expected value"); // 208 bytes
    buffer.reserve(100).expect("expected value"); // 312 bytes

    // Should fail now (need 104, only ~70 left in primary)
    assert!(buffer.reserve(100).is_none());
  }

  #[test]
  fn test_wal_buffer_reset() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);
    buffer.reserve(500).expect("expected value");
    assert!(!buffer.is_empty());

    buffer.reset();
    assert!(buffer.is_empty());
    assert_eq!(buffer.head(), 0);
    assert_eq!(buffer.tail(), 0);
  }

  #[test]
  fn test_wal_buffer_write_record() {
    let (mut pager, _temp) = create_test_pager();

    // Create WAL buffer starting at page 1 (offset 4096), size 4 pages
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write a record
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      1,
      build_create_node_payload(100, Some("test_key")),
    );

    let new_head = buffer
      .write_record(&record, &mut pager)
      .expect("expected value");
    assert!(new_head > 0);
    assert!(buffer.has_pending_writes());

    // Flush to disk
    buffer.flush(&mut pager).expect("expected value");
    assert!(!buffer.has_pending_writes());
  }

  #[test]
  fn test_wal_buffer_write_and_scan() {
    let (mut pager, _temp) = create_test_pager();

    // Create WAL buffer
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write multiple records
    for i in 0..5 {
      let record = WalRecord::new(
        WalRecordType::CreateNode,
        i,
        build_create_node_payload(100 + i, None),
      );
      buffer
        .write_record(&record, &mut pager)
        .expect("expected value");
    }

    // Flush
    buffer.flush(&mut pager).expect("expected value");

    // Scan records
    let records = buffer.scan_records(&mut pager).expect("expected value");
    assert_eq!(records.len(), 5);

    for (i, record) in records.iter().enumerate() {
      assert_eq!(record.txid, i as u64);
      assert_eq!(record.record_type, WalRecordType::CreateNode);
    }
  }

  #[test]
  fn test_wal_buffer_stats() {
    let mut buffer = WalBuffer::new(4096, 1024, 4096);
    buffer.reserve(100).expect("expected value");

    let stats = buffer.stats();
    assert_eq!(stats.capacity, 1024);
    assert!(stats.used > 0);
  }

  #[test]
  fn test_wal_buffer_discard_pending() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    let record = WalRecord::new(WalRecordType::Begin, 1, Vec::new());
    buffer
      .write_record(&record, &mut pager)
      .expect("expected value");
    assert!(buffer.has_pending_writes());

    buffer.discard_pending();
    assert!(!buffer.has_pending_writes());
  }

  #[test]
  fn test_dual_region_switch() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Initially in primary region
    assert_eq!(buffer.active_region(), 0);

    // Write to primary
    let record1 = WalRecord::new(WalRecordType::Begin, 1, Vec::new());
    buffer
      .write_record(&record1, &mut pager)
      .expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    let primary_head_before = buffer.primary_head();
    assert!(primary_head_before > 0);

    // Switch to secondary
    buffer.switch_to_secondary();
    assert_eq!(buffer.active_region(), 1);

    // Write to secondary
    let record2 = WalRecord::new(WalRecordType::Begin, 2, Vec::new());
    buffer
      .write_record(&record2, &mut pager)
      .expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // Primary head should be unchanged
    assert_eq!(buffer.primary_head(), primary_head_before);
    // Secondary head should have advanced
    assert!(buffer.secondary_head() > buffer.secondary_region_start);
  }

  #[test]
  fn test_dual_region_merge() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    // Write to primary
    let record1 = WalRecord::new(
      WalRecordType::CreateNode,
      1,
      build_create_node_payload(100, Some("node1")),
    );
    buffer
      .write_record(&record1, &mut pager)
      .expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // Switch to secondary and write more
    buffer.switch_to_secondary();
    let record2 = WalRecord::new(
      WalRecordType::CreateNode,
      2,
      build_create_node_payload(101, Some("node2")),
    );
    buffer
      .write_record(&record2, &mut pager)
      .expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // Verify both regions have data
    assert!(buffer.primary_head() > 0);
    assert!(buffer.secondary_head() > buffer.secondary_region_start);

    // Merge secondary into primary (simulates checkpoint completion)
    buffer
      .merge_secondary_into_primary(&mut pager)
      .expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // After merge, should be back in primary with just the secondary records
    assert_eq!(buffer.active_region(), 0);
    assert_eq!(buffer.tail(), 0);

    // Scan should show the merged record (just the one from secondary)
    let records = buffer.scan_records(&mut pager).expect("expected value");
    assert_eq!(records.len(), 1); // Only secondary record preserved
    assert_eq!(records[0].txid, 2);
  }

  /// Header for the 4-page WAL at page 1 used by these tests.
  fn test_header() -> DbHeaderV1 {
    let mut header = DbHeaderV1::new(4096, 4);
    header.wal_start_page = 1;
    header
  }

  fn write_node_record(buffer: &mut WalBuffer, pager: &mut FilePager, txid: u64) {
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      txid,
      build_create_node_payload(txid, Some(&format!("node-{txid}"))),
    );
    buffer.write_record(&record, pager).expect("write record");
  }

  fn txids(buffer: &mut WalBuffer, pager: &mut FilePager) -> Vec<u64> {
    let records = buffer.scan_records(pager).expect("scan records");
    records.iter().map(|record| record.txid).collect()
  }

  /// Primary txids 1..=pre_cut, then a background cut, then `post_cut`
  /// records in the secondary region, all flushed.
  fn buffer_with_cut(pager: &mut FilePager, pre_cut: u64, post_cut: &[u64]) -> WalBuffer {
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    for txid in 1..=pre_cut {
      write_node_record(&mut buffer, pager, txid);
    }
    buffer.switch_to_secondary();
    for &txid in post_cut {
      write_node_record(&mut buffer, pager, txid);
    }
    buffer.flush(pager).expect("flush");
    buffer
  }

  /// A cut taken while the primary region is empty records
  /// wal_primary_head = 0 and a wal_head in the secondary region. Reading
  /// that wal_head as the primary head (the shim for headers that predate
  /// the region fields) put the head at or past the end of the primary
  /// region.
  #[test]
  fn from_header_keeps_the_empty_primary_region_of_a_cut() {
    let (mut pager, _temp) = create_test_pager();
    for post_cut in [&[][..], &[10, 11][..]] {
      let buffer = buffer_with_cut(&mut pager, 0, post_cut);
      let mut header = test_header();
      buffer.store_in_header(&mut header);
      let reopened = WalBuffer::from_header(&header);
      assert_eq!(reopened.region_state(), buffer.region_state());
      assert_eq!(reopened.primary_head(), 0);
    }

    // A header without the region fields still names its primary head.
    let mut legacy = test_header();
    legacy.wal_head = 96;
    legacy.wal_primary_head = 0;
    legacy.wal_secondary_head = 0;
    assert_eq!(WalBuffer::from_header(&legacy).primary_head(), 96);
  }

  #[test]
  fn retire_primary_region_keeps_only_post_cut_records() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = buffer_with_cut(&mut pager, 3, &[10, 11]);
    let mut cut_header = test_header();
    buffer.store_in_header(&mut cut_header);
    let post_cut_bytes = buffer.secondary_head() - buffer.primary_region_size();

    buffer.retire_primary_region();

    assert!(buffer.is_primary_retired());
    assert!(!buffer.has_pending_writes(), "retiring writes no WAL bytes");
    assert_eq!(buffer.used(), post_cut_bytes);
    assert_eq!(txids(&mut buffer, &mut pager), vec![10, 11]);
    assert!(buffer
      .scan_region(0, &mut pager)
      .expect("scan primary")
      .is_empty());

    // The persisted form reopens into the same retained state.
    let mut retained_header = test_header();
    buffer.store_in_header(&mut retained_header);
    let mut reopened = WalBuffer::from_header(&retained_header);
    assert!(reopened.is_primary_retired());
    assert_eq!(txids(&mut reopened, &mut pager), vec![10, 11]);

    // The cut header's primary records are still intact as a crash fallback.
    let mut fallback = WalBuffer::from_header(&cut_header);
    let fallback_records = fallback
      .records_for_recovery(&mut pager)
      .expect("fallback records");
    let fallback_txids: Vec<u64> = fallback_records.iter().map(|r| r.txid).collect();
    assert_eq!(fallback_txids, vec![1, 2, 3, 10, 11]);
  }

  #[test]
  fn compact_secondary_into_primary_rewinds_primary_usage() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = buffer_with_cut(&mut pager, 40, &[50, 51, 52]);
    let pre_cut_usage = buffer.primary_head() as f64 / buffer.primary_region_size() as f64;
    let post_cut_bytes = buffer.secondary_head() - buffer.primary_region_size();
    buffer.retire_primary_region();

    buffer
      .compact_secondary_into_primary(&mut pager)
      .expect("compact");

    assert_eq!(buffer.active_region(), 0);
    assert_eq!(buffer.tail(), 0);
    assert_eq!(buffer.primary_head(), post_cut_bytes);
    assert!(!buffer.has_secondary_records());
    assert!(
      !buffer.has_pending_writes(),
      "compaction flushes its rewrite"
    );
    assert!(buffer.usage_ratio() < pre_cut_usage / 4.0);
    assert_eq!(txids(&mut buffer, &mut pager), vec![50, 51, 52]);

    let mut header = test_header();
    buffer.store_in_header(&mut header);
    let mut reopened = WalBuffer::from_header(&header);
    assert!(!reopened.is_primary_retired());
    assert_eq!(reopened.usage_ratio(), buffer.usage_ratio());
    assert_eq!(txids(&mut reopened, &mut pager), vec![50, 51, 52]);
  }

  #[test]
  fn failed_compaction_keeps_retained_state() {
    let (mut pager, temp) = create_test_pager();
    let mut buffer = buffer_with_cut(&mut pager, 3, &[10, 11]);
    buffer.retire_primary_region();
    let retained = buffer.region_state();

    // Writes through a read-only pager fail after the rewrite is buffered.
    let mut read_only = crate::core::pager::open_pager_with_locking(temp.path(), 4096, true, false)
      .expect("read-only pager");
    assert!(buffer
      .compact_secondary_into_primary(&mut read_only)
      .is_err());

    assert_eq!(buffer.region_state(), retained);
    assert!(!buffer.has_pending_writes());
    assert_eq!(txids(&mut buffer, &mut pager), vec![10, 11]);

    buffer
      .compact_secondary_into_primary(&mut pager)
      .expect("retry compaction");
    assert_eq!(buffer.active_region(), 0);
    assert_eq!(txids(&mut buffer, &mut pager), vec![10, 11]);
  }

  fn write_tx_record(
    buffer: &mut WalBuffer,
    pager: &mut FilePager,
    record_type: WalRecordType,
    txid: u64,
    node_id: u64,
  ) {
    let payload = match record_type {
      WalRecordType::CreateNode => build_create_node_payload(node_id, None),
      _ => Vec::new(),
    };
    let record = WalRecord::new(record_type, txid, payload);
    buffer.write_record(&record, pager).expect("write record");
  }

  fn record_ids(records: &[ParsedWalRecord]) -> Vec<(WalRecordType, u64, Vec<u8>)> {
    records
      .iter()
      .map(|record| (record.record_type, record.txid, record.payload.clone()))
      .collect()
  }

  #[test]
  fn carry_open_transactions_copies_them_whole_into_secondary() {
    use WalRecordType::{Begin, Commit, CreateNode, Rollback};
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    // Tx 1 commits; tx 2 stays open; tx 3 is unterminated but no longer open
    // (it failed); tx 4 rolls back; tx 5 has two BEGINs, as in a WAL rebuilt
    // from both regions after an earlier carry.
    for (record_type, txid, node_id) in [
      (Begin, 1, 0),
      (CreateNode, 1, 10),
      (Begin, 2, 0),
      (CreateNode, 2, 20),
      (Commit, 1, 0),
      (Begin, 3, 0),
      (CreateNode, 3, 30),
      (Begin, 4, 0),
      (CreateNode, 4, 40),
      (Rollback, 4, 0),
      (Begin, 5, 0),
      (CreateNode, 5, 50),
      (Begin, 5, 0),
      (CreateNode, 5, 51),
      (CreateNode, 2, 21),
    ] {
      write_tx_record(&mut buffer, &mut pager, record_type, txid, node_id);
    }
    let primary_records = buffer.scan_region(0, &mut pager).expect("scan primary");
    let primary_head = buffer.primary_head();

    let open = HashSet::from([2, 4, 5]);
    let carried = buffer
      .open_transaction_records(&open, &mut pager)
      .expect("collect open records");
    buffer.switch_to_secondary();
    buffer
      .carry_into_secondary(&carried, &mut pager)
      .expect("carry");
    buffer.flush(&mut pager).expect("flush");

    assert_eq!(buffer.primary_head(), primary_head);
    let expected: Vec<_> = [2usize, 3, 12, 13, 14]
      .iter()
      .map(|&index| primary_records[index].clone())
      .collect();
    assert_eq!(
      record_ids(&buffer.scan_region(1, &mut pager).expect("scan secondary")),
      record_ids(&expected)
    );

    // The open transactions finish after the cut. Merging both regions, as
    // crash recovery does, replays each committed transaction's records once.
    write_tx_record(&mut buffer, &mut pager, CreateNode, 2, 22);
    write_tx_record(&mut buffer, &mut pager, Commit, 2, 0);
    write_tx_record(&mut buffer, &mut pager, Commit, 5, 0);
    buffer.flush(&mut pager).expect("flush");
    let merged = buffer.records_for_recovery(&mut pager).expect("merge");
    let committed: Vec<(u64, Vec<u64>)> =
      crate::core::wal::record::extract_committed_transactions_in_order(&merged)
        .into_iter()
        .map(|(txid, records)| {
          let nodes = records
            .iter()
            .map(|record| {
              crate::core::wal::record::parse_create_node_payload(&record.payload)
                .expect("create node payload")
                .node_id
            })
            .collect();
          (txid, nodes)
        })
        .collect();
    assert_eq!(
      committed,
      vec![(1, vec![10]), (2, vec![20, 21, 22]), (5, vec![51])]
    );
  }

  #[test]
  fn open_transaction_records_must_fit_in_secondary() {
    let (mut pager, _temp) = create_test_pager();
    // The secondary region is a quarter of the WAL: too small for tx 7.
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    write_tx_record(&mut buffer, &mut pager, WalRecordType::Begin, 7, 0);
    let key = "k".repeat(1500);
    for node_id in 0..3 {
      let record = WalRecord::new(
        WalRecordType::CreateNode,
        7,
        build_create_node_payload(node_id, Some(&key)),
      );
      buffer.write_record(&record, &mut pager).expect("write");
    }
    buffer.flush(&mut pager).expect("flush");
    let before = buffer.region_state();

    let result = buffer.open_transaction_records(&HashSet::from([7]), &mut pager);

    assert!(matches!(result, Err(KiteError::WalBufferFull)));
    assert_eq!(buffer.region_state(), before);
    assert!(!buffer.has_pending_writes());
    assert_eq!(txids(&mut buffer, &mut pager), vec![7, 7, 7, 7]);
  }

  /// Records in the primary region, a cut, then records in the secondary
  /// region, all flushed. `secondary` records hold `key_len`-byte keys.
  fn buffer_with_cut_records(
    pager: &mut FilePager,
    primary: &[(WalRecordType, u64)],
    secondary: &[(WalRecordType, u64)],
    key_len: usize,
  ) -> WalBuffer {
    let key = "k".repeat(key_len);
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    let write = |buffer: &mut WalBuffer, pager: &mut FilePager, (record_type, txid)| {
      let payload = match record_type {
        WalRecordType::CreateNode => build_create_node_payload(txid, Some(&key)),
        _ => Vec::new(),
      };
      let record = WalRecord::new(record_type, txid, payload);
      buffer.write_record(&record, pager).expect("write record");
    };
    for &record in primary {
      write(&mut buffer, pager, record);
    }
    buffer.switch_to_secondary();
    for &record in secondary {
      write(&mut buffer, pager, record);
    }
    buffer.flush(pager).expect("flush");
    buffer
  }

  #[test]
  fn merge_cut_into_primary_appends_secondary_records_after_primary() {
    use WalRecordType::{Begin, Commit, CreateNode};
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = buffer_with_cut_records(
      &mut pager,
      &[
        (Begin, 1),
        (CreateNode, 1),
        (Commit, 1),
        (Begin, 2),
        (CreateNode, 2),
      ],
      &[
        (Begin, 2),
        (CreateNode, 2),
        (Commit, 2),
        (Begin, 3),
        (CreateNode, 3),
      ],
      16,
    );
    let mut cut_header = test_header();
    buffer.store_in_header(&mut cut_header);
    let primary_head = buffer.primary_head();
    let primary_bytes =
      pager.read_page(1).expect("read primary page")[..primary_head as usize].to_vec();

    assert!(buffer.merge_cut_into_primary(&mut pager).expect("merge"));

    assert_eq!(buffer.active_region(), 0);
    assert!(!buffer.has_secondary_records());
    assert!(!buffer.has_pending_writes(), "the merge is flushed");
    assert_eq!(
      record_ids(&buffer.scan_records(&mut pager).expect("scan")),
      record_ids(
        &WalBuffer::from_header(&cut_header)
          .records_for_recovery(&mut pager)
          .expect("cut records")
      )
    );
    // The primary records the cut header names are untouched.
    assert!(
      pager.read_page(1).expect("read primary page")[..primary_head as usize] == primary_bytes
    );
  }

  #[test]
  fn merge_cut_into_primary_changes_nothing_when_records_do_not_fit() {
    use WalRecordType::{Begin, Commit, CreateNode};
    let (mut pager, _temp) = create_test_pager();
    // 4 KiB secondary region, 12 KiB primary region: ~11 KiB in primary plus
    // ~3 KiB in secondary do not fit in the primary region.
    let primary: Vec<_> = (1..=10)
      .flat_map(|txid| [(Begin, txid), (CreateNode, txid), (Commit, txid)])
      .collect();
    let mut buffer = buffer_with_cut_records(
      &mut pager,
      &primary,
      &[
        (Begin, 20),
        (CreateNode, 20),
        (CreateNode, 20),
        (Commit, 20),
      ],
      1000,
    );
    assert!(buffer.primary_head() > 10 * 1024);
    let cut = buffer.region_state();
    let wal_pages: Vec<_> = (1..5)
      .map(|page| pager.read_page(page).expect("read page"))
      .collect();

    assert!(!buffer.merge_cut_into_primary(&mut pager).expect("merge"));

    assert_eq!(buffer.region_state(), cut);
    assert!(!buffer.has_pending_writes());
    for (index, page) in (1..5).enumerate() {
      assert!(
        pager.read_page(page).expect("read page") == wal_pages[index],
        "page {page} changed"
      );
    }
  }

  #[test]
  fn trim_to_valid_records_drops_an_unreadable_tail() {
    use WalRecordType::{Begin, Commit, CreateNode};
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    for (record_type, txid) in [(Begin, 1), (CreateNode, 1), (Commit, 1)] {
      write_tx_record(&mut buffer, &mut pager, record_type, txid, txid);
    }
    let valid_end = buffer.primary_head();
    for (record_type, txid) in [(Begin, 2), (CreateNode, 2), (Commit, 2)] {
      write_tx_record(&mut buffer, &mut pager, record_type, txid, txid);
    }
    buffer.flush(&mut pager).expect("flush");
    let head = buffer.primary_head();
    // Tx 2's BEGIN never landed intact.
    let mut page = pager.read_page(1).expect("read page");
    page[valid_end as usize + 9] ^= 0xFF;
    pager.write_page(1, &page).expect("write page");

    assert!(buffer.trim_to_valid_records(&mut pager).expect("trim"));
    assert_eq!(
      (buffer.primary_head(), buffer.head()),
      (valid_end, valid_end)
    );
    assert!(head > valid_end);
    assert!(!buffer
      .trim_to_valid_records(&mut pager)
      .expect("trim again"));

    // The next record lands where replay reaches it.
    write_tx_record(&mut buffer, &mut pager, Begin, 3, 0);
    buffer.flush(&mut pager).expect("flush");
    assert_eq!(txids(&mut buffer, &mut pager), vec![1, 1, 1, 3]);
  }

  #[test]
  fn secondary_holds_whole_transactions_rejects_a_transaction_begun_before_the_cut() {
    use WalRecordType::{Begin, Commit, CreateNode, Rollback};
    let (mut pager, _temp) = create_test_pager();
    // Tx 1 was copied at the cut; tx 2 began after it; tx 3 rolled back.
    let mut buffer = buffer_with_cut_records(
      &mut pager,
      &[(Begin, 1), (CreateNode, 1), (Begin, 3)],
      &[
        (Begin, 1),
        (CreateNode, 1),
        (Begin, 2),
        (Commit, 1),
        (Rollback, 3),
      ],
      0,
    );
    assert!(buffer
      .secondary_holds_whole_transactions(&HashSet::from([2, 3]), &mut pager)
      .expect("check"));
    // Tx 4 is open but has no record in the secondary region.
    assert!(!buffer
      .secondary_holds_whole_transactions(&HashSet::from([4]), &mut pager)
      .expect("check"));

    // Tx 5 commits in the secondary region without its BEGIN there.
    let mut buffer = buffer_with_cut_records(
      &mut pager,
      &[(Begin, 5), (CreateNode, 5)],
      &[(CreateNode, 5), (Commit, 5)],
      0,
    );
    assert!(!buffer
      .secondary_holds_whole_transactions(&HashSet::new(), &mut pager)
      .expect("check"));
  }
}
