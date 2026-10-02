//! Single-file compactor and vacuum operations.

#[cfg(test)]
use std::cell::Cell;

use std::sync::atomic::Ordering;

use crate::core::pager::{pages_to_store, FilePager};
use crate::core::snapshot::reader::{ParseSnapshotOptions, SnapshotData};
use crate::core::snapshot::writer::{build_snapshot_to_memory, SnapshotBuildInput};
use crate::core::wal::buffer::WalBuffer;
use crate::error::{KiteError, Result};
use crate::types::{DbHeaderV1, DeltaState};
use crate::util::compression::CompressionOptions;

use super::checkpoint::{snapshot_vector_stores, WrittenSnapshot};
use super::open::map_snapshot_range;
use super::SingleFileDB;

/// Options for single-file optimize operation
#[derive(Debug, Clone, Default)]
pub struct SingleFileOptimizeOptions {
  /// Compression options for the new snapshot
  pub compression: Option<CompressionOptions>,
}

/// Options for vacuum operation
#[derive(Debug, Clone)]
pub struct VacuumOptions {
  /// Shrink WAL region if empty
  pub shrink_wal: bool,
  /// Minimum WAL size to keep (bytes), rounded up to whole pages. Values
  /// below 16 pages (64 KiB at 4 KiB pages), the smallest WAL `resize_wal`
  /// accepts, are raised to it.
  pub min_wal_size: Option<u64>,
}

/// Options for resizing WAL region
#[derive(Debug, Clone)]
pub struct ResizeWalOptions {
  /// Allow shrinking WAL size (default false)
  pub allow_shrink: bool,
  /// Perform a checkpoint before resizing (default true)
  pub checkpoint: bool,
}

impl Default for ResizeWalOptions {
  fn default() -> Self {
    Self {
      allow_shrink: false,
      checkpoint: true,
    }
  }
}

impl Default for VacuumOptions {
  fn default() -> Self {
    Self {
      shrink_wal: true,
      min_wal_size: None,
    }
  }
}

/// Minimum WAL pages to keep (64KB at 4KB page size)
const MIN_WAL_PAGES: u64 = 16;

#[cfg(test)]
thread_local! {
  /// Steps vacuum or WAL resize may still pass on this thread before
  /// `compaction_step` injects a failure; `None` disarms it.
  static COMPACTION_TEST_FAULT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// A point between two steps of vacuum or WAL resize. Tests arm
/// `COMPACTION_TEST_FAULT` to fail at the nth such point on their thread.
fn compaction_step(step: &str) -> Result<()> {
  #[cfg(test)]
  {
    let fail = COMPACTION_TEST_FAULT.with(|fault| match fault.get() {
      Some(0) => {
        fault.set(None);
        true
      }
      Some(left) => {
        fault.set(Some(left - 1));
        false
      }
      None => false,
    });
    if fail {
      return Err(KiteError::Internal(format!(
        "injected compaction failure: {step}"
      )));
    }
  }

  let _ = step;
  Ok(())
}

/// Compaction rewrites every page below `end_page`, without gaps, as header,
/// WAL, and snapshot pages, so withdraw them all from reuse first: left on a
/// free list, they would take the next checkpoint's snapshot over the live WAL
/// or snapshot. Withdrawing before the first write also covers a failure
/// part-way, when a header slot may already name the new layout; leaking pages
/// until the next vacuum is safe where reusing them is not.
fn withdraw_compacted_pages(pager: &mut FilePager, end_page: u64) {
  pager.withdraw_free_pages(0, end_page as u32);
}

fn read_snapshot_pages(pager: &mut FilePager, start_page: u32, page_count: u32) -> Result<Vec<u8>> {
  let mut bytes = Vec::with_capacity(page_count as usize * pager.page_size());
  for page in 0..page_count {
    bytes.extend_from_slice(&pager.read_page(start_page + page)?);
  }
  Ok(bytes)
}

/// Point `header` at an empty WAL, as for a WAL just created at its size.
/// Region positions recorded for another WAL size would lie outside it.
fn reset_wal_positions(header: &mut DbHeaderV1) {
  header.wal_head = 0;
  header.wal_tail = 0;
  header.wal_primary_head = 0;
  header.wal_secondary_head = 0;
  header.active_wal_region = 0;
  header.checkpoint_in_progress = 0;
}

impl SingleFileDB {
  /// Optimize (compact) a single-file database.
  ///
  /// This merges snapshot + delta into a new snapshot and clears WAL.
  pub fn optimize_single_file(&self, options: Option<SingleFileOptimizeOptions>) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }

    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;

    let (nodes, edges, labels, etypes, propkeys, vector_stores) = self.collect_graph_data()?;
    let installed_vector_stores = snapshot_vector_stores(&vector_stores)?;

    let header = self.header.read().clone();
    let new_gen = header.active_snapshot_gen + 1;
    let compression = options.and_then(|o| o.compression);

    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      vector_stores: Some(vector_stores),
      compression,
    })?;

    let new_snapshot_start_page = self.snapshot_append_start_page(&header)?;
    let new_snapshot_page_count =
      pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;

    {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(
        &mut pager,
        new_snapshot_start_page as u32,
        &snapshot_buffer,
        header.page_size as usize,
      )?;
    }
    let snapshot = WrittenSnapshot {
      generation: new_gen,
      start_page: new_snapshot_start_page,
      page_count: new_snapshot_page_count,
    };
    let loaded = self.load_unnamed_snapshot(snapshot, installed_vector_stores)?;

    // The snapshot covers every WAL record, so the installed header names an
    // empty WAL. The previous snapshot is retired only after both durable
    // slots name the optimized one; checkpoint reuse may consume its pages.
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      self.install_snapshot(
        &mut pager,
        &mut wal_buffer,
        &mut header,
        snapshot,
        WalBuffer::reset,
      )?;
    }

    // The installed snapshot holds everything the delta did.
    self.install_loaded_snapshot(loaded, DeltaState::new());

    Ok(())
  }

  /// Vacuum operation - shrink file by reclaiming free pages.
  ///
  /// Moves the snapshot to directly after the WAL and truncates the file
  /// there; see `compact` for the crash protocol. With `shrink_wal`, an empty
  /// WAL also shrinks to `min_wal_size`.
  pub fn vacuum_single_file(&self, options: Option<VacuumOptions>) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }

    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;

    let options = options.unwrap_or_default();

    let header = self.header.read().clone();
    let page_size = header.page_size as u64;

    // Never below MIN_WAL_PAGES, the smallest WAL resize_wal accepts.
    let min_wal_pages = options.min_wal_size.map_or(MIN_WAL_PAGES, |min_wal_size| {
      min_wal_size.div_ceil(page_size).max(MIN_WAL_PAGES)
    });

    let wal_is_empty =
      header.wal_head == header.wal_tail || (header.wal_head == 0 && header.wal_tail == 0);
    let can_shrink_wal =
      options.shrink_wal && wal_is_empty && header.wal_page_count > min_wal_pages;

    if header.snapshot_page_count == 0 && !can_shrink_wal {
      return Ok(());
    }

    let mut layout = header;
    if can_shrink_wal {
      layout.wal_page_count = min_wal_pages;
      reset_wal_positions(&mut layout);
    }
    self.compact(layout)
  }

  /// Resize the WAL region (single-file only).
  ///
  /// This operation is offline (no active transactions). By default it
  /// checkpoints to clear WAL before resizing, under the same hold of the
  /// checkpoint gate, so no commit can refill the WAL in between. The
  /// snapshot moves to directly after the resized WAL; see `compact` for the
  /// crash protocol.
  pub fn resize_wal(&self, wal_size_bytes: usize, options: Option<ResizeWalOptions>) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }

    let options = options.unwrap_or_default();

    if wal_size_bytes == 0 {
      return Err(KiteError::Internal("WAL size must be > 0".to_string()));
    }

    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;
    if options.checkpoint {
      self.checkpoint_holding_gate()?;
    }

    let header = self.header.read().clone();
    let wal_is_empty =
      header.wal_head == header.wal_tail || (header.wal_head == 0 && header.wal_tail == 0);
    if !wal_is_empty {
      return Err(KiteError::Internal(
        "WAL must be empty before resize (run checkpoint)".to_string(),
      ));
    }

    let new_wal_page_count = pages_to_store(wal_size_bytes, header.page_size as usize) as u64;

    if new_wal_page_count < MIN_WAL_PAGES {
      return Err(KiteError::Internal(format!(
        "WAL size too small: minimum is {MIN_WAL_PAGES} pages"
      )));
    }

    if new_wal_page_count < header.wal_page_count && !options.allow_shrink {
      return Err(KiteError::Internal(
        "WAL shrink requires allow_shrink=true".to_string(),
      ));
    }

    if new_wal_page_count == header.wal_page_count {
      return Ok(());
    }

    let mut layout = header;
    layout.wal_page_count = new_wal_page_count;
    reset_wal_positions(&mut layout);
    self.compact(layout)
  }

  /// Install `layout`'s WAL with the snapshot directly after it, and truncate
  /// the file after the snapshot. Callers hold the exclusive checkpoint gate;
  /// `layout` is the installed header with any WAL change applied (a changed
  /// WAL must be empty).
  ///
  /// No step overwrites or truncates a page that a durable header slot may
  /// still name:
  ///
  /// 1. If the snapshot moves, copy it past every page the installed header
  ///    or `layout` names (the bridge copy), then install `layout` with the
  ///    snapshot at the bridge copy in both slots. Now no slot names the old
  ///    snapshot or any page of the old WAL outside the new one.
  /// 2. Copy the snapshot to its compacted place after the WAL, which no slot
  ///    names.
  /// 3. Install the compacted `layout` in both slots. Now no slot names the
  ///    bridge copy or any page after the compacted snapshot.
  /// 4. Truncate the file after the compacted snapshot.
  ///
  /// A failure leaves memory on the last layout installed in both slots (see
  /// `install_compacted_layout`). Pages a slot may name for the failed step
  /// lie on no free list, so nothing reuses them before a later vacuum.
  fn compact(&self, mut layout: DbHeaderV1) -> Result<()> {
    // The header installed below names the WAL as recorded in memory, so its
    // bytes must be durable first; the buffer rebuilt from it starts clean.
    {
      let mut pager = self.pager.lock();
      self.wal_buffer.lock().flush(&mut pager)?;
      pager.sync()?;
    }

    let installed = self.header.read().clone();
    let page_size = installed.page_size as usize;
    let snapshot_page_count = layout.snapshot_page_count;
    let wal_end_page = layout.wal_start_page + layout.wal_page_count;
    withdraw_compacted_pages(&mut self.pager.lock(), wal_end_page + snapshot_page_count);

    if snapshot_page_count > 0 && installed.snapshot_start_page != wal_end_page {
      let snapshot_bytes = read_snapshot_pages(
        &mut self.pager.lock(),
        installed.snapshot_start_page as u32,
        snapshot_page_count as u32,
      )?;

      // Past the file's end and past the compacted snapshot, so it overlaps
      // neither the installed pages nor anything `layout` names.
      let bridge_start_page = self
        .snapshot_append_start_page(&installed)?
        .max(wal_end_page + snapshot_page_count);
      self.write_snapshot_pages(
        &mut self.pager.lock(),
        bridge_start_page as u32,
        &snapshot_bytes,
        page_size,
      )?;
      compaction_step("bridge copy written")?;
      layout.snapshot_start_page = bridge_start_page;
      layout.db_size_pages = bridge_start_page + snapshot_page_count;
      self.install_compacted_layout(&mut layout)?;

      self.write_snapshot_pages(
        &mut self.pager.lock(),
        wal_end_page as u32,
        &snapshot_bytes,
        page_size,
      )?;
      compaction_step("compacted copy written")?;
    }

    if snapshot_page_count > 0 {
      layout.snapshot_start_page = wal_end_page;
    }
    layout.db_size_pages = wal_end_page + snapshot_page_count;
    self.install_compacted_layout(&mut layout)?;
    compaction_step("compacted layout installed")?;

    let mut pager = self.pager.lock();
    let file_pages = pager.file_size().div_ceil(page_size as u64);
    if file_pages > layout.db_size_pages {
      pager.truncate_pages(layout.db_size_pages as u32)?;
    }
    Ok(())
  }

  /// Install `layout` durably in both header slots, then make it the
  /// in-memory header, WAL, and snapshot.
  ///
  /// One slot is synced before the other is written, so a crash always finds
  /// a durable slot naming either the previous layout or `layout`. Until both
  /// are durable, memory keeps the previous layout (with the newest change
  /// counter, so later header writes still outrank every slot on disk), and
  /// the next header write goes to the slot this install did not make
  /// durable.
  fn install_compacted_layout(&self, layout: &mut DbHeaderV1) -> Result<()> {
    // Map the copy `layout` names first, so nothing can fail once it is
    // installed. Its bytes match the mapped snapshot's, so the vector stores,
    // which hold commits since the last checkpoint, and their lazy entries
    // (offsets into the snapshot) stay valid.
    let snapshot = if layout.snapshot_page_count > 0 {
      let pager = self.pager.lock();
      Some(SnapshotData::parse(
        map_snapshot_range(&pager, layout)?,
        &ParseSnapshotOptions::default(),
      )?)
    } else {
      None
    };

    let persisted = {
      let mut pager = self.pager.lock();
      self
        .persist_compacted_header_slot(&mut pager, layout)
        .and_then(|()| compaction_step("one header slot durable"))
        .and_then(|()| self.persist_compacted_header_slot(&mut pager, layout))
    };
    if let Err(error) = persisted {
      let mut header = self.header.write();
      header.change_counter = header.change_counter.max(layout.change_counter);
      return Err(error);
    }

    *self.header.write() = layout.clone();
    *self.wal_buffer.lock() = WalBuffer::from_header(layout);
    *self.snapshot.write() = snapshot;
    Ok(())
  }

  /// Write `header` to the next header slot and sync it. On failure the
  /// slot may hold a torn or unsynced copy, so the next header write targets
  /// it again and the other slot stays the crash fallback.
  fn persist_compacted_header_slot(
    &self,
    pager: &mut FilePager,
    header: &mut DbHeaderV1,
  ) -> Result<()> {
    let durable_slot = self.header_slot.load(Ordering::Acquire);
    let persisted = self
      .persist_header(pager, header, false)
      .and_then(|()| compaction_step("header slot written"))
      .and_then(|()| pager.sync());
    if persisted.is_err() {
      self.header_slot.store(durable_slot, Ordering::Release);
    }
    persisted
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::constants::DEFAULT_PAGE_SIZE;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use std::path::Path;
  use tempfile::tempdir;

  #[test]
  fn test_resize_wal_grow_reopen() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("resize-wal.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new().wal_size(64 * 1024))?;
    db.begin(false)?;
    db.create_node(Some("a"))?;
    db.commit()?;

    db.resize_wal(1024 * 1024, None)?;
    close_single_file(db)?;

    let reopened = open_single_file(&db_path, SingleFileOpenOptions::new().wal_size(1024 * 1024))?;
    assert!(reopened.node_by_key("a").is_some());
    close_single_file(reopened)?;

    Ok(())
  }

  #[test]
  fn test_vacuum_and_resize_relocate_snapshot_safely() -> Result<()> {
    let temp_dir = tempdir()?;
    let db_path = temp_dir.path().join("relocate-snapshot.kitedb");
    let small_wal = 4 * 1024 * 1024;
    let options = SingleFileOpenOptions::new()
      .wal_size(small_wal)
      .auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone())?;

    db.begin(false)?;
    for index in 0..64 {
      db.create_node(Some(&format!("node-{index}")))?;
    }
    db.commit()?;
    db.checkpoint()?;

    db.vacuum_single_file(None)?;
    assert!(db.node_by_key("node-63").is_some());

    close_single_file(db)?;

    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new()
        .wal_size(MIN_WAL_PAGES as usize * crate::constants::DEFAULT_PAGE_SIZE)
        .auto_checkpoint(false),
    )?;
    db.resize_wal(
      small_wal,
      Some(ResizeWalOptions {
        checkpoint: false,
        ..Default::default()
      }),
    )?;
    assert!(db.node_by_key("node-0").is_some());
    close_single_file(db)?;

    let reopened = open_single_file(&db_path, options)?;
    assert!(reopened.node_by_key("node-63").is_some());
    close_single_file(reopened)?;

    Ok(())
  }

  const MIB: usize = 1024 * 1024;

  fn test_options(wal_size: usize) -> SingleFileOpenOptions {
    SingleFileOpenOptions::new()
      .wal_size(wal_size)
      .auto_checkpoint(false)
      .background_checkpoint(false)
  }

  fn commit_node(db: &SingleFileDB, key: &str) {
    db.begin(false).expect("begin");
    db.create_node(Some(key)).expect("create node");
    db.commit().expect("commit");
  }

  /// A database and the keys it must (and must not) hold.
  struct Fixture {
    db: SingleFileDB,
    present: Vec<String>,
    absent: Vec<String>,
    /// Present keys committed after the last checkpoint. Only the newest
    /// header slot may name them, so a copy with that slot torn may lose them
    /// until a later operation writes both slots.
    wal_only: Vec<String>,
  }

  /// One large snapshot, installed directly after the WAL.
  fn fresh_snapshot_db(path: &Path, wal_size: usize) -> Fixture {
    let db = open_single_file(path, test_options(wal_size)).expect("open");
    db.begin(false).expect("begin");
    for index in 0..2000 {
      db.create_node(Some(&format!("bulk-{index}")))
        .expect("create node");
    }
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    Fixture {
      db,
      present: vec!["bulk-0".into(), "bulk-1999".into()],
      absent: Vec::new(),
      wal_only: Vec::new(),
    }
  }

  /// A large snapshot replaced by a small one at the end of the file, so
  /// vacuum and resize move the snapshot via their temporary copy.
  fn relocatable_snapshot_db(path: &Path, wal_size: usize) -> Fixture {
    let Fixture { db, .. } = fresh_snapshot_db(path, wal_size);
    db.begin(false).expect("begin");
    for index in 10..2000 {
      let id = db.node_by_key(&format!("bulk-{index}")).expect("bulk node");
      db.delete_node(id).expect("delete node");
    }
    db.commit().expect("commit");
    db.checkpoint().expect("checkpoint");
    Fixture {
      db,
      present: vec!["bulk-0".into(), "bulk-9".into()],
      absent: vec!["bulk-10".into(), "bulk-1999".into()],
      wal_only: Vec::new(),
    }
  }

  fn assert_keys(db: &SingleFileDB, fixture: &Fixture, context: &str) {
    assert_keys_except_wal_only(db, fixture, context);
    for key in &fixture.wal_only {
      assert!(db.node_by_key(key).is_some(), "{context}: {key} missing");
    }
  }

  fn assert_keys_except_wal_only(db: &SingleFileDB, fixture: &Fixture, context: &str) {
    for key in &fixture.present {
      assert!(db.node_by_key(key).is_some(), "{context}: {key} missing");
    }
    for key in &fixture.absent {
      assert!(
        db.node_by_key(key).is_none(),
        "{context}: {key} resurrected"
      );
    }
  }

  /// The in-memory header names a readable snapshot and the WAL the buffer
  /// writes to, and no free or deferred page lies in either.
  fn assert_memory_consistent(db: &SingleFileDB, context: &str) {
    let header = db.header.read().clone();
    let pager = db.pager.lock();
    if header.snapshot_page_count > 0 {
      let mapped = map_snapshot_range(&pager, &header)
        .unwrap_or_else(|error| panic!("{context}: header names unmappable pages: {error}"));
      SnapshotData::parse(mapped, &ParseSnapshotOptions::default())
        .unwrap_or_else(|error| panic!("{context}: header names an unreadable snapshot: {error}"));
    }
    {
      let wal = db.wal_buffer.lock();
      assert_eq!(
        wal.capacity(),
        header.wal_page_count * u64::from(header.page_size),
        "{context}: WAL buffer and header disagree on the WAL size"
      );
      assert!(
        wal.head() <= wal.capacity() && wal.secondary_head() <= wal.capacity(),
        "{context}: WAL positions lie outside the WAL"
      );
    }
    let wal_end = header.wal_start_page + header.wal_page_count;
    let snapshot =
      header.snapshot_start_page..header.snapshot_start_page + header.snapshot_page_count;
    for page in pager
      .free_page_list()
      .into_iter()
      .chain(pager.deferred_free_page_list())
      .map(u64::from)
    {
      assert!(
        page >= wal_end && !snapshot.contains(&page),
        "{context}: live page {page} is listed as free"
      );
    }
  }

  /// Open a copy of the file as a crash now would leave it, with header slot
  /// `torn` corrupted, and check its keys; with `both_slots_current`, also
  /// those only the newest slot could name before. The copy opens with the
  /// WAL size of the slot open will select.
  fn assert_crash_copy_opens(
    db_path: &Path,
    torn: Option<usize>,
    both_slots_current: bool,
    fixture: &Fixture,
    context: &str,
  ) {
    let page_size = DEFAULT_PAGE_SIZE;
    let mut bytes = std::fs::read(db_path).expect("read database file");
    if let Some(slot) = torn {
      bytes[slot * page_size + 32] ^= 0xFF;
    }
    let selected = (0..2)
      .filter_map(|slot| {
        DbHeaderV1::parse(&bytes[slot * page_size..(slot + 1) * page_size])
          .ok()
          .map(|header| (header, slot))
      })
      .max_by_key(|(header, slot)| (header.change_counter, *slot == 0))
      .map(|(header, _)| header)
      .unwrap_or_else(|| panic!("{context}: no valid header slot with slot {torn:?} torn"));
    let copy_path = db_path.with_extension("crash.kitedb");
    std::fs::write(&copy_path, &bytes).expect("write crash copy");

    let wal_size = selected.wal_page_count as usize * page_size;
    let copy = open_single_file(&copy_path, test_options(wal_size)).unwrap_or_else(|error| {
      panic!("{context}: crash copy with slot {torn:?} torn does not open: {error}")
    });
    let context = format!("{context}, crash copy with slot {torn:?} torn");
    if torn.is_none() || both_slots_current {
      assert_keys(&copy, fixture, &context);
    } else {
      assert_keys_except_wal_only(&copy, fixture, &context);
    }
    drop(copy);
    std::fs::remove_file(&copy_path).expect("remove crash copy");
  }

  fn assert_crash_copies_open(
    db_path: &Path,
    both_slots_current: bool,
    fixture: &Fixture,
    context: &str,
  ) {
    for torn in [None, Some(0), Some(1)] {
      assert_crash_copy_opens(db_path, torn, both_slots_current, fixture, context);
    }
  }

  fn arm_compaction_fault(steps: Option<usize>) {
    COMPACTION_TEST_FAULT.with(|fault| fault.set(steps));
  }

  fn compaction_fault_armed() -> bool {
    COMPACTION_TEST_FAULT.with(|fault| fault.get().is_some())
  }

  /// Run `operation` with a failure injected at its first step, then its
  /// second, and so on until it completes. After each failure memory must
  /// still match a durable state: the snapshot readable, the header naming
  /// it and the WAL in use. A crash at that moment, even with either header
  /// slot torn, must open on the same data, and the database must keep
  /// working. After the complete run both header slots must name the result.
  fn assert_every_step_fails_safely(
    name: &str,
    setup: impl Fn(&Path) -> Fixture,
    operation: impl Fn(&SingleFileDB) -> Result<()>,
  ) {
    for step in 0.. {
      assert!(step < 64, "{name}: never completed");
      let temp_dir = tempdir().expect("temp dir");
      let db_path = temp_dir.path().join("compaction-fault.kitedb");
      let mut fixture = setup(&db_path);
      let context = format!("{name}, failure at step {step}");

      arm_compaction_fault(Some(step));
      let result = operation(&fixture.db);
      let failed = !compaction_fault_armed();
      arm_compaction_fault(None);

      if failed {
        let Err(error) = result else {
          panic!("{context}: injected failure was swallowed");
        };
        assert!(
          error.to_string().contains("injected compaction failure"),
          "{context}: unexpected error {error}"
        );
      } else {
        result.unwrap_or_else(|error| panic!("{name}: failed without a fault: {error}"));
      }
      assert_keys(&fixture.db, &fixture, &context);
      assert_memory_consistent(&fixture.db, &context);
      // A completed operation has written both slots.
      assert_crash_copies_open(&db_path, !failed, &fixture, &context);

      // The database keeps working, and the operation succeeds when retried.
      commit_node(&fixture.db, "after-operation");
      fixture.db.checkpoint().expect("checkpoint after operation");
      operation(&fixture.db).unwrap_or_else(|error| panic!("{context}: retry failed: {error}"));
      fixture.present.push("after-operation".into());
      assert_keys(&fixture.db, &fixture, &format!("{context}, after retry"));
      assert_memory_consistent(&fixture.db, &format!("{context}, after retry"));
      assert_crash_copies_open(&db_path, true, &fixture, &format!("{context}, after retry"));

      let wal_size = {
        let header = fixture.db.header.read();
        header.wal_page_count as usize * header.page_size as usize
      };
      let Fixture {
        db,
        present,
        absent,
        wal_only,
      } = fixture;
      drop(db);
      let reopened = open_single_file(&db_path, test_options(wal_size))
        .unwrap_or_else(|error| panic!("{context}: reopen failed: {error}"));
      let fixture = Fixture {
        db: reopened,
        present,
        absent,
        wal_only,
      };
      assert_keys(&fixture.db, &fixture, &format!("{context}, reopened"));

      if !failed {
        return;
      }
    }
  }

  fn keep_wal() -> VacuumOptions {
    VacuumOptions {
      shrink_wal: false,
      min_wal_size: None,
    }
  }

  fn resize_to(wal_size: usize) -> impl Fn(&SingleFileDB) -> Result<()> {
    move |db| {
      db.resize_wal(
        wal_size,
        Some(ResizeWalOptions {
          allow_shrink: true,
          checkpoint: false,
        }),
      )
    }
  }

  /// Regression: a failure after vacuum's first header write left the
  /// in-memory snapshot unset, so reads saw an empty database until reopen;
  /// the slot naming its temporary copy also outlived the truncation of that
  /// copy.
  #[test]
  fn vacuum_failing_at_any_step_leaves_memory_matching_disk() {
    assert_every_step_fails_safely(
      "vacuum keeping the WAL",
      |path| {
        let mut fixture = relocatable_snapshot_db(path, MIB);
        commit_node(&fixture.db, "in-wal");
        fixture.wal_only.push("in-wal".into());
        fixture
      },
      |db| db.vacuum_single_file(Some(keep_wal())),
    );
    assert_every_step_fails_safely(
      "vacuum shrinking the WAL",
      |path| relocatable_snapshot_db(path, MIB),
      |db| db.vacuum_single_file(None),
    );
  }

  /// Regression: as for vacuum; also, growing the WAL by less than the
  /// snapshot's size wrote the compacted copy over the temporary copy the
  /// installed header named.
  #[test]
  fn resize_wal_failing_at_any_step_leaves_memory_matching_disk() {
    assert_every_step_fails_safely(
      "resize growing the WAL",
      |path| relocatable_snapshot_db(path, MIB),
      resize_to(2 * MIB),
    );
    assert_every_step_fails_safely(
      "resize shrinking the WAL",
      |path| relocatable_snapshot_db(path, MIB),
      resize_to(MIB / 8),
    );
    assert_every_step_fails_safely(
      "resize growing the WAL by one page",
      |path| fresh_snapshot_db(path, MIB),
      resize_to(MIB + DEFAULT_PAGE_SIZE),
    );
  }

  /// After a WAL resize both header slots must name the new layout.
  /// Regression: the older slot still named the temporary snapshot copy, at
  /// the old WAL size, after resize truncated that copy; tearing the newest
  /// slot left the database unopenable.
  #[test]
  fn resize_wal_survives_a_torn_newest_header_slot() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("resize-torn-header.kitedb");
    let fixture = fresh_snapshot_db(&db_path, MIB);
    resize_to(2 * MIB)(&fixture.db).expect("resize WAL");
    let newest_slot = fixture.db.header_slot.load(Ordering::Acquire) as usize;
    let Fixture {
      db,
      present,
      absent,
      wal_only,
    } = fixture;
    drop(db);

    let mut bytes = std::fs::read(&db_path).expect("read database file");
    bytes[newest_slot * DEFAULT_PAGE_SIZE + 32] ^= 0xFF;
    std::fs::write(&db_path, &bytes).expect("tear newest header slot");

    let reopened = open_single_file(&db_path, test_options(2 * MIB))
      .expect("reopen with the newest header slot torn");
    let fixture = Fixture {
      db: reopened,
      present,
      absent,
      wal_only,
    };
    assert_keys(&fixture.db, &fixture, "after resize with newest slot torn");
  }

  /// Vacuum only moves the snapshot, so in-memory state built since the last
  /// checkpoint must survive it. Regression: vacuum rebuilt the vector stores
  /// from the snapshot alone, dropping vectors committed since the checkpoint
  /// until reopen.
  #[test]
  fn vacuum_keeps_vectors_committed_since_the_last_checkpoint() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("vacuum-wal-vectors.kitedb");
    let Fixture { db, .. } = relocatable_snapshot_db(&db_path, MIB);
    db.begin(false).expect("begin");
    let embedding = db.define_propkey("embedding").expect("define propkey");
    let node = db.create_node(Some("with-vector")).expect("create node");
    db.set_node_vector(node, embedding, &[1.0, 2.0, 3.0])
      .expect("set vector");
    db.commit().expect("commit");

    let before = db
      .node_vector(node, embedding)
      .expect("vector before vacuum");

    db.vacuum_single_file(Some(keep_wal())).expect("vacuum");
    assert_eq!(
      db.node_vector(node, embedding),
      Some(before),
      "vacuum dropped a vector committed since the last checkpoint"
    );
  }

  /// `resize_wal` with `checkpoint: true` must checkpoint and resize as one
  /// step. Regression: it checkpointed, released the gate, then took it again
  /// to resize, so a concurrent commit in between failed the resize with
  /// "WAL must be empty before resize".
  #[test]
  fn resize_wal_with_checkpoint_succeeds_under_concurrent_writes() {
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Arc;

    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("resize-concurrent.kitedb");
    let small = 64 * 1024;
    let large = 128 * 1024;
    let db = Arc::new(
      open_single_file(
        &db_path,
        SingleFileOpenOptions::new()
          .wal_size(small)
          .auto_checkpoint(false),
      )
      .expect("open"),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
      let db = Arc::clone(&db);
      let stop = Arc::clone(&stop);
      std::thread::spawn(move || {
        let mut index = 0u64;
        while !stop.load(AtomicOrdering::Relaxed) {
          db.begin(false).expect("writer begin");
          db.create_node(Some(&format!("w-{index}")))
            .expect("writer create");
          db.commit().expect("writer commit");
          index += 1;
        }
        index
      })
    };

    let mut failures = Vec::new();
    for attempt in 0..30 {
      let target = if attempt % 2 == 0 { large } else { small };
      if let Err(error) = db.resize_wal(
        target,
        Some(ResizeWalOptions {
          allow_shrink: true,
          checkpoint: true,
        }),
      ) {
        failures.push(format!("resize #{attempt} to {target}: {error}"));
      }
    }
    stop.store(true, AtomicOrdering::Relaxed);
    let committed = writer.join().expect("writer thread");

    assert!(
      failures.is_empty(),
      "resize_wal(checkpoint: true) failed under concurrent writes ({} of 30, {committed} writer commits): {:?}",
      failures.len(),
      failures
    );
    assert!(db.node_by_key("w-0").is_some());
  }

  /// Compaction waits under the gate for open transactions to finish, so a
  /// caller inside its own transaction is refused rather than left waiting
  /// for itself.
  #[test]
  fn compaction_refuses_callers_open_transaction() {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("compaction-own-tx.kitedb");
    let db = open_single_file(&db_path, test_options(MIB)).expect("open");
    db.begin(false).expect("begin");
    db.create_node(Some("mine")).expect("create node");

    for checkpoint in [true, false] {
      let result = db.resize_wal(
        2 * MIB,
        Some(ResizeWalOptions {
          allow_shrink: false,
          checkpoint,
        }),
      );
      assert!(
        matches!(result, Err(KiteError::TransactionInProgress)),
        "resize_wal(checkpoint: {checkpoint}) with an open transaction: {result:?}"
      );
    }
    assert!(matches!(
      db.vacuum_single_file(None),
      Err(KiteError::TransactionInProgress)
    ));
    assert!(matches!(
      db.optimize_single_file(None),
      Err(KiteError::TransactionInProgress)
    ));

    db.commit().expect("commit");
    db.resize_wal(2 * MIB, None).expect("resize after commit");
    assert!(db.node_by_key("mine").is_some());
  }
}
