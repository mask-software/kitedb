//! Single-file compactor and vacuum operations.

use crate::core::pager::pages_to_store;
use crate::core::snapshot::writer::{build_snapshot_to_memory, SnapshotBuildInput};
use crate::core::wal::buffer::WalBuffer;
use crate::error::{KiteError, Result};
use crate::util::compression::CompressionOptions;

use super::checkpoint::WrittenSnapshot;
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
  /// Minimum WAL size to keep (bytes)
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

/// Compaction rewrites every page below `end_page`, without gaps, as header,
/// WAL, and snapshot pages, so withdraw them all from reuse first: left on a
/// free list, they would take the next checkpoint's snapshot over the live WAL
/// or snapshot. Withdrawing before the first write also covers a failure
/// part-way, when a header slot may already name the new layout; leaking pages
/// until the next vacuum is safe where reusing them is not.
fn withdraw_compacted_pages(pager: &mut crate::core::pager::FilePager, end_page: u64) {
  pager.withdraw_free_pages(0, end_page as u32);
}

fn read_snapshot_pages(
  pager: &mut crate::core::pager::FilePager,
  start_page: u32,
  page_count: u32,
) -> Result<Vec<u8>> {
  let mut bytes = Vec::with_capacity(page_count as usize * pager.page_size());
  for page in 0..page_count {
    bytes.extend_from_slice(&pager.read_page(start_page + page)?);
  }
  Ok(bytes)
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
        WrittenSnapshot {
          generation: new_gen,
          start_page: new_snapshot_start_page,
          page_count: new_snapshot_page_count,
        },
        WalBuffer::reset,
      )?;
    }

    self.delta.write().clear();
    self.reload_snapshot()?;

    Ok(())
  }

  /// Vacuum operation - shrink file by reclaiming free pages.
  pub fn vacuum_single_file(&self, options: Option<VacuumOptions>) -> Result<()> {
    if self.read_only {
      return Err(KiteError::ReadOnly);
    }

    if self.current_tx_handle().is_some() {
      return Err(KiteError::TransactionInProgress);
    }

    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;

    let options = options.unwrap_or_default();

    let mut new_header = self.header.read().clone();
    let page_size = new_header.page_size as u64;

    let min_wal_pages = if let Some(min_wal_size) = options.min_wal_size {
      min_wal_size.div_ceil(page_size)
    } else {
      MIN_WAL_PAGES
    };

    let wal_is_empty = new_header.wal_head == new_header.wal_tail
      || (new_header.wal_head == 0 && new_header.wal_tail == 0);
    let can_shrink_wal =
      options.shrink_wal && wal_is_empty && new_header.wal_page_count > min_wal_pages;

    if new_header.snapshot_page_count == 0 && !can_shrink_wal {
      return Ok(());
    }

    let new_wal_page_count = if can_shrink_wal {
      min_wal_pages
    } else {
      new_header.wal_page_count
    };
    let new_wal_end_page = new_header.wal_start_page + new_wal_page_count;

    let current_snapshot_start = new_header.snapshot_start_page;
    let snapshot_page_count = new_header.snapshot_page_count;
    let snapshot_relocation_needed =
      snapshot_page_count > 0 && current_snapshot_start != new_wal_end_page;
    withdraw_compacted_pages(
      &mut self.pager.lock(),
      new_wal_end_page + snapshot_page_count,
    );
    let snapshot_bytes = if snapshot_relocation_needed {
      let mut pager = self.pager.lock();
      Some(read_snapshot_pages(
        &mut pager,
        current_snapshot_start as u32,
        snapshot_page_count as u32,
      )?)
    } else {
      None
    };

    // A compacted location may overlap the currently installed snapshot.
    // First copy to append-only pages and install that copy durably; only then
    // may the old mapping and pages be reclaimed or overwritten.
    if let Some(snapshot_bytes) = snapshot_bytes.as_deref() {
      let append_start = self.snapshot_append_start_page(&new_header)?;
      {
        let mut pager = self.pager.lock();
        self.write_snapshot_pages(
          &mut pager,
          append_start as u32,
          snapshot_bytes,
          new_header.page_size as usize,
        )?;
      }

      new_header.snapshot_start_page = append_start;
      new_header.db_size_pages = append_start + snapshot_page_count;
      {
        let mut pager = self.pager.lock();
        self.persist_header(&mut pager, &mut new_header, true)?;
      }
      *self.header.write() = new_header.clone();
      *self.snapshot.write() = None;

      {
        let mut pager = self.pager.lock();
        self.write_snapshot_pages(
          &mut pager,
          new_wal_end_page as u32,
          snapshot_bytes,
          new_header.page_size as usize,
        )?;
      }
      new_header.snapshot_start_page = new_wal_end_page;
    } else if snapshot_page_count > 0 {
      new_header.snapshot_start_page = new_wal_end_page;
    }

    if can_shrink_wal {
      new_header.wal_page_count = new_wal_page_count;
    }

    new_header.db_size_pages = if new_header.snapshot_page_count > 0 {
      new_header.snapshot_start_page + new_header.snapshot_page_count
    } else {
      new_header.wal_start_page + new_header.wal_page_count
    };
    if snapshot_bytes.is_none() {
      let mut pager = self.pager.lock();
      self.persist_header(&mut pager, &mut new_header, true)?;
      if new_header.snapshot_page_count > 0 {
        *self.snapshot.write() = None;
      }
      pager.truncate_pages(new_header.db_size_pages as u32)?;
    } else {
      // The append-only header was already durable. Install the compacted
      // location before truncating the now-unreachable append-only copy.
      let mut pager = self.pager.lock();
      self.persist_header(&mut pager, &mut new_header, true)?;
      pager.truncate_pages(new_header.db_size_pages as u32)?;
    }

    let new_wal_buffer = WalBuffer::from_header(&new_header);

    {
      let mut header_guard = self.header.write();
      *header_guard = new_header;
    }

    {
      let mut wal_buffer = self.wal_buffer.lock();
      *wal_buffer = new_wal_buffer;
    }

    self.reload_snapshot()?;

    Ok(())
  }

  /// Resize the WAL region (single-file only).
  ///
  /// This operation is offline (no active transactions). By default it
  /// checkpoints to clear WAL before resizing.
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

    if options.checkpoint {
      self.checkpoint()?;
    }

    let _checkpoint_gate = self.exclusive_checkpoint_gate()?;

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

    let mut new_header = header.clone();
    let new_wal_end_page = new_header.wal_start_page + new_wal_page_count;

    let current_snapshot_start = new_header.snapshot_start_page;
    let snapshot_page_count = new_header.snapshot_page_count;
    let snapshot_relocation_needed =
      snapshot_page_count > 0 && current_snapshot_start != new_wal_end_page;
    withdraw_compacted_pages(
      &mut self.pager.lock(),
      new_wal_end_page + snapshot_page_count,
    );
    let snapshot_bytes = if snapshot_relocation_needed {
      let mut pager = self.pager.lock();
      Some(read_snapshot_pages(
        &mut pager,
        current_snapshot_start as u32,
        snapshot_page_count as u32,
      )?)
    } else {
      None
    };

    // Resizing can move the compacted snapshot into the currently mapped
    // range. Use an installed append-only copy as the crash-safe bridge.
    if let Some(snapshot_bytes) = snapshot_bytes.as_deref() {
      let append_start = self.snapshot_append_start_page(&new_header)?;
      {
        let mut pager = self.pager.lock();
        self.write_snapshot_pages(
          &mut pager,
          append_start as u32,
          snapshot_bytes,
          new_header.page_size as usize,
        )?;
      }
      new_header.snapshot_start_page = append_start;
      new_header.db_size_pages = append_start + snapshot_page_count;
      {
        let mut pager = self.pager.lock();
        self.persist_header(&mut pager, &mut new_header, true)?;
      }
      *self.header.write() = new_header.clone();
      *self.snapshot.write() = None;

      {
        let mut pager = self.pager.lock();
        self.write_snapshot_pages(
          &mut pager,
          new_wal_end_page as u32,
          snapshot_bytes,
          new_header.page_size as usize,
        )?;
      }
      new_header.snapshot_start_page = new_wal_end_page;
    } else if snapshot_page_count > 0 {
      new_header.snapshot_start_page = new_wal_end_page;
    }

    new_header.wal_page_count = new_wal_page_count;
    new_header.wal_head = 0;
    new_header.wal_tail = 0;
    new_header.wal_primary_head = 0;
    new_header.wal_secondary_head = 0;
    new_header.active_wal_region = 0;
    new_header.checkpoint_in_progress = 0;

    new_header.db_size_pages = if new_header.snapshot_page_count > 0 {
      new_header.snapshot_start_page + new_header.snapshot_page_count
    } else {
      new_header.wal_start_page + new_header.wal_page_count
    };
    if snapshot_bytes.is_none() {
      let mut pager = self.pager.lock();
      self.persist_header(&mut pager, &mut new_header, true)?;
      if new_header.snapshot_page_count > 0 {
        *self.snapshot.write() = None;
      }
      if new_header.db_size_pages < header.db_size_pages {
        pager.truncate_pages(new_header.db_size_pages as u32)?;
      }
    } else {
      let mut pager = self.pager.lock();
      self.persist_header(&mut pager, &mut new_header, true)?;
      pager.truncate_pages(new_header.db_size_pages as u32)?;
    }

    let new_wal_buffer = WalBuffer::from_header(&new_header);

    {
      let mut header_guard = self.header.write();
      *header_guard = new_header;
    }

    {
      let mut wal_buffer = self.wal_buffer.lock();
      *wal_buffer = new_wal_buffer;
    }

    self.reload_snapshot()?;

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
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
}
