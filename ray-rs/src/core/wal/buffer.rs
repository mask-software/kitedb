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
//! Salts: each region's records carry its salt (header `wal_primary_salt` /
//! `wal_secondary_salt`) XORed into their CRC. A region gets a fresh salt
//! whenever it is emptied for reuse, so records an earlier cycle left in place
//! (a checkpoint rewinds the head but does not erase them) fail their CRC, and
//! replay stops there rather than applying them again after newer commits.
//! Records are built unsalted ([`WalRecord::build`], as replication frames
//! carry them) and salted as they are written here.
//!
//! I/O: records are buffered in memory as contiguous byte runs and flushed
//! with one positioned write per run, writing exactly the bytes appended (no
//! page is read back first, and bytes already on disk are never rewritten).
//! A commit group seals the runs ([`WalBuffer::seal`]) and writes them
//! without the WAL lock, so transactions keep appending meanwhile. Scans read
//! a region's live bytes with one positioned read and parse them in memory.
//!
//! Zeros ahead: a header may be written in the same sync as the records it
//! names (one sync per commit group in `SyncMode::Full`; in
//! `SyncMode::Normal` nothing is synced), so a crash can leave it on disk
//! without some of their pages. Recovery, which stops at the first record
//! that does not parse, then reads what those pages held before. Bytes of an
//! earlier WAL cycle fail its salt check, but bytes past the head written in
//! this one (a torn group's records past where recovery stopped, the copies
//! of a compaction a crash interrupted) parse, and would replay a commit
//! recovery once dropped, after newer ones. So records are only ever written
//! over durable zeros: before records reach past the zeros written and
//! synced ahead of the head, the bytes from there to a chunk past the
//! records are zeroed and synced first ([`SealedWrites::write`],
//! [`WalBuffer::flush`]). A commit group also tops the zeros up, in the
//! write its own sync makes durable, so a Full-mode group needs no sync of
//! its own for them. Writes a sync makes durable before any header names
//! them (a compaction's or merge's) need no zeros, and neither does a new
//! database, whose WAL is created as zeros its creating sync made durable.

use std::collections::{HashMap, HashSet};

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::*;

use super::record::{
  apply_wal_salt, build_rollback_payload, parse_wal_record_with_salt, read_wal_record_with_salt,
  salt_wal_record, wal_frames, wal_records_end, ParsedWalRecord, WalFrame, WalRecord, WalRecordAt,
};

/// WAL region split ratio: primary gets 75%, secondary gets 25%
const PRIMARY_REGION_RATIO: f64 = 0.75;

/// The fewest bytes zeros ahead reach past the records (see
/// `WalBuffer::zero_ahead`).
const MIN_ZERO_AHEAD: u64 = 64 * 1024;

/// Where the secondary region of a WAL of `capacity` bytes starts (the
/// primary region's size).
fn secondary_region_start(capacity: u64) -> u64 {
  (capacity as f64 * PRIMARY_REGION_RATIO) as u64
}

/// The salt of the records at `offset` (relative to the WAL start) of the WAL
/// `header` describes: the salt of the region holding that offset.
pub(crate) fn header_salt_at(header: &DbHeaderV1, offset: u64) -> u32 {
  let capacity = header.wal_page_count * header.page_size as u64;
  if offset >= secondary_region_start(capacity) {
    header.wal_secondary_salt
  } else {
    header.wal_primary_salt
  }
}

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
  /// Page size (for [`WalBufferStats::pending_pages`])
  page_size: usize,
  /// Writes not yet flushed to the pager
  pending: PendingWrites,

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
  /// Salt of the primary region's records (0: unsalted, a v1 WAL)
  primary_salt: u32,
  /// Salt of the secondary region's records (0: unsalted, or never used)
  secondary_salt: u32,
  /// A failed commit group's COMMIT records were rewritten as ROLLBACK
  /// records (see [`Self::restore_sealed`]), and the rewrite is not durable
  /// yet: the next flush syncs.
  unsynced_rollbacks: bool,
  /// Where the durable zeros ahead of the active region's records end
  /// (relative to the WAL start): every byte from the end of the records
  /// written to the file to here is a zero that a sync made durable (see
  /// "Zeros ahead" in the module docs). At the head when nothing is known.
  zeroed_end: u64,
  /// The head when `zeroed_end` last fell back to it: zeros ahead reach
  /// about as far past the records as records were written since (see
  /// [`Self::zero_ahead`]).
  zero_base: u64,
}

/// The buffered writes [`WalBuffer::seal`] took for a commit group to write
/// to the file without the WAL lock, and the WAL positions that group's
/// header names.
#[derive(Debug)]
pub struct SealedWrites {
  runs: Vec<PendingRun>,
  state: WalRegionState,
  /// Rewritten records of a failed group are among the runs, or were
  /// flushed and not synced: the group syncs before its header.
  sync_first: bool,
  /// Bytes to zero and sync before the runs, which reach past the durable
  /// zeros (absolute file offsets).
  zero_first: Option<ZeroRange>,
  /// Bytes to zero after the runs, which the group's sync makes durable.
  zero_after: Option<ZeroRange>,
}

/// File bytes to zero: `offset..end`, and `end` relative to the WAL start.
#[derive(Debug, Clone, Copy)]
struct ZeroRange {
  offset: u64,
  end: u64,
  wal_end: u64,
}

impl SealedWrites {
  /// Write the sealed runs to the pager, in file order, one positioned write
  /// per run: first, if they reach past the durable zeros ahead of the head,
  /// zero the bytes from there and sync; after them, any zeros the seal
  /// tops up. Callers hold the pager lock from [`WalBuffer::seal`] on, so no
  /// one reads or flushes the WAL meanwhile, and note the zeros with
  /// [`WalBuffer::note_sealed_written`].
  pub fn write(&self, pager: &mut FilePager) -> Result<()> {
    if let Some(zeros) = self.zero_first {
      write_zeros(pager, zeros)?;
      pager.sync_data()?;
    }
    for run in &self.runs {
      pager.write_range(run.offset, &run.data)?;
    }
    if let Some(zeros) = self.zero_after {
      write_zeros(pager, zeros)?;
    }
    Ok(())
  }

  /// The WAL positions as of the seal, for the header naming the records.
  pub fn state(&self) -> &WalRegionState {
    &self.state
  }

  /// Whether a sync must make these writes durable before a header names
  /// them, whatever the sync mode (see [`WalBuffer::needs_sync`]).
  pub fn needs_sync(&self) -> bool {
    self.sync_first
  }
}

impl WalBuffer {
  /// Create a new WAL buffer
  pub fn new(base_offset: u64, capacity: u64, page_size: usize) -> Self {
    let primary_region_size = secondary_region_start(capacity);
    let secondary_region_start = primary_region_size;
    let secondary_region_size = capacity - primary_region_size;

    Self {
      base_offset,
      capacity,
      head: 0,
      tail: 0,
      page_size,
      pending: PendingWrites::default(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region: 0,
      primary_head: 0,
      secondary_head: secondary_region_start,
      primary_salt: INITIAL_WAL_SALT,
      secondary_salt: 0,
      unsynced_rollbacks: false,
      zeroed_end: 0,
      zero_base: 0,
    }
  }

  /// Create from existing header state.
  ///
  /// Fails with `InvalidWal` if the header's WAL positions are outside their
  /// regions (an active region other than 0 or 1, a tail past the primary
  /// head, a secondary head before the secondary region, or a head past the
  /// WAL). The header checksum keeps a torn write from getting here, so only
  /// a writer bug or a crafted file does; accepted, such positions underflow
  /// later, which panics in debug builds and leaves the WAL looking full in
  /// release builds.
  pub fn from_header(header: &DbHeaderV1) -> Result<Self> {
    let base_offset = header.wal_start_page * header.page_size as u64;
    let capacity = header.wal_page_count * header.page_size as u64;

    let primary_region_size = secondary_region_start(capacity);
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

    let invalid = |what: String| Err(KiteError::InvalidWal(format!("header {what}")));
    let tail = header.wal_tail;
    if active_region > 1 {
      return invalid(format!("names WAL region {active_region} active"));
    }
    if header.wal_head > capacity || primary_head > capacity || secondary_head > capacity {
      return invalid(format!(
        "names WAL heads (head {}, primary {primary_head}, secondary {secondary_head}) past \
         the {capacity}-byte WAL",
        header.wal_head
      ));
    }
    if tail > primary_head || tail > header.wal_head {
      return invalid(format!(
        "names WAL tail {tail} past its head (head {}, primary {primary_head})",
        header.wal_head
      ));
    }
    if secondary_head < secondary_region_start {
      return invalid(format!(
        "names secondary WAL head {secondary_head} before the region's start \
         {secondary_region_start}"
      ));
    }
    // A cut or retired primary region never passes its end. (Headers from
    // before the region fields used the whole WAL as one region, so a primary
    // head past it is only refused once there is a secondary region.)
    if (active_region == 1 || header.checkpoint_in_progress != 0)
      && primary_head > primary_region_size
    {
      return invalid(format!(
        "names primary WAL head {primary_head} past the region's end {primary_region_size}"
      ));
    }

    Ok(Self {
      base_offset,
      capacity,
      head: header.wal_head,
      tail: header.wal_tail,
      page_size: header.page_size as usize,
      pending: PendingWrites::default(),
      primary_region_size,
      secondary_region_start,
      secondary_region_size,
      active_region,
      primary_head,
      secondary_head,
      primary_salt: header.wal_primary_salt,
      secondary_salt: header.wal_secondary_salt,
      unsynced_rollbacks: false,
      // Past the head lies whatever a crash left there.
      zeroed_end: header.wal_head,
      zero_base: header.wal_head,
    })
  }

  /// The WAL was just created, empty: every byte of it is a zero the
  /// creating sync made durable (see "Zeros ahead" in the module docs).
  pub fn note_created_zeroed(&mut self) {
    self.zeroed_end = self.active_region_end();
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
    // Writing starts over at the region's start: earlier cuts' records there
    // must not parse as this one's.
    if !self.has_secondary_records() {
      self.secondary_salt = self.fresh_salt();
    }
    self.active_region = 1;
    // Update head to track active position
    self.head = self.secondary_head;
    self.forget_zeros();
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

    let (_, bytes) = self.region_bytes(0, 0, pager)?;
    let records: Vec<(WalRecordType, WalFrame)> = wal_frames(&bytes, self.primary_salt).collect();
    let unseen = transactions_without_boundaries(&records, open);
    if !unseen.is_empty() {
      return Err(KiteError::InvalidWal(format!(
        "no BEGIN record found for open transactions {unseen:?}"
      )));
    }
    let mut last_begin: HashMap<TxId, usize> = HashMap::new();
    for (index, (record_type, frame)) in records.iter().enumerate() {
      if !open.contains(&frame.txid) {
        continue;
      }
      match record_type {
        WalRecordType::Begin => {
          last_begin.insert(frame.txid, index);
        }
        WalRecordType::Commit | WalRecordType::Rollback => {
          last_begin.remove(&frame.txid);
        }
        _ => {}
      }
    }
    for (index, (_, frame)) in records.iter().enumerate() {
      if last_begin
        .get(&frame.txid)
        .is_some_and(|begin| index >= *begin)
      {
        carried.extend_from_slice(&bytes[frame.start..frame.end]);
      }
    }

    if carried.len() as u64 > self.secondary_region_size {
      return Err(KiteError::WalBufferFull);
    }
    // The copies are returned unsalted, as built.
    xor_salt(&mut carried, self.primary_salt)?;
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
    if let Err(error) = self.write_record_bytes_batch(records) {
      self.pending.clear();
      self.restore_region_state(prior);
      return Err(error);
    }
    Ok(())
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
    let switches = self.active_region != 1;
    self.primary_head = self.primary_region_size;
    self.tail = self.secondary_region_start;
    self.active_region = 1;
    self.head = self.secondary_head;
    if switches {
      self.forget_zeros();
    }
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
    self.compact_secondary_into_primary_reusing(Vec::new(), pager)
  }

  /// [`Self::compact_secondary_into_primary`], reusing `read`: the secondary
  /// region's first records exactly as they lie there now, as
  /// [`RegionBytes::parse`] returned them during the cut whose
  /// records these are (the region only grows while a cut lasts, and keeps
  /// its records when the install retires the primary region). Only the
  /// records after them are read and checked here, so a caller holding a
  /// lock does that work before taking it.
  pub fn compact_secondary_into_primary_reusing(
    &mut self,
    read: Vec<u8>,
    pager: &mut FilePager,
  ) -> Result<()> {
    // Flush earlier writes first so an error below drops only the bytes this
    // rewrite buffered.
    self.flush(pager)?;
    let retained = self.region_state();
    // Synced before any header names them: no zeros ahead needed.
    let result = self
      .merge_secondary_into_primary(read, pager)
      .and_then(|()| self.write_pending(pager))
      .and_then(|()| pager.sync_data());
    if result.is_err() {
      self.pending.clear();
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
      primary_salt: self.primary_salt,
      secondary_salt: self.secondary_salt,
    }
  }

  /// Restore positions (and salts) captured by [`Self::region_state`].
  /// Pending writes are not touched.
  pub fn restore_region_state(&mut self, state: WalRegionState) {
    self.head = state.head;
    self.tail = state.tail;
    self.primary_head = state.primary_head;
    self.secondary_head = state.secondary_head;
    self.active_region = state.active_region;
    self.primary_salt = state.primary_salt;
    self.secondary_salt = state.secondary_salt;
    // Bytes written past the head since the state was saved may lie there.
    self.forget_zeros();
  }

  /// Record this buffer's positions, active region, and salts in `header`.
  pub fn store_in_header(&self, header: &mut DbHeaderV1) {
    self.region_state().store_in_header(header);
  }

  /// A salt neither region uses now, for a region about to be reused: one
  /// past the newest, so a salt recurs only after 2^32 resets. Never 0, which
  /// marks an unsalted (v1) region.
  fn fresh_salt(&self) -> u32 {
    let mut salt = self.primary_salt.max(self.secondary_salt);
    loop {
      salt = salt.wrapping_add(1);
      if salt != 0 && salt != self.primary_salt && salt != self.secondary_salt {
        return salt;
      }
    }
  }

  /// Salt of `region`'s records (0: primary, 1: secondary).
  fn region_salt(&self, region: u8) -> u32 {
    if region == 0 {
      self.primary_salt
    } else {
      self.secondary_salt
    }
  }

  /// Salt `records`, whole unsalted records, for `region`.
  fn salt_for(&self, region: u8, records: &mut [u8]) -> Result<()> {
    xor_salt(records, self.region_salt(region))
  }

  /// The secondary region's records that parse, as they lie there (salted
  /// with its salt): `read`, its first records as they lie there now (see
  /// [`Self::compact_secondary_into_primary_reusing`]), then those after
  /// them, read with one positioned read.
  fn secondary_record_bytes(&self, mut read: Vec<u8>, pager: &mut FilePager) -> Result<Vec<u8>> {
    let from = self.secondary_region_start + read.len() as u64;
    if from > self.secondary_head {
      return Err(KiteError::Internal(format!(
        "{} bytes of secondary WAL records read, but the region holds {}",
        read.len(),
        self.secondary_head - self.secondary_region_start
      )));
    }
    let (start, bytes) = self.region_bytes(1, from, pager)?;
    let end = wal_records_end(&bytes, self.secondary_salt);
    warn_dropped_tail("secondary", start + end as u64, self.secondary_head);
    read.extend_from_slice(&bytes[..end]);
    Ok(read)
  }

  /// Merge secondary records into a fresh primary region (buffered, not
  /// flushed). Only [`Self::compact_secondary_into_primary`] may call it: it
  /// adds the flush, sync, and error rollback that make the rewrite safe to
  /// install.
  ///
  /// The records are copied as they are, re-salted for the primary region
  /// (each CRC XORed with both salts), not decoded and rebuilt.
  fn merge_secondary_into_primary(&mut self, read: Vec<u8>, pager: &mut FilePager) -> Result<()> {
    let mut records = self.secondary_record_bytes(read, pager)?;
    if records.len() as u64 > self.primary_region_size {
      return Err(KiteError::WalBufferFull);
    }
    let secondary_salt = self.secondary_salt;

    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.tail = 0;
    self.active_region = 0;
    self.head = 0;
    // The primary region is rewritten from its start.
    self.primary_salt = self.fresh_salt();

    xor_salt(&mut records, secondary_salt ^ self.primary_salt)?;
    self.primary_head = records.len() as u64;
    self.head = self.primary_head;
    self.forget_zeros();
    self.pending.write_vec(self.file_offset(0), records);
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
    if self.region_end(0, pager)? != self.primary_head {
      return Ok(false);
    }
    let mut merged = self.secondary_record_bytes(Vec::new(), pager)?;
    if self.primary_head + merged.len() as u64 > self.primary_region_size {
      return Ok(false);
    }
    // They join the primary region's records, so they take its salt.
    xor_salt(&mut merged, self.secondary_salt ^ self.primary_salt)?;

    let cut = self.region_state();
    let merged_len = merged.len() as u64;
    if !merged.is_empty() {
      self
        .pending
        .write_vec(self.file_offset(self.primary_head), merged);
      // Synced before any header names them: no zeros ahead needed.
      let written = self.write_pending(pager).and_then(|()| pager.sync_data());
      if let Err(error) = written {
        self.pending.clear();
        self.restore_region_state(cut);
        return Err(error);
      }
    }
    self.primary_head += merged_len;
    self.secondary_head = self.secondary_region_start;
    self.active_region = 0;
    self.head = self.primary_head;
    self.forget_zeros();
    Ok(true)
  }

  /// Bytes the records of both regions take: what
  /// [`Self::merge_cut_into_primary`] would leave in the primary region.
  #[cfg(test)]
  pub fn merged_cut_size(&mut self, pager: &mut FilePager) -> Result<u64> {
    let primary_end = self.region_end(0, pager)?;
    let secondary_end = self.region_end(1, pager)?;
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
    let primary_end = if self.is_primary_retired() {
      self.primary_head
    } else {
      self.region_end(0, pager)?
    };
    let secondary_end = if self.active_region == 1 {
      self.region_end(1, pager)?
    } else {
      self.secondary_head
    };
    Ok(self.trim_to(primary_end, secondary_end))
  }

  /// Prepare the WAL a writable open found for appends, reading and checking
  /// each region's records once for both steps:
  ///
  /// 1. Fail with `InvalidWal` if a region's records stop before its head at
  ///    a record whose CRC checks but whose type this version does not know
  ///    (a newer version wrote it). It is not torn, so trimming or compacting
  ///    the region, which keeps only the records before it, would drop it and
  ///    every record after it for good. Replay stops at such a record either
  ///    way.
  /// 2. Unless the primary region is retired (compacting it keeps only
  ///    readable records), trim the regions in use as
  ///    [`Self::trim_to_valid_records`] does, reporting each dropped tail.
  ///
  /// Returns whether a head moved; the caller then persists a header naming
  /// the trimmed WAL before writing more records.
  pub fn check_and_trim(&mut self, pager: &mut FilePager) -> Result<bool> {
    let mut ends = [0; 2];
    for region in [0, 1] {
      let (start, bytes) = self.region_bytes(region, 0, pager)?;
      let salt = self.region_salt(region);
      let valid = wal_records_end(&bytes, salt);
      let end = start + valid as u64;
      if end < self.region_head(region) {
        if let WalRecordAt::UnknownType(record_type) =
          read_wal_record_with_salt(&bytes, valid, salt)
        {
          return Err(KiteError::InvalidWal(format!(
            "WAL record of unknown type {record_type} at offset {end}, probably written by a \
             newer version; open the database read-only, or with that version"
          )));
        }
      }
      ends[usize::from(region)] = end;
    }
    if self.is_primary_retired() {
      return Ok(false);
    }
    Ok(self.trim_to(ends[0], ends[1]))
  }

  /// Trim the regions in use (see [`Self::trim_to_valid_records`]) to where
  /// their readable records end; returns whether a head moved.
  fn trim_to(&mut self, primary_end: u64, secondary_end: u64) -> bool {
    let mut trimmed = false;
    if !self.is_primary_retired() {
      warn_dropped_tail("primary", primary_end, self.primary_head);
      trimmed |= primary_end != self.primary_head;
      self.primary_head = primary_end;
    }
    if self.active_region == 1 {
      warn_dropped_tail("secondary", secondary_end, self.secondary_head);
      trimmed |= secondary_end != self.secondary_head;
      self.secondary_head = secondary_end;
    }
    self.head = self.region_head(self.active_region);
    if trimmed {
      // Past the new head lie the bytes trimmed away.
      self.forget_zeros();
    }
    trimmed
  }

  /// The head of `region` (0: primary, 1: secondary).
  fn region_head(&self, region: u8) -> u64 {
    if region == 0 {
      self.primary_head
    } else {
      self.secondary_head
    }
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
    let (_, bytes) = self.region_bytes(1, 0, pager)?;
    for (record_type, frame) in wal_frames(&bytes, self.secondary_salt) {
      match record_type {
        WalRecordType::Begin => {
          begun.insert(frame.txid);
        }
        WalRecordType::Commit if !begun.contains(&frame.txid) => return Ok(false),
        WalRecordType::Rollback => {
          rolled_back.insert(frame.txid);
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
    self.scan_region_from(region, 0, pager)
  }

  /// `scan_region_to_end`, starting at offset `from` (relative to the WAL
  /// start, a record boundary such as an end this returned; at the region's
  /// start if before it), to scan on after records already read.
  pub fn scan_region_from(
    &mut self,
    region: u8,
    from: u64,
    pager: &mut FilePager,
  ) -> Result<(Vec<ParsedWalRecord>, u64)> {
    let (records, _, end) = self.read_region_from(region, from, pager)?.parse();
    Ok((records, end))
  }

  /// The bytes [`Self::scan_region_from`] parses, read with one positioned
  /// read but not parsed, so a caller can parse them
  /// ([`RegionBytes::parse`]) after releasing the locks writers need.
  pub fn read_region_from(
    &self,
    region: u8,
    from: u64,
    pager: &mut FilePager,
  ) -> Result<RegionBytes> {
    let (start, bytes) = self.region_bytes(region, from, pager)?;
    Ok(RegionBytes {
      start,
      bytes,
      salt: self.region_salt(region),
    })
  }

  /// Where the records that parse in `region` end (relative to the WAL
  /// start); none are copied out.
  fn region_end(&self, region: u8, pager: &mut FilePager) -> Result<u64> {
    let (start, bytes) = self.region_bytes(region, 0, pager)?;
    Ok(start + wal_records_end(&bytes, self.region_salt(region)) as u64)
  }

  /// The bytes of `region` up to its head, from offset `from` (relative to
  /// the WAL start; the region's start if before it), read with one
  /// positioned read; and where they start.
  fn region_bytes(&self, region: u8, from: u64, pager: &mut FilePager) -> Result<(u64, Vec<u8>)> {
    let (start, end) = if region == 0 {
      (self.tail, self.primary_head)
    } else {
      (self.secondary_region_start, self.secondary_head)
    };
    let start = start.max(from);
    if start >= end {
      return Ok((end, Vec::new()));
    }
    let bytes = self.read_at_offset(self.file_offset(start), (end - start) as usize, pager)?;
    Ok((start, bytes))
  }

  /// Calculate the file offset for a buffer-relative position
  pub fn file_offset(&self, buffer_pos: u64) -> u64 {
    self.base_offset + buffer_pos
  }

  /// Reserve space for a record, returning the write position
  /// Returns None if buffer is full
  ///
  /// Test only: it moves the head past bytes it never writes, which replay
  /// would then read as records.
  #[cfg(test)]
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
  pub fn write_record(&mut self, record: &WalRecord) -> Result<u64> {
    self.write_built_record(&mut record.build())
  }

  /// Write one record as [`WalRecord::build`] encoded it (unsalted), salted
  /// for the active region, and return the new head. `record` is salted in
  /// place and unsalted again before this returns, so a caller can keep the
  /// bytes it built (a replication frame carries them unsalted) without
  /// encoding the record twice. Buffered, like [`Self::write_record`]; the
  /// pager is not touched until [`Self::flush`].
  pub fn write_built_record(&mut self, record: &mut [u8]) -> Result<u64> {
    let salt = self.region_salt(self.active_region);
    if !salt_wal_record(record, salt) {
      return Err(KiteError::Internal(
        "WAL record bytes are not one whole record".to_string(),
      ));
    }
    let written = self.write_record_bytes(record);
    salt_wal_record(record, salt);
    written
  }

  /// Write prebuilt record bytes in a single batch
  /// The buffer must contain a sequence of padded, unsalted records (as
  /// [`WalRecord::build`] writes them); they are salted for the active region.
  pub fn write_record_bytes_batch(&mut self, record_bytes: &[u8]) -> Result<u64> {
    self.write_owned_record_bytes(&mut record_bytes.to_vec())
  }

  /// [`Self::write_record_bytes_batch`] for bytes the caller gives up: once
  /// they fit, they are salted in place and taken (left empty), buffered
  /// without another copy where they start a run. If they do not fit
  /// (`WalBufferFull`) they are left as they are, and nothing is written.
  pub fn write_owned_record_bytes(&mut self, records: &mut Vec<u8>) -> Result<u64> {
    if records.is_empty() {
      return Ok(self.head);
    }

    if !records.len().is_multiple_of(WAL_RECORD_ALIGNMENT) {
      return Err(KiteError::Internal(
        "WAL batch bytes must be alignment-sized".to_string(),
      ));
    }

    if !self.can_fit(records.len()) {
      return Err(KiteError::WalBufferFull);
    }
    let length = records.len() as u64;
    let (head, region_end) = if self.active_region == 0 {
      (self.primary_head, self.primary_region_size)
    } else {
      (
        self.secondary_head,
        self.secondary_region_start + self.secondary_region_size,
      )
    };
    if head + length > region_end {
      return Err(KiteError::WalBufferFull);
    }
    self.salt_for(self.active_region, records)?;
    self
      .pending
      .write_vec(self.file_offset(head), std::mem::take(records));
    if self.active_region == 0 {
      self.primary_head += length;
      self.head = self.primary_head;
    } else {
      self.secondary_head += length;
      self.head = self.secondary_head;
    }
    Ok(self.head)
  }

  /// Write raw record bytes to the active region
  fn write_record_bytes(&mut self, record_bytes: &[u8]) -> Result<u64> {
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
      self.buffer_write(file_offset, record_bytes);

      // Update head
      self.primary_head += aligned_size as u64;
      self.head = self.primary_head;
    } else {
      // Secondary region (no wrap-around)
      let file_offset = self.file_offset(self.secondary_head);

      // Buffer the write
      self.buffer_write(file_offset, record_bytes);

      // Update head
      self.secondary_head += aligned_size as u64;
      self.head = self.secondary_head;
    }

    Ok(self.head)
  }

  /// Buffer `data` for file `offset` until the next [`Self::flush`].
  fn buffer_write(&mut self, offset: u64, data: &[u8]) {
    self.pending.write(offset, data);
  }

  /// Read `length` bytes from file `offset`, buffered writes included: one
  /// positioned read, none if buffered writes cover the range.
  fn read_at_offset(&self, offset: u64, length: usize, pager: &mut FilePager) -> Result<Vec<u8>> {
    let mut bytes = if self.pending.covers(offset, length) {
      vec![0; length]
    } else {
      pager.read_range(offset, length)?
    };
    self.pending.overlay(offset, &mut bytes);
    Ok(bytes)
  }

  /// Write every buffered byte run to the pager, in file order, one
  /// positioned write per run, then sync if a failed commit group's records
  /// were rewritten and not synced yet (see [`Self::restore_sealed`]): every
  /// header written after a flush may name bytes past them. If the runs
  /// reach past the durable zeros ahead of the head, the bytes from there to
  /// a chunk past them are zeroed and synced first (see "Zeros ahead" in the
  /// module docs). On error every run stays buffered.
  pub fn flush(&mut self, pager: &mut FilePager) -> Result<()> {
    if let Some(zeros) = self.zeros_first() {
      write_zeros(pager, zeros)?;
      pager.sync_data()?;
      self.note_zeroed(zeros);
    }
    self.write_pending(pager)?;
    if self.unsynced_rollbacks {
      pager.sync_data()?;
      self.unsynced_rollbacks = false;
    }
    Ok(())
  }

  /// Write every buffered byte run to the pager, without zeros ahead: for
  /// bytes synced before any header names them. On error every run stays
  /// buffered.
  fn write_pending(&mut self, pager: &mut FilePager) -> Result<()> {
    for run in &self.pending.runs {
      pager.write_range(run.offset, &run.data)?;
    }
    self.pending.clear();
    Ok(())
  }

  /// Where the active region ends (relative to the WAL start).
  fn active_region_end(&self) -> u64 {
    if self.active_region == 0 {
      self.primary_region_size
    } else {
      self.capacity
    }
  }

  /// Nothing is known past the head any more: zeros ahead start over there.
  fn forget_zeros(&mut self) {
    self.zeroed_end = self.head;
    self.zero_base = self.head;
  }

  /// How far past the buffered runs, which end at `records_end`, the zeros
  /// ahead reach: as far as records were written since they started over
  /// (`zero_base`), so a database written to a little pays little and one
  /// written to a lot syncs for zeros rarely; at least 64 KiB, and twice the
  /// runs' bytes, so writes like these keep landing on zeros a sync already
  /// made durable. At most a quarter of the region (8 MiB) for zeros written
  /// and synced before the runs, since each such write costs a sync of its
  /// own; an eighth (1 MiB) for zeros a group tops up in its write, which
  /// its sync makes durable, so no one group's write grows much.
  fn zero_ahead(&self, records_end: u64, top_up: bool) -> u64 {
    let region = if self.active_region == 0 {
      self.primary_region_size
    } else {
      self.secondary_region_size
    };
    let most = if top_up {
      (region / 8).clamp(MIN_ZERO_AHEAD, 1024 * 1024)
    } else {
      (region / 4).clamp(MIN_ZERO_AHEAD, 8 * 1024 * 1024)
    };
    let pending: u64 = self
      .pending
      .runs
      .iter()
      .map(|run| run.data.len() as u64)
      .sum();
    let written = records_end.saturating_sub(self.zero_base);
    written.clamp(MIN_ZERO_AHEAD, most).max(2 * pending)
  }

  /// Where the buffered runs end (relative to the WAL start), if any.
  fn pending_end(&self) -> Option<u64> {
    self
      .pending
      .runs
      .iter()
      .map(|run| run.end() - self.base_offset)
      .max()
  }

  /// The zeros ahead from `zeroed_end` to [`Self::zero_ahead`] past
  /// `records_end`, page aligned and never past the active region.
  fn zeros_to(&self, records_end: u64, top_up: bool) -> Option<ZeroRange> {
    let page = self.page_size as u64;
    let end = (records_end + self.zero_ahead(records_end, top_up))
      .div_ceil(page)
      .saturating_mul(page)
      .min(self.active_region_end());
    (end > self.zeroed_end).then(|| ZeroRange {
      offset: self.file_offset(self.zeroed_end),
      end: self.file_offset(end),
      wal_end: end,
    })
  }

  /// The zeros to write and sync before the buffered runs, if they reach
  /// past the durable zeros (see [`Self::zeros_to`]).
  fn zeros_first(&self) -> Option<ZeroRange> {
    let records_end = self.pending_end()?;
    if records_end <= self.zeroed_end {
      return None;
    }
    self.zeros_to(records_end, false)
  }

  /// The zeros a commit group tops up after its runs, in the write its sync
  /// makes durable, once fewer than half of [`Self::zero_ahead`] of durable
  /// zeros would be left past them.
  fn zeros_after(&self) -> Option<ZeroRange> {
    let records_end = self.pending_end()?;
    if records_end > self.zeroed_end
      || self.zeroed_end >= records_end + self.zero_ahead(records_end, true) / 2
    {
      return None;
    }
    self.zeros_to(records_end, true)
  }

  /// Flush and sync to disk.
  ///
  /// The WAL lies inside the file, so a data sync ([`FilePager::sync_data`])
  /// makes it durable.
  pub fn sync(&mut self, pager: &mut FilePager) -> Result<()> {
    self.flush(pager)?;
    pager.sync_data()?;
    self.unsynced_rollbacks = false;
    Ok(())
  }

  /// Take every buffered write, for a commit group to write to the file
  /// without the WAL lock ([`SealedWrites::write`]), with the positions the
  /// group's header names. Records appended meanwhile are buffered after the
  /// sealed ones, and no header names them before a later flush or seal.
  ///
  /// The caller holds the pager lock until the sealed writes are written
  /// (or given back with [`Self::restore_sealed`]): reads and flushes of the
  /// WAL take it, and must not miss the sealed bytes.
  ///
  /// The seal also decides the zeros ahead [`SealedWrites::write`] writes:
  /// before the runs, if they reach past the durable zeros; after them, if
  /// `top_up` (the group syncs once its writes are done) and few are left.
  pub fn seal(&mut self, top_up: bool) -> SealedWrites {
    let zero_first = self.zeros_first();
    let zero_after = if top_up && zero_first.is_none() {
      self.zeros_after()
    } else {
      None
    };
    SealedWrites {
      runs: std::mem::take(&mut self.pending.runs),
      state: self.region_state(),
      sync_first: self.unsynced_rollbacks,
      zero_first,
      zero_after,
    }
  }

  /// The sealed writes are written ([`SealedWrites::write`] returned): the
  /// zeros it synced before them are durable.
  pub fn note_sealed_written(&mut self, sealed: &SealedWrites) {
    if let Some(zeros) = sealed.zero_first {
      self.note_zeroed(zeros);
    }
  }

  /// Zeros written before records are durable.
  fn note_zeroed(&mut self, zeros: ZeroRange) {
    self.zeroed_end = self.zeroed_end.max(zeros.wal_end);
  }

  /// The sealed writes are durable (written and synced): a rewrite of a
  /// failed group among them needs no further sync, and the zeros topped up
  /// after them are durable.
  pub fn note_sealed_synced(&mut self, sealed: &SealedWrites) {
    self.note_sealed_written(sealed);
    if let Some(zeros) = sealed.zero_after {
      self.note_zeroed(zeros);
    }
    if sealed.sync_first {
      self.unsynced_rollbacks = false;
    }
  }

  /// Give back the writes of a commit group whose write or header failed,
  /// and make its commits fail for good: each COMMIT record at the WAL
  /// positions `commits` (with its transaction id) becomes a ROLLBACK record
  /// of the same size, so replay discards the transaction.
  ///
  /// The head is not rewound: other transactions may have appended records
  /// after the group's since the seal. The sealed bytes go back into the
  /// buffer (a failed write may have left them torn on disk), before the
  /// records appended since. Until a sync makes the rewrite durable,
  /// [`Self::needs_sync`] holds and every flush syncs: an OS crash could
  /// otherwise keep an early write-back of a COMMIT record, lose its
  /// rewrite, and keep a later header that names it, and recovery would
  /// replay a commit that returned an error.
  pub fn restore_sealed(
    &mut self,
    sealed: SealedWrites,
    commits: impl IntoIterator<Item = (u64, TxId)>,
  ) -> Result<()> {
    let appended = std::mem::replace(&mut self.pending.runs, sealed.runs);
    for run in appended {
      self.pending.write_vec(run.offset, run.data);
    }
    self.unsynced_rollbacks = true;
    for (position, txid) in commits {
      let region = u8::from(position >= self.secondary_region_start);
      let mut rollback =
        WalRecord::new(WalRecordType::Rollback, txid, build_rollback_payload()).build();
      if !salt_wal_record(&mut rollback, self.region_salt(region)) {
        return Err(KiteError::Internal(
          "a ROLLBACK record is not one whole record".to_string(),
        ));
      }
      self.buffer_write(self.file_offset(position), &rollback);
    }
    Ok(())
  }

  /// Whether a failed commit group's rewritten records may not be durable
  /// yet: a header must not name bytes past them before a sync, which the
  /// next flush does.
  pub fn needs_sync(&self) -> bool {
    self.unsynced_rollbacks
  }

  /// Check if there are pending writes
  pub fn has_pending_writes(&self) -> bool {
    !self.pending.is_empty()
  }

  /// Reset the buffer (after checkpoint). The primary region gets a fresh
  /// salt: the records left in place must not replay as the new cycle's.
  pub fn reset(&mut self) {
    self.head = 0;
    self.tail = 0;
    self.pending.clear();
    // Also reset dual-region state
    self.primary_head = 0;
    self.secondary_head = self.secondary_region_start;
    self.active_region = 0;
    self.primary_salt = self.fresh_salt();
    self.forget_zeros();
  }

  /// Clear pending writes without flushing
  pub fn discard_pending(&mut self) {
    self.pending.clear();
  }

  /// Scan all valid records from tail to head, read with one positioned
  /// read, each checked with the salt of the region it lies in.
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

    // A record never crosses the head: bytes past it end the scan.
    let bytes = self.read_at_offset(
      self.file_offset(self.tail),
      (self.head - self.tail) as usize,
      pager,
    )?;
    let mut offset = 0;
    while offset < bytes.len() {
      let at = self.tail + offset as u64;
      let salt = self.region_salt(u8::from(at >= self.secondary_region_start));
      match parse_wal_record_with_salt(&bytes, offset, salt) {
        Some(record) => {
          offset = record.record_end;
          records.push(record);
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
      pending_pages: self.pending.pages(self.page_size as u64),
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

/// Report bytes a rewrite of a WAL region drops for good: those from `end`,
/// where the last record that parses ends, to the `head` a header named. A
/// crash during a write leaves such a torn tail; anything else there is
/// corruption.
fn warn_dropped_tail(region: &str, end: u64, head: u64) {
  if end < head {
    eprintln!(
      "Warning: dropping {} bytes of the {region} WAL region after its last valid record \
       (offset {end}, head {head}): a write torn by a crash, or corruption",
      head - end
    );
  }
}

/// Transactions in `txids` with no BEGIN, COMMIT, or ROLLBACK among `records`.
fn transactions_without_boundaries(
  records: &[(WalRecordType, WalFrame)],
  txids: &HashSet<TxId>,
) -> HashSet<TxId> {
  let mut unseen = txids.clone();
  for (record_type, frame) in records {
    if matches!(
      record_type,
      WalRecordType::Begin | WalRecordType::Commit | WalRecordType::Rollback
    ) {
      unseen.remove(&frame.txid);
    }
  }
  unseen
}

/// XOR `salt` into the CRC of each of `records`, whole records back to back
/// (see [`apply_wal_salt`]): salts unsalted records, unsalts salted ones,
/// and with two salts XORed together moves records from one to the other.
fn xor_salt(records: &mut [u8], salt: u32) -> Result<()> {
  if apply_wal_salt(records, salt) {
    Ok(())
  } else {
    Err(KiteError::Internal(
      "WAL record bytes are not whole records".to_string(),
    ))
  }
}

/// Write zeros over `zeros`, one positioned write per MiB.
fn write_zeros(pager: &mut FilePager, zeros: ZeroRange) -> Result<()> {
  const PIECE: u64 = 1024 * 1024;
  let buffer = vec![0u8; (zeros.end - zeros.offset).min(PIECE) as usize];
  let mut offset = zeros.offset;
  while offset < zeros.end {
    let len = (zeros.end - offset).min(PIECE) as usize;
    pager.write_range(offset, &buffer[..len])?;
    offset += len as u64;
  }
  Ok(())
}

/// Bytes buffered for the file until the next flush: disjoint byte runs at
/// absolute file offsets, in offset order. Runs that touch are merged, so
/// appends (the WAL's only steady write pattern) grow one run.
#[derive(Debug, Default)]
struct PendingWrites {
  runs: Vec<PendingRun>,
}

#[derive(Debug)]
struct PendingRun {
  offset: u64,
  data: Vec<u8>,
}

impl PendingRun {
  fn end(&self) -> u64 {
    self.offset + self.data.len() as u64
  }
}

impl PendingWrites {
  fn is_empty(&self) -> bool {
    self.runs.is_empty()
  }

  fn clear(&mut self) {
    self.runs.clear();
  }

  /// Buffer `data` for `offset`, over any bytes buffered there before.
  fn write(&mut self, offset: u64, data: &[u8]) {
    if data.is_empty() {
      return;
    }
    let end = offset + data.len() as u64;
    if let Some(last) = self.runs.last_mut() {
      if last.end() == offset {
        last.data.extend_from_slice(data);
        return;
      }
    }
    // The runs that overlap or touch [offset, end) merge with it.
    let first = self.runs.partition_point(|run| run.end() < offset);
    let past = self.runs.partition_point(|run| run.offset <= end);
    if first == past {
      self.runs.insert(
        first,
        PendingRun {
          offset,
          data: data.to_vec(),
        },
      );
      return;
    }
    let start = offset.min(self.runs[first].offset);
    let stop = end.max(self.runs[past - 1].end());
    let mut merged = vec![0; (stop - start) as usize];
    for run in self.runs.drain(first..past) {
      let at = (run.offset - start) as usize;
      merged[at..at + run.data.len()].copy_from_slice(&run.data);
    }
    let at = (offset - start) as usize;
    merged[at..at + data.len()].copy_from_slice(data);
    self.runs.insert(
      first,
      PendingRun {
        offset: start,
        data: merged,
      },
    );
  }

  /// [`Self::write`], keeping `data` itself as a new run when it touches no
  /// buffered byte (as a whole batch written after a flush does).
  fn write_vec(&mut self, offset: u64, data: Vec<u8>) {
    let end = offset + data.len() as u64;
    let first = self.runs.partition_point(|run| run.end() < offset);
    if data.is_empty() || self.runs.get(first).is_some_and(|run| run.offset <= end) {
      self.write(offset, &data);
    } else {
      self.runs.insert(first, PendingRun { offset, data });
    }
  }

  /// Whether one run holds every byte of `offset..offset + length`.
  fn covers(&self, offset: u64, length: usize) -> bool {
    let end = offset + length as u64;
    self
      .runs
      .iter()
      .any(|run| run.offset <= offset && end <= run.end())
  }

  /// Copy the buffered bytes of `offset..offset + buffer.len()` into
  /// `buffer`.
  fn overlay(&self, offset: u64, buffer: &mut [u8]) {
    let end = offset + buffer.len() as u64;
    for run in &self.runs {
      let (from, to) = (run.offset.max(offset), run.end().min(end));
      if from < to {
        let source = (from - run.offset) as usize;
        let target = (from - offset) as usize;
        let len = (to - from) as usize;
        buffer[target..target + len].copy_from_slice(&run.data[source..source + len]);
      }
    }
  }

  /// The pages of `page_size` bytes the runs touch.
  fn pages(&self, page_size: u64) -> usize {
    let mut pages = 0;
    let mut last_page = None;
    for run in &self.runs {
      let first = run.offset / page_size;
      let last = (run.end() - 1) / page_size;
      let first = match last_page {
        Some(previous) if previous >= first => previous + 1,
        _ => first,
      };
      if first <= last {
        pages += (last - first + 1) as usize;
      }
      last_page = Some(last);
    }
    pages
  }
}

/// Bytes of a WAL region as [`WalBuffer::read_region_from`] read them.
#[derive(Debug)]
pub struct RegionBytes {
  /// Where they start, relative to the WAL start.
  start: u64,
  bytes: Vec<u8>,
  /// The region's salt when they were read.
  salt: u32,
}

impl RegionBytes {
  /// The records that parse, up to the first that does not; their bytes as
  /// they lie in the region (salted); and where they end, relative to the
  /// WAL start.
  pub fn parse(mut self) -> (Vec<ParsedWalRecord>, Vec<u8>, u64) {
    let mut records = Vec::new();
    let mut end = 0;
    for (record_type, frame) in wal_frames(&self.bytes, self.salt) {
      records.push(frame.parse(record_type, &self.bytes));
      end = frame.end;
    }
    self.bytes.truncate(end);
    (records, self.bytes, self.start + end as u64)
  }
}

/// Saved WAL positions; see [`WalBuffer::region_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalRegionState {
  head: u64,
  tail: u64,
  primary_head: u64,
  secondary_head: u64,
  active_region: u8,
  primary_salt: u32,
  secondary_salt: u32,
}

impl WalRegionState {
  /// Record these positions, active region, and salts in `header`.
  pub fn store_in_header(&self, header: &mut DbHeaderV1) {
    header.wal_head = self.head;
    header.wal_tail = self.tail;
    header.wal_primary_head = self.primary_head;
    header.wal_secondary_head = self.secondary_head;
    header.active_wal_region = self.active_region;
    header.set_wal_salts(self.primary_salt, self.secondary_salt);
  }
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

    let new_head = buffer.write_record(&record).expect("expected value");
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
      buffer.write_record(&record).expect("expected value");
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
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);

    let record = WalRecord::new(WalRecordType::Begin, 1, Vec::new());
    buffer.write_record(&record).expect("expected value");
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
    buffer.write_record(&record1).expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    let primary_head_before = buffer.primary_head();
    assert!(primary_head_before > 0);

    // Switch to secondary
    buffer.switch_to_secondary();
    assert_eq!(buffer.active_region(), 1);

    // Write to secondary
    let record2 = WalRecord::new(WalRecordType::Begin, 2, Vec::new());
    buffer.write_record(&record2).expect("expected value");
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
    buffer.write_record(&record1).expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // Switch to secondary and write more
    buffer.switch_to_secondary();
    let record2 = WalRecord::new(
      WalRecordType::CreateNode,
      2,
      build_create_node_payload(101, Some("node2")),
    );
    buffer.write_record(&record2).expect("expected value");
    buffer.flush(&mut pager).expect("expected value");

    // Verify both regions have data
    assert!(buffer.primary_head() > 0);
    assert!(buffer.secondary_head() > buffer.secondary_region_start);

    // Merge secondary into primary (simulates checkpoint completion)
    buffer
      .merge_secondary_into_primary(Vec::new(), &mut pager)
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

  fn pending_runs(pending: &PendingWrites) -> Vec<(u64, Vec<u8>)> {
    pending
      .runs
      .iter()
      .map(|run| (run.offset, run.data.clone()))
      .collect()
  }

  #[test]
  fn pending_writes_merge_runs_that_touch_or_overlap() {
    let mut pending = PendingWrites::default();
    pending.write(50, &[5; 10]);
    pending.write(0, &[1; 10]);
    pending.write(10, &[2; 5]);
    assert_eq!(
      pending_runs(&pending),
      vec![(0, [vec![1; 10], vec![2; 5]].concat()), (50, vec![5; 10])]
    );
    // Overlapping both runs: one run, the newest bytes winning.
    pending.write(12, &[3; 40]);
    let mut expected = vec![1; 10];
    expected.extend([2; 2]);
    expected.extend([3; 40]);
    expected.extend([5; 8]);
    assert_eq!(pending_runs(&pending), vec![(0, expected.clone())]);
    assert!(pending.covers(5, 50) && !pending.covers(55, 10));
    let mut window = vec![9; 8];
    pending.overlay(56, &mut window);
    assert_eq!(window, [5, 5, 5, 5, 9, 9, 9, 9]);
    assert_eq!(pending.pages(32), 2);
    pending.write_vec(100, vec![4; 4]);
    pending.write_vec(60, vec![6; 2]);
    assert_eq!(pending.runs.len(), 2);
    assert_eq!(pending_runs(&pending)[0].1.len(), 62);
    assert_eq!(pending.pages(4096), 1);
  }

  /// Header for the 4-page WAL at page 1 used by these tests.
  fn test_header() -> DbHeaderV1 {
    let mut header = DbHeaderV1::new(4096, 4);
    header.wal_start_page = 1;
    header
  }

  fn write_node_record(buffer: &mut WalBuffer, txid: u64) {
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      txid,
      build_create_node_payload(txid, Some(&format!("node-{txid}"))),
    );
    buffer.write_record(&record).expect("write record");
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
      write_node_record(&mut buffer, txid);
    }
    buffer.switch_to_secondary();
    for &txid in post_cut {
      write_node_record(&mut buffer, txid);
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
      let reopened = WalBuffer::from_header(&header).expect("from header");
      assert_eq!(reopened.region_state(), buffer.region_state());
      assert_eq!(reopened.primary_head(), 0);
    }

    // A header without the region fields still names its primary head.
    let mut legacy = test_header();
    legacy.wal_head = 96;
    legacy.wal_primary_head = 0;
    legacy.wal_secondary_head = 0;
    assert_eq!(
      WalBuffer::from_header(&legacy)
        .expect("from header")
        .primary_head(),
      96
    );
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
    let mut reopened = WalBuffer::from_header(&retained_header).expect("from header");
    assert!(reopened.is_primary_retired());
    assert_eq!(txids(&mut reopened, &mut pager), vec![10, 11]);

    // The cut header's primary records are still intact as a crash fallback.
    let mut fallback = WalBuffer::from_header(&cut_header).expect("from header");
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
    let mut reopened = WalBuffer::from_header(&header).expect("from header");
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

  fn write_tx_record(buffer: &mut WalBuffer, record_type: WalRecordType, txid: u64, node_id: u64) {
    let payload = match record_type {
      WalRecordType::CreateNode => build_create_node_payload(node_id, None),
      _ => Vec::new(),
    };
    let record = WalRecord::new(record_type, txid, payload);
    buffer.write_record(&record).expect("write record");
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
      write_tx_record(&mut buffer, record_type, txid, node_id);
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
    write_tx_record(&mut buffer, CreateNode, 2, 22);
    write_tx_record(&mut buffer, Commit, 2, 0);
    write_tx_record(&mut buffer, Commit, 5, 0);
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
    write_tx_record(&mut buffer, WalRecordType::Begin, 7, 0);
    let key = "k".repeat(1500);
    for node_id in 0..3 {
      let record = WalRecord::new(
        WalRecordType::CreateNode,
        7,
        build_create_node_payload(node_id, Some(&key)),
      );
      buffer.write_record(&record).expect("write");
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
    let write = |buffer: &mut WalBuffer, (record_type, txid)| {
      let payload = match record_type {
        WalRecordType::CreateNode => build_create_node_payload(txid, Some(&key)),
        _ => Vec::new(),
      };
      let record = WalRecord::new(record_type, txid, payload);
      buffer.write_record(&record).expect("write record");
    };
    for &record in primary {
      write(&mut buffer, record);
    }
    buffer.switch_to_secondary();
    for &record in secondary {
      write(&mut buffer, record);
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
          .expect("from header")
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
      write_tx_record(&mut buffer, record_type, txid, txid);
    }
    let valid_end = buffer.primary_head();
    for (record_type, txid) in [(Begin, 2), (CreateNode, 2), (Commit, 2)] {
      write_tx_record(&mut buffer, record_type, txid, txid);
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
    write_tx_record(&mut buffer, Begin, 3, 0);
    buffer.flush(&mut pager).expect("flush");
    assert_eq!(txids(&mut buffer, &mut pager), vec![1, 1, 1, 3]);
  }

  /// A header naming `head` bytes of `region` (as after a crash in which the
  /// header naming a commit landed but its WAL page did not), reopened.
  fn reopened_naming(buffer: &WalBuffer, region: u8, head: u64) -> WalBuffer {
    let mut header = test_header();
    buffer.store_in_header(&mut header);
    if region == 0 {
      header.wal_primary_head = head;
    } else {
      header.wal_secondary_head = head;
    }
    header.wal_head = head;
    WalBuffer::from_header(&header).expect("from header")
  }

  /// A reset rewinds the primary head but leaves the records in place. With
  /// a fresh salt they no longer parse, so a header naming bytes past the
  /// last record written since does not replay the previous cycle's.
  #[test]
  fn reset_salts_the_primary_region_afresh() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    write_node_record(&mut buffer, 1);
    let record_len = buffer.primary_head();
    write_node_record(&mut buffer, 2);
    buffer.flush(&mut pager).expect("flush");
    let old_salt = buffer.primary_salt;

    buffer.reset();
    assert_ne!(buffer.primary_salt, old_salt);
    assert_ne!(buffer.primary_salt, 0);
    write_node_record(&mut buffer, 3);
    buffer.flush(&mut pager).expect("flush");
    assert_eq!(buffer.primary_head(), record_len);

    let mut crashed = reopened_naming(&buffer, 0, 2 * record_len);
    assert_eq!(txids(&mut crashed, &mut pager), vec![3]);
    assert!(crashed.trim_to_valid_records(&mut pager).expect("trim"));
    assert_eq!(crashed.primary_head(), record_len);
  }

  /// Leaving a cut keeps the secondary region's bytes. The next cut writes
  /// there with a fresh salt, so the earlier cut's records are not its own.
  #[test]
  fn each_cut_salts_the_secondary_region_afresh() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = buffer_with_cut(&mut pager, 1, &[10, 11]);
    let first_cut_salt = buffer.secondary_salt;
    assert_ne!(first_cut_salt, 0);
    assert!(buffer
      .merge_cut_into_primary(&mut pager)
      .expect("leave cut"));
    assert_eq!(txids(&mut buffer, &mut pager), vec![1, 10, 11]);

    buffer.switch_to_secondary();
    assert_ne!(buffer.secondary_salt, first_cut_salt);
    assert_ne!(buffer.secondary_salt, buffer.primary_salt);
    write_node_record(&mut buffer, 12);
    buffer.flush(&mut pager).expect("flush");
    let record_len = buffer.secondary_head() - buffer.secondary_region_start;

    let mut crashed = reopened_naming(&buffer, 1, buffer.secondary_region_start + 2 * record_len);
    let secondary: Vec<u64> = crashed
      .scan_region(1, &mut pager)
      .expect("scan")
      .iter()
      .map(|record| record.txid)
      .collect();
    assert_eq!(secondary, vec![12]);
  }

  /// Compaction rewrites the retained records from the primary region's
  /// start, so it salts them afresh. Restoring a saved region state (as a
  /// failed install does) restores the salts with the positions, matching
  /// the durable header.
  #[test]
  fn compaction_salts_afresh_and_region_state_restores_salts() {
    let (mut pager, _temp) = create_test_pager();
    let mut buffer = buffer_with_cut(&mut pager, 3, &[10]);
    buffer.retire_primary_region();
    let retained = buffer.region_state();
    let (primary_salt, secondary_salt) = (buffer.primary_salt, buffer.secondary_salt);

    buffer
      .compact_secondary_into_primary(&mut pager)
      .expect("compact");
    assert_ne!(buffer.primary_salt, primary_salt);
    assert_ne!(buffer.primary_salt, secondary_salt);
    assert_eq!(txids(&mut buffer, &mut pager), vec![10]);

    buffer.restore_region_state(retained);
    assert_eq!(buffer.region_state(), retained);
    assert_eq!(
      (buffer.primary_salt, buffer.secondary_salt),
      (primary_salt, secondary_salt)
    );
    assert_eq!(txids(&mut buffer, &mut pager), vec![10]);
  }

  #[test]
  fn fresh_salt_is_never_zero_or_in_use() {
    let mut buffer = WalBuffer::new(4096, 4 * 4096, 4096);
    buffer.primary_salt = 7;
    buffer.secondary_salt = 3;
    assert_eq!(buffer.fresh_salt(), 8);
    buffer.primary_salt = u32::MAX;
    buffer.secondary_salt = 1;
    assert_eq!(buffer.fresh_salt(), 2);
  }

  /// A v1 WAL (salts 0) stays unsalted, and its header v1, until a reset
  /// salts the emptied region; from then on the header needs a v2 reader.
  #[test]
  fn first_salt_upgrades_a_v1_header() {
    let mut header = test_header();
    header.version = 1;
    header.min_reader_version = 1;
    header.wal_primary_salt = 0;
    header.wal_secondary_salt = 0;
    let mut buffer = WalBuffer::from_header(&header).expect("from header");
    buffer.store_in_header(&mut header);
    assert_eq!((header.version, header.min_reader_version), (1, 1));

    buffer.reset();
    buffer.store_in_header(&mut header);
    assert_ne!(header.wal_primary_salt, 0);
    assert_eq!(
      (header.version, header.min_reader_version),
      (VERSION_SINGLE_FILE, MIN_READER_SINGLE_FILE)
    );
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
