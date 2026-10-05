//! The database header in memory (`SingleFileInner::header`), and the one
//! thing the automatic checkpoint's check needs of it, kept beside its lock.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::core::wal::buffer::wal_region_bytes;
use crate::types::DbHeaderV1;

/// When an automatic checkpoint starts: the open options
/// `checkpoint_log_ratio` and `checkpoint_log_budget` (see `bytes`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LogTrigger {
  /// The fraction of the snapshot's size the log may reach.
  pub(crate) ratio: f64,
  /// The most log a checkpoint waits for.
  pub(crate) budget: u64,
}

impl LogTrigger {
  /// Bytes of log (WAL segments and WAL) at which a checkpoint starts: the
  /// `ratio` of the snapshot's size, at least half the WAL's region (three
  /// eighths of the WAL: where earlier releases checkpointed by default, so
  /// a small database holds no more log, and no larger delta, than it did
  /// with them), and at most the log budget, which bounds the memory the
  /// delta replaying the log takes (about ten times its size) on a large
  /// database. A budget too large to compute with saturates: no cap.
  pub(crate) fn bytes(&self, header: &DbHeaderV1) -> u64 {
    let page_size = header.page_size as u64;
    let floor = (wal_region_bytes(header) / 2).min(self.budget);
    let snapshot = header.snapshot_page_count.saturating_mul(page_size);
    // Saturates at u64::MAX.
    let wanted = (self.ratio * snapshot as f64) as u64;
    wanted.clamp(floor, self.budget.max(floor))
  }

  /// Bytes the WAL may hold before the log reaches the trigger (at least
  /// one byte of log does): the trigger less the bytes of the WAL segments
  /// the snapshot does not cover, or 0 if those reach it already. Segments a
  /// checkpoint kept for a transaction still open do not count:
  /// checkpointing again would keep them again, and the delta holds nothing
  /// of them.
  fn wal_headroom(&self, header: &DbHeaderV1) -> u64 {
    let table = &header.wal_segments;
    let segments: u64 = table
      .entries
      .iter()
      .filter(|segment| segment.seq > table.covered)
      .map(|segment| segment.byte_len)
      .sum();
    self.bytes(header).max(1).saturating_sub(segments)
  }
}

/// The header in memory: a read-write lock, and the WAL's headroom below
/// the checkpoint trigger (`LogTrigger::wal_headroom`), which every write
/// of the header keeps up to date as it ends (`HeaderWriteGuard`). The
/// automatic checkpoint's check after every commit and rollback reads the
/// headroom, not the header (`SingleFileDB::log_reached_trigger`): a commit
/// group's leader holds the header's write lock across its header write
/// and, in Full mode, the group's sync, and a check that waited for it
/// would make every commit wait for the next group's sync, and the leader
/// for the checks.
pub(crate) struct HeaderCell {
  header: RwLock<DbHeaderV1>,
  trigger: LogTrigger,
  wal_headroom: AtomicU64,
}

impl HeaderCell {
  pub(crate) fn new(header: DbHeaderV1, trigger: LogTrigger) -> Self {
    let wal_headroom = AtomicU64::new(trigger.wal_headroom(&header));
    Self {
      header: RwLock::new(header),
      trigger,
      wal_headroom,
    }
  }

  pub(crate) fn read(&self) -> RwLockReadGuard<'_, DbHeaderV1> {
    self.header.read()
  }

  /// The header, to change: the WAL's headroom follows when the guard is
  /// dropped, before the lock is released.
  pub(crate) fn write(&self) -> HeaderWriteGuard<'_> {
    HeaderWriteGuard {
      header: self.header.write(),
      cell: self,
    }
  }

  pub(crate) fn trigger(&self) -> LogTrigger {
    self.trigger
  }

  /// Bytes the WAL may hold before the log reaches the checkpoint trigger,
  /// as of the header's last write; without its lock.
  pub(crate) fn wal_headroom(&self) -> u64 {
    self.wal_headroom.load(Ordering::Acquire)
  }
}

/// The header's write lock, held (see `HeaderCell::write`).
pub(crate) struct HeaderWriteGuard<'a> {
  header: RwLockWriteGuard<'a, DbHeaderV1>,
  cell: &'a HeaderCell,
}

impl Deref for HeaderWriteGuard<'_> {
  type Target = DbHeaderV1;

  fn deref(&self) -> &DbHeaderV1 {
    &self.header
  }
}

impl DerefMut for HeaderWriteGuard<'_> {
  fn deref_mut(&mut self) -> &mut DbHeaderV1 {
    &mut self.header
  }
}

impl Drop for HeaderWriteGuard<'_> {
  fn drop(&mut self) {
    let headroom = self.cell.trigger.wal_headroom(&self.header);
    self.cell.wal_headroom.store(headroom, Ordering::Release);
  }
}
