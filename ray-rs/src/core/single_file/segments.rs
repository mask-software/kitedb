//! WAL segments: where the WAL spills its records instead of forcing a
//! checkpoint.
//!
//! The WAL area is fixed. When a commit does not fit in it, its records are
//! *spilled*: copied, unsalted, into a WAL segment, an extent of pages
//! elsewhere in the file that the header names (`DbHeaderV1::wal_segments`),
//! and the WAL starts over under a fresh salt. Spills fill an extent one
//! after another until it is full; then a new extent is allocated (the first
//! free range that holds it, else the end of the file). The log is the
//! segments by seq, each up to its `byte_len`, then the WAL; recovery replays
//! it in that order. A checkpoint that covers the segments frees them.
//!
//! A spill is crash safe without zeros or salts in the extent: its bytes are
//! written and synced before any header names them, recovery reads exactly
//! the `byte_len` a durable header names, and the WAL is not written again
//! until the header naming the segment and the emptied WAL is durable in
//! both slots (in `SyncMode::Normal` a slot's durable content can lag: both
//! durable slots must name the new state before the WAL's records are
//! overwritten). Until then a crash finds the records in the WAL.

use std::collections::HashMap;

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::core::wal::buffer::{wal_region_bytes, WalBuffer};
use crate::core::wal::record::{
  apply_wal_salt, parse_wal_record_with_salt, wal_records_end, ParsedWalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;

use super::checkpoint::{checkpoint_phase, restore_header, CheckpointPhase};
use super::SingleFileDB;

/// What `SingleFileDB::spill_or_append` did.
pub(crate) enum SpillOutcome {
  /// The WAL spilled (or has room again): write the records to it.
  Spilled,
  /// The records went to the WAL segment log.
  Appended,
  /// The WAL segments are at their limit, or their table is full: a
  /// checkpoint must free some first (see `wait_for_segment_space`).
  Full,
}

/// Parse `bytes`, whole records with salt `salt` (0: unsalted, as WAL
/// segments hold them), all of them: bytes that do not parse are corruption
/// in what a durable header names, not a torn tail. `what` names them in the
/// error.
pub(crate) fn parse_whole_records(
  bytes: &[u8],
  salt: u32,
  what: impl FnOnce() -> String,
) -> Result<Vec<ParsedWalRecord>> {
  let mut records = Vec::new();
  let mut offset = 0;
  while offset < bytes.len() {
    let Some(record) = parse_wal_record_with_salt(bytes, offset, salt) else {
      return Err(KiteError::InvalidWal(format!(
        "{} does not parse at byte {offset} of its {} bytes",
        what(),
        bytes.len()
      )));
    };
    offset = record.record_end;
    records.push(record);
  }
  Ok(records)
}

/// Read and parse the records of `segment` from byte `from` to its
/// `byte_len`, with `read` (a file offset and a length).
pub(crate) fn read_wal_segment(
  segment: &WalSegment,
  page_size: u64,
  from: u64,
  read: impl FnOnce(u64, usize) -> Result<Vec<u8>>,
) -> Result<Vec<ParsedWalRecord>> {
  if from >= segment.byte_len {
    return Ok(Vec::new());
  }
  let bytes = read(
    segment.start_page * page_size + from,
    (segment.byte_len - from) as usize,
  )?;
  parse_whole_records(&bytes, 0, || {
    format!(
      "WAL segment {} (pages {}..{}) from byte {from}",
      segment.seq,
      segment.start_page,
      segment.end_page()
    )
  })
}

/// The records of the WAL segments a header names, in log order (see
/// `read_wal_segment_log`).
pub(crate) struct SegmentLog {
  pub(crate) records: Vec<ParsedWalRecord>,
  /// How many of `records` lie in segments the snapshot covers
  /// (`WalSegmentTable::covered`): recovery replays only the transactions
  /// whose COMMIT record comes after those.
  pub(crate) covered_records: usize,
  /// Each segment's seq, with the number of `records` up to its end.
  pub(crate) segment_ends: Vec<(u64, usize)>,
}

/// The records of the WAL segments `header` names, in log order. A segment
/// holds whole unsalted records up to its `byte_len`, synced before any
/// header named it, so a record that does not parse there is corruption,
/// not a torn write: an error, not the log's end.
pub(crate) fn read_wal_segment_log(pager: &FilePager, header: &DbHeaderV1) -> Result<SegmentLog> {
  let page_size = header.page_size as u64;
  let mut log = SegmentLog {
    records: Vec::new(),
    covered_records: 0,
    segment_ends: Vec::with_capacity(header.wal_segments.entries.len()),
  };
  for segment in header.wal_segments.entries.iter() {
    log.records.extend(read_wal_segment(
      segment,
      page_size,
      0,
      |offset, length| pager.read_range(offset, length),
    )?);
    if segment.seq <= header.wal_segments.covered {
      log.covered_records = log.records.len();
    }
    log.segment_ends.push((segment.seq, log.records.len()));
  }
  Ok(log)
}

/// `read_wal_segment_log`'s records and covered count.
pub(crate) fn read_wal_segment_records(
  pager: &FilePager,
  header: &DbHeaderV1,
) -> Result<(Vec<ParsedWalRecord>, usize)> {
  let log = read_wal_segment_log(pager, header)?;
  Ok((log.records, log.covered_records))
}

/// The spilled transactions of a log (`records`: the segments' records, then
/// the WAL's; `segment_ends` and `covered_records` as `read_wal_segment_log`
/// gives them): each transaction that commits after the covered records with
/// a record in a WAL segment, from its last BEGIN on (what replay keeps),
/// with the oldest such segment and where its COMMIT lies. An open rebuilds
/// `SingleFileInner::spilled_txids` from them, since the segments they hold
/// stay needed until a checkpoint covers them, across reopens.
pub(crate) fn spilled_transactions_in_log(
  records: &[ParsedWalRecord],
  segment_ends: &[(u64, usize)],
  covered_records: usize,
) -> HashMap<TxId, SpilledTransaction> {
  let segment_of = |index: usize| {
    let at = segment_ends.partition_point(|&(_, end)| end <= index);
    segment_ends.get(at).map(|&(seq, _)| seq)
  };
  let mut begun: HashMap<TxId, usize> = HashMap::new();
  let mut spilled = HashMap::new();
  for (index, record) in records.iter().enumerate() {
    match record.record_type {
      WalRecordType::Begin => {
        begun.insert(record.txid, index);
      }
      WalRecordType::Rollback => {
        begun.remove(&record.txid);
      }
      WalRecordType::Commit => {
        let Some(begin) = begun.remove(&record.txid) else {
          continue;
        };
        if index < covered_records {
          continue;
        }
        if let Some(first_segment) = segment_of(begin) {
          spilled.insert(
            record.txid,
            SpilledTransaction {
              first_segment,
              commit_segment: Some(segment_of(index).unwrap_or(COMMITTED_IN_WAL)),
            },
          );
        }
      }
      _ => {}
    }
  }
  spilled
}

/// Where a spilled transaction's COMMIT record lies while it is in the WAL
/// (`SpilledTransaction::commit_segment`).
pub(crate) const COMMITTED_IN_WAL: u64 = u64::MAX;

/// A write transaction with records in a WAL segment, from the spill that
/// moved its first records there until a checkpoint covers its commit, or it
/// ends without one (see `SingleFileInner::spilled_txids`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpilledTransaction {
  /// The oldest segment holding a record of it.
  pub(crate) first_segment: u64,
  /// Where its COMMIT record lies: `None` while it is open; the segment
  /// holding it once it committed, or `COMMITTED_IN_WAL` until a spill moves
  /// it there.
  pub(crate) commit_segment: Option<u64>,
}

impl SingleFileDB {
  /// Pages of a new WAL segment extent: the configured size, else a
  /// sixteenth of the segment limit, from two WALs to the larger of
  /// `WAL_SEGMENT_DEFAULT_SIZE` and two WALs; and at least one and a half WALs and
  /// `record_bytes`, in whole pages. Small next to the limit: an open
  /// transaction whose records spilled keeps the extent it began in whole,
  /// with the records written before it, until a checkpoint covers its
  /// commit, and those count against the limit too. A sixteenth leaves the
  /// table's 64 entries room for the limit's worth of extents and more.
  fn wal_segment_extent_pages(&self, header: &DbHeaderV1, record_bytes: u64) -> u64 {
    let page_size = header.page_size as u64;
    let wal_bytes = header.wal_page_count.saturating_mul(page_size);
    let configured = self.wal_segment_size.unwrap_or_else(|| {
      (self.wal_segment_limit(header) / 16).clamp(
        wal_bytes.saturating_mul(2),
        (WAL_SEGMENT_DEFAULT_SIZE as u64).max(wal_bytes.saturating_mul(2)),
      )
    });
    let extent = configured
      .max(wal_bytes.saturating_mul(3) / 2)
      .max(record_bytes);
    extent.div_ceil(page_size)
  }

  /// Bytes of log (WAL segments and WAL) at which a checkpoint starts: the
  /// `checkpoint_log_ratio` of the snapshot's size, at least half the WAL's
  /// region (three eighths of the WAL: where earlier releases checkpointed
  /// by default, so a small database holds no more log, and no larger delta,
  /// than it did with them), and at most the log budget, which bounds the
  /// memory the delta replaying the log takes (about ten times its size) on
  /// a large database. A budget too large to compute with saturates: no cap.
  pub(crate) fn checkpoint_log_trigger(&self, header: &DbHeaderV1) -> u64 {
    let page_size = header.page_size as u64;
    let floor = (wal_region_bytes(header) / 2).min(self.checkpoint_log_budget);
    let snapshot = header.snapshot_page_count.saturating_mul(page_size);
    // Saturates at u64::MAX.
    let wanted = (self.checkpoint_log_ratio * snapshot as f64) as u64;
    wanted.clamp(floor, self.checkpoint_log_budget.max(floor))
  }

  /// The most bytes of WAL segments: past it the WAL spills no more, and
  /// writers wait for a checkpoint (or, without automatic checkpoints, fail
  /// with `WalBufferFull`). Twice the checkpoint trigger, at least 16 WALs,
  /// at most four times the log budget; or the explicit `wal_segment_limit`.
  pub(crate) fn wal_segment_limit(&self, header: &DbHeaderV1) -> u64 {
    let explicit = self
      .wal_segment_limit_bytes
      .load(std::sync::atomic::Ordering::Relaxed);
    if explicit > 0 {
      return explicit;
    }
    let wal = header
      .wal_page_count
      .saturating_mul(header.page_size as u64);
    // The trigger is at most the budget, so this is at least twice it.
    let trigger = self.checkpoint_log_trigger(header);
    trigger
      .saturating_mul(2)
      .max(wal.saturating_mul(16))
      .min(self.checkpoint_log_budget.saturating_mul(4))
  }

  /// The size of the log the snapshot does not cover (the WAL segments past
  /// the covered ones, and the WAL), as a fraction of the checkpoint
  /// trigger: at 1.0 an automatic checkpoint starts. Segments a checkpoint
  /// kept for a transaction still open do not count: checkpointing again
  /// would keep them again, and the delta holds nothing of them.
  pub(crate) fn log_usage_ratio(&self) -> f64 {
    let wal_bytes = self.wal_buffer.lock().used();
    let header = self.header.read();
    let table = &header.wal_segments;
    let segments: u64 = table
      .entries
      .iter()
      .filter(|segment| segment.seq > table.covered)
      .map(|segment| segment.byte_len)
      .sum();
    (segments + wal_bytes) as f64 / self.checkpoint_log_trigger(&header).max(1) as f64
  }

  /// Segments `header` names that nothing needs any more: covered by the
  /// snapshot, and older than every segment holding a record of a
  /// transaction it does not cover (see `spilled_txids`). A spill drops them
  /// from the table and frees them, so segments a checkpoint kept for a
  /// transaction that has since ended do not wait for another checkpoint.
  /// None while a background checkpoint runs, unless `for_cut` (its own
  /// cut): it may be reading them.
  pub(crate) fn unneeded_wal_segments(
    &self,
    header: &DbHeaderV1,
    for_cut: bool,
  ) -> Vec<WalSegment> {
    if !for_cut && self.is_checkpoint_running() {
      return Vec::new();
    }
    let table = &header.wal_segments;
    let needed_from = self
      .spilled_txids
      .lock()
      .values()
      .map(|spilled| spilled.first_segment)
      .min()
      .unwrap_or(u64::MAX);
    table
      .entries
      .iter()
      .filter(|segment| segment.seq <= table.covered && segment.seq < needed_from)
      .copied()
      .collect()
  }

  /// Entries of the WAL segment table: `MAX_WAL_SEGMENTS`, or fewer in tests
  /// that fill the table quickly (`set_wal_segment_test_capacity`).
  pub(crate) fn wal_segment_capacity(&self) -> usize {
    #[cfg(test)]
    {
      let capacity = self
        .wal_segment_test_capacity
        .load(std::sync::atomic::Ordering::Relaxed);
      if capacity > 0 {
        return capacity.min(MAX_WAL_SEGMENTS);
      }
    }
    MAX_WAL_SEGMENTS
  }

  /// Whether the WAL may spill now: the segments are under their limit, with
  /// room in the table (one entry stays free for a checkpoint's cut, which
  /// spills whatever the limit). `unneeded` (`unneeded_wal_segments`) do not
  /// count: a spill drops them. A caller that spills on this answer hands the
  /// spill the same `unneeded`, under the same hold of the commit lock and
  /// the header lock, so the spill does what the decision counted on.
  pub(crate) fn can_spill(&self, header: &DbHeaderV1, unneeded: &[WalSegment]) -> bool {
    if self.read_only {
      return false;
    }
    let (entries, bytes) = header
      .wal_segments
      .entries
      .iter()
      .filter(|segment| !unneeded.iter().any(|dropped| dropped.seq == segment.seq))
      .fold((0, 0u64), |(entries, bytes), segment| {
        (entries + 1, bytes.saturating_add(segment.byte_len))
      });
    entries < self.wal_segment_capacity() - 1 && bytes < self.wal_segment_limit(header)
  }

  /// Whether a writer could spill now (`can_spill`, counting out the
  /// segments a spill now would drop).
  pub(crate) fn has_segment_space(&self) -> bool {
    let header = self.header.read();
    let unneeded = self.unneeded_wal_segments(&header, false);
    self.can_spill(&header, &unneeded)
  }

  /// The oldest WAL segment an open write transaction holds records in: no
  /// checkpoint can drop it, or any after it, before that transaction ends.
  pub(crate) fn oldest_pinned_segment(&self) -> Option<u64> {
    self
      .spilled_txids
      .lock()
      .values()
      .filter(|spilled| spilled.commit_segment.is_none())
      .map(|spilled| spilled.first_segment)
      .min()
  }

  /// The oldest WAL segment a checkpoint whose cut is at segment `covered`
  /// must keep: the oldest holding a record of a transaction its snapshot
  /// does not cover (open at the cut, or committed after it), else the one
  /// after `covered`.
  pub(crate) fn wal_segments_needed_after(&self, covered: u64) -> u64 {
    self
      .spilled_txids
      .lock()
      .values()
      .filter(|spilled| spilled.commit_segment.is_none_or(|commit| commit > covered))
      .map(|spilled| spilled.first_segment)
      .min()
      .map_or(covered + 1, |oldest| oldest.min(covered + 1))
  }

  /// Whether the WAL segments are full (see `can_spill`) with segments no
  /// checkpoint can drop (from `oldest_pinned_segment` on): a writer that
  /// needs room then fails with `WalBufferFull` instead of waiting for a
  /// checkpoint that cannot free any. Says why in a warning.
  pub(crate) fn segments_full_of_pinned(&self) -> bool {
    let Some(oldest) = self.oldest_pinned_segment() else {
      return false;
    };
    let header = self.header.read();
    let (count, bytes) = header
      .wal_segments
      .entries
      .iter()
      .filter(|segment| segment.seq >= oldest)
      .fold((0, 0u64), |(count, bytes), segment| {
        (count + 1, bytes.saturating_add(segment.byte_len))
      });
    let limit = self.wal_segment_limit(&header);
    let full = bytes >= limit || count >= self.wal_segment_capacity() - 1;
    if full {
      #[cfg(test)]
      super::checkpoint::count_checkpoint_test_pinned_refusal(&self.path);
      eprintln!(
        "Warning: WAL segments are full: open write transactions hold records in {count} of \
         them ({bytes} bytes; the limit is {limit}), which no checkpoint frees before they finish"
      );
    }
    full
  }

  /// Whether a spill of `length` bytes needs a new table entry: the newest
  /// segment is sealed, or has no room for them.
  pub(crate) fn spill_needs_new_segment(header: &DbHeaderV1, length: u64) -> bool {
    let page_size = header.page_size as u64;
    !header.wal_segments.entries.last().is_some_and(|last| {
      !last.sealed && last.byte_len.saturating_add(length) <= last.page_count * page_size
    })
  }

  /// Spill: move the WAL's records, then `extra` (whole unsalted records
  /// that follow them in the log: a commit or a transaction's records that
  /// do not fit in the WAL at all), to the end of the WAL segment log, and
  /// empty the WAL. `unneeded`, the segments nothing needs any more as the
  /// caller's decision to spill counted them (`unneeded_wal_segments`, under
  /// the same hold of the locks), leave the table in the same header, and
  /// are freed once it is durable. Callers checked `can_spill` (a
  /// checkpoint's cut spills whatever the limit), and hold the commit lock
  /// (`lock_commits`) and the pager, the WAL and the header locked, in that
  /// order: no checkpoint can cut meanwhile, so none reads `unneeded`. Fails
  /// with `WalBufferFull`, changing nothing, if the records need a new
  /// segment and the table has no room.
  ///
  /// On error nothing a header names has changed: the WAL keeps its records
  /// (its positions restored if only the header install failed), and a new
  /// extent's pages are freed, or held back if a header slot may name them.
  pub(crate) fn spill_wal(
    &self,
    pager: &mut FilePager,
    wal: &mut WalBuffer,
    header: &mut DbHeaderV1,
    extra: &[u8],
    unneeded: &[WalSegment],
  ) -> Result<()> {
    // A writable open leaves the WAL in the primary region alone.
    debug_assert!(wal.active_region() == 0 && !wal.is_primary_retired());
    wal.flush(pager)?;
    let (tail, head) = (wal.tail(), wal.head());
    let mut bytes = if head > tail {
      pager.read_range(wal.base_offset() + tail, (head - tail) as usize)?
    } else {
      Vec::new()
    };
    let salt = header.wal_primary_salt;
    if wal_records_end(&bytes, salt) != bytes.len() || !apply_wal_salt(&mut bytes, salt) {
      return Err(KiteError::InvalidWal(
        "the WAL's records do not parse; not spilling them".to_string(),
      ));
    }
    bytes.extend_from_slice(extra);
    if bytes.is_empty() {
      return Ok(());
    }
    let page_size = header.page_size as u64;
    let length = bytes.len() as u64;

    // Append to the open extent if the records fit, else start one.
    let mut entries: Vec<WalSegment> = header
      .wal_segments
      .entries
      .iter()
      .filter(|segment| !unneeded.iter().any(|dropped| dropped.seq == segment.seq))
      .copied()
      .collect();
    let appends = !Self::spill_needs_new_segment(header, length)
      && entries
        .last()
        .is_some_and(|last| Some(last.seq) == header.wal_segments.entries.last().map(|s| s.seq));
    if !appends {
      if entries.len() >= self.wal_segment_capacity() {
        return Err(KiteError::WalBufferFull);
      }
      let page_count = self.wal_segment_extent_pages(header, length);
      let start_page = self.reserve_pages(pager, header, page_count, true)?;
      entries.push(WalSegment {
        seq: header.wal_segments.next_seq(),
        start_page,
        page_count,
        byte_len: 0,
        sealed: false,
      });
    }
    let segment = *entries.last().expect("an open segment");
    let free_new_extent = |pager: &mut FilePager| {
      if !appends {
        pager.free_pages(segment.start_page as u32, segment.page_count as u32);
      }
    };
    let written = pager
      .write_range(segment.start_page * page_size + segment.byte_len, &bytes)
      .and_then(|()| pager.sync())
      .and_then(|()| checkpoint_phase(&self.path, CheckpointPhase::SpillSegmentWritten));
    if let Err(error) = written {
      free_new_extent(pager);
      return Err(error);
    }

    // The header naming the records in the segment and an empty WAL.
    let prior_header = header.clone();
    let prior_wal = wal.region_state();
    if let Some(last) = entries.last_mut() {
      last.byte_len += length;
    }
    header.wal_segments = WalSegmentTable {
      next_seq: header.wal_segments.next_seq().max(segment.seq + 1),
      covered: header.wal_segments.covered,
      entries: entries.into(),
    };
    wal.reset();
    wal.store_in_header(header);
    header.max_node_id = self
      .next_node_id
      .load(std::sync::atomic::Ordering::SeqCst)
      .saturating_sub(1);
    header.next_tx_id = self.next_tx_id.load(std::sync::atomic::Ordering::SeqCst);
    if let Err(error) = self.persist_spill_header(pager, header) {
      restore_header(header, prior_header);
      wal.restore_region_state(prior_wal);
      if !appends {
        // A slot may name it: out of reuse until a later install is durable
        // in both slots.
        pager.defer_free_pages(segment.start_page as u32, segment.page_count as u32);
      }
      return Err(error);
    }
    // Both slots name the new table: no fallback reaches the dropped ones.
    if !unneeded.is_empty() {
      Self::free_wal_segments(pager, unneeded);
      self.note_wal_segments_freed();
    }
    self
      .wal_spills
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    self.note_spilled_transactions(segment.seq);
    Ok(())
  }

  /// Drop `unneeded`, the segments nothing needs any more
  /// (`unneeded_wal_segments`), from the table, durably in both header
  /// slots, then free them. For a checkpoint's cut that has nothing to spill
  /// or to cover; callers hold what `spill_wal`'s do. Returns whether it
  /// dropped any.
  pub(crate) fn drop_unneeded_wal_segments(
    &self,
    pager: &mut FilePager,
    header: &mut DbHeaderV1,
    unneeded: &[WalSegment],
  ) -> Result<bool> {
    if unneeded.is_empty() {
      return Ok(false);
    }
    let prior_header = header.clone();
    let kept: Vec<WalSegment> = header
      .wal_segments
      .entries
      .iter()
      .filter(|segment| !unneeded.iter().any(|dropped| dropped.seq == segment.seq))
      .copied()
      .collect();
    header.wal_segments = WalSegmentTable {
      next_seq: header.wal_segments.next_seq(),
      covered: header.wal_segments.covered,
      entries: kept.into(),
    };
    if let Err(error) = self.persist_spill_header(pager, header) {
      restore_header(header, prior_header);
      return Err(error);
    }
    Self::free_wal_segments(pager, unneeded);
    self.note_wal_segments_freed();
    Ok(true)
  }

  /// Count a release of WAL segment space (see `wait_for_segment_space`).
  pub(crate) fn note_wal_segments_freed(&self) {
    self
      .wal_segment_frees
      .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
  }

  /// After a spill into segment `seq`: the open write transactions' records
  /// so far are in it or older ones, and the COMMIT records the WAL held are
  /// in it.
  fn note_spilled_transactions(&self, seq: u64) {
    let open = self.open_write_txids.lock();
    let mut spilled = self.spilled_txids.lock();
    for spilled in spilled.values_mut() {
      if spilled.commit_segment == Some(COMMITTED_IN_WAL) {
        spilled.commit_segment = Some(seq);
      }
    }
    for &txid in open.iter() {
      spilled.entry(txid).or_insert(SpilledTransaction {
        first_segment: seq,
        commit_segment: None,
      });
    }
  }

  /// Forget the spilled transactions a checkpoint whose cut is at segment
  /// `covered` holds (their COMMIT records lie in a segment up to it).
  pub(crate) fn forget_covered_spilled_transactions(&self, covered: u64) {
    self.spilled_txids.lock().retain(|_, spilled| {
      spilled
        .commit_segment
        .is_none_or(|commit| commit == COMMITTED_IN_WAL || commit > covered)
    });
  }

  /// Seal the newest WAL segment in `header` (in memory: the next header
  /// written carries it): spills start a new one after it. A checkpoint's
  /// cut seals it, so its snapshot covers whole segments.
  pub(crate) fn seal_newest_wal_segment(header: &mut DbHeaderV1) {
    if header
      .wal_segments
      .entries
      .last()
      .is_some_and(|last| !last.sealed)
    {
      let mut entries = header.wal_segments.entries.to_vec();
      if let Some(last) = entries.last_mut() {
        last.sealed = true;
      }
      header.wal_segments.entries = entries.into();
    }
  }

  /// For a writer holding no lock whose records (`records`, whole and
  /// unsalted) the WAL refused: spill the WAL so they fit, or, if it is empty
  /// and they do not fit anyway, append them to the WAL segment log and run
  /// `then` as if the WAL had taken them.
  pub(crate) fn spill_or_append(
    &self,
    records: &[u8],
    then: impl FnOnce(),
  ) -> Result<SpillOutcome> {
    let _commit_guard = self.lock_commits();
    self.ensure_writes_allowed()?;
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    let mut header = self.header.write();
    if wal.can_fit(records.len()) {
      // Another writer made room meanwhile.
      return Ok(SpillOutcome::Spilled);
    }
    let unneeded = self.unneeded_wal_segments(&header, false);
    if !self.can_spill(&header, &unneeded) {
      return Ok(SpillOutcome::Full);
    }
    checkpoint_phase(&self.path, CheckpointPhase::SpillDecided)?;
    if !wal.is_empty() {
      self.spill_wal(&mut pager, &mut wal, &mut header, &[], &unneeded)?;
      return Ok(SpillOutcome::Spilled);
    }
    self.spill_wal(&mut pager, &mut wal, &mut header, records, &unneeded)?;
    then();
    if let Some(last) = header.wal_segments.entries.last() {
      self.note_spilled_transactions(last.seq);
    }
    Ok(SpillOutcome::Appended)
  }

  /// Reserve `page_count` pages for data no header names yet (a snapshot, a
  /// WAL segment extent): with `reuse_free`, the first free range that holds
  /// them, taken off the free list; else (or with none) the pages past the
  /// end of the file and of every page `header` names, which the file grows
  /// to hold now. Callers hold the pager lock across the call, so choosing
  /// the pages and taking them are one step, and no other allocator can
  /// choose them too; they stay the caller's until it frees them or a
  /// header names them.
  pub(crate) fn reserve_pages(
    &self,
    pager: &mut FilePager,
    header: &DbHeaderV1,
    page_count: u64,
    reuse_free: bool,
  ) -> Result<u64> {
    let count = u32::try_from(page_count).map_err(|_| {
      KiteError::Internal(format!(
        "{page_count} pages are too many to reserve at once"
      ))
    })?;
    self.withdraw_live_pages(pager, header);
    if reuse_free {
      if let Some(start_page) = pager.find_free_range(count) {
        pager.consume_free_range(start_page, count);
        return Ok(start_page as u64);
      }
    }
    let page_size = header.page_size as u64;
    let file_pages = pager.file_size().div_ceil(page_size);
    let start_page = Self::append_start_page(pager, header);
    let end_page = start_page + page_count;
    if end_page > file_pages {
      let grow = u32::try_from(end_page - file_pages).map_err(|_| {
        KiteError::Internal(format!(
          "growing the file by {} pages is too much at once",
          end_page - file_pages
        ))
      })?;
      pager.allocate_pages(grow)?;
    }
    Ok(start_page)
  }

  /// Withdraw from the free lists every page `header` names (header pages,
  /// WAL, snapshot, WAL segments): a bookkeeping mistake listing one must not
  /// put new data over it.
  pub(crate) fn withdraw_live_pages(&self, pager: &mut FilePager, header: &DbHeaderV1) {
    let mut listed = pager
      .withdraw_free_pages(0, (header.wal_start_page + header.wal_page_count) as u32)
      + pager.withdraw_free_pages(
        header.snapshot_start_page as u32,
        header
          .snapshot_start_page
          .saturating_add(header.snapshot_page_count) as u32,
      );
    for segment in header.wal_segments.entries.iter() {
      listed += pager.withdraw_free_pages(segment.start_page as u32, segment.end_page() as u32);
    }
    if listed > 0 {
      eprintln!(
        "Warning: {listed} pages of the header, WAL, snapshot or WAL segments were listed as \
         free; withdrew them from reuse"
      );
    }
  }

  /// Install a spill's header durably in both slots (see the module docs).
  /// If the first slot never becomes durable, the next header write targets
  /// it again (as `persist_checkpoint_header` does).
  fn persist_spill_header(&self, pager: &mut FilePager, header: &mut DbHeaderV1) -> Result<()> {
    let durable_slot = self.header_slot.load(std::sync::atomic::Ordering::Acquire);
    let first_slot = self
      .persist_header(pager, header, false)
      .and_then(|()| pager.sync());
    if let Err(error) = first_slot {
      self
        .header_slot
        .store(durable_slot, std::sync::atomic::Ordering::Release);
      return Err(error);
    }
    checkpoint_phase(&self.path, CheckpointPhase::SpillHeaderDurable)?;
    self.persist_header(pager, header, true)
  }

  /// Drop every WAL segment from `header` (a checkpoint covering the whole
  /// log installs it): the snapshot covers every seq written so far. Returns
  /// the dropped segments, to free once the header is durable in both slots.
  pub(crate) fn drop_all_wal_segments(header: &mut DbHeaderV1) -> Vec<WalSegment> {
    let dropped = header.wal_segments.entries.to_vec();
    let next_seq = header.wal_segments.next_seq();
    header.wal_segments = WalSegmentTable {
      next_seq,
      covered: next_seq - 1,
      entries: Vec::new().into(),
    };
    dropped
  }

  /// Drop from `header` the WAL segments before `keep_from`, and record that
  /// the snapshot covers the transactions committed in segments up to
  /// `covered` (a background checkpoint's install; see `LogCut`). Returns
  /// the dropped segments, to free once the header is durable in both slots.
  pub(crate) fn drop_wal_segments_before(
    header: &mut DbHeaderV1,
    covered: u64,
    keep_from: u64,
  ) -> Vec<WalSegment> {
    let (dropped, kept): (Vec<WalSegment>, Vec<WalSegment>) = header
      .wal_segments
      .entries
      .iter()
      .partition(|segment| segment.seq < keep_from);
    header.wal_segments = WalSegmentTable {
      next_seq: header.wal_segments.next_seq(),
      covered: covered.max(header.wal_segments.covered),
      entries: kept.into(),
    };
    dropped
  }

  /// Free the pages of `segments`, which no durable header slot names any
  /// more.
  pub(crate) fn free_wal_segments(pager: &mut FilePager, segments: &[WalSegment]) {
    for segment in segments {
      pager.free_pages(segment.start_page as u32, segment.page_count as u32);
    }
  }
}

/// Finish, at a writable open, a background checkpoint's cut that does not
/// fit back into the primary region (a v2 writer left it; v3 writers never
/// cut into the secondary region): its records, primary region's then
/// secondary region's, go to a new WAL segment at the end of the file, and a
/// header naming it and an empty WAL is installed in both slots. Crash safe
/// as a spill is: until a slot naming the segment is durable, the cut header
/// stays the fallback, and nothing overwrites the WAL before both are.
pub(crate) fn finish_cut_into_segment(
  pager: &mut FilePager,
  wal: &mut WalBuffer,
  header: &mut DbHeaderV1,
  header_slot: &mut u32,
) -> Result<()> {
  let bytes = wal.cut_records_unsalted(pager)?;
  let page_size = header.page_size as u64;
  if !bytes.is_empty() {
    if header.wal_segments.entries.len() >= MAX_WAL_SEGMENTS {
      return Err(KiteError::InvalidWal(
        "the WAL segment table is full; cannot finish the background checkpoint's cut".to_string(),
      ));
    }
    let page_count = (bytes.len() as u64).div_ceil(page_size);
    let start_page = pager
      .file_size()
      .div_ceil(page_size)
      .max(live_end_page(header));
    let file_pages = pager.file_size().div_ceil(page_size);
    if start_page + page_count > file_pages {
      pager.allocate_pages((start_page + page_count - file_pages) as u32)?;
    }
    pager.write_range(start_page * page_size, &bytes)?;
    pager.sync()?;
    let mut entries = header.wal_segments.entries.to_vec();
    let seq = header.wal_segments.next_seq();
    entries.push(WalSegment {
      seq,
      start_page,
      page_count,
      byte_len: bytes.len() as u64,
      sealed: true,
    });
    header.wal_segments = WalSegmentTable {
      next_seq: seq + 1,
      covered: header.wal_segments.covered,
      entries: entries.into(),
    };
  }
  wal.reset();
  wal.store_in_header(header);
  header.checkpoint_in_progress = 0;
  for _ in 0..2 {
    header.change_counter += 1;
    let slot = crate::core::header::other_header_slot(*header_slot);
    crate::core::header::write_header_slot(pager, header, slot)?;
    pager.sync()?;
    *header_slot = slot;
  }
  Ok(())
}

/// One past the last page `header` names: the WAL, the snapshot, or a WAL
/// segment.
pub(crate) fn live_end_page(header: &DbHeaderV1) -> u64 {
  header
    .wal_segments
    .entries
    .iter()
    .map(WalSegment::end_page)
    .chain([
      header.wal_start_page + header.wal_page_count,
      header
        .snapshot_start_page
        .saturating_add(header.snapshot_page_count),
      header.db_size_pages,
    ])
    .max()
    .unwrap_or(0)
}
