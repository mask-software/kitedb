//! Single-file compactor and vacuum operations.

use std::sync::atomic::Ordering;

use crate::core::pager::pages_to_store;
use crate::core::snapshot::writer::{build_snapshot_to_memory, SnapshotBuildInput};
use crate::core::wal::buffer::WalBuffer;
use crate::error::{RayError, Result};
use crate::util::compression::CompressionOptions;

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

impl SingleFileDB {
  /// Optimize (compact) a single-file database.
  ///
  /// This merges snapshot + delta into a new snapshot and clears WAL.
  pub fn optimize_single_file(&self, options: Option<SingleFileOptimizeOptions>) -> Result<()> {
    if self.read_only {
      return Err(RayError::ReadOnly);
    }

    if self.has_transaction() {
      return Err(RayError::TransactionInProgress);
    }

    if self.is_checkpoint_running() {
      // Wait for background checkpoint to complete (mirrors TS behavior)
      while self.is_checkpoint_running() {
        std::thread::sleep(std::time::Duration::from_millis(1));
      }
    }

    let (nodes, edges, labels, etypes, propkeys) = self.collect_graph_data();

    let header = self.header.read().clone();
    let old_snapshot_start_page = header.snapshot_start_page;
    let old_snapshot_page_count = header.snapshot_page_count;
    let new_gen = header.active_snapshot_gen + 1;
    let compression = options.and_then(|o| o.compression);

    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      compression,
    })?;

    let wal_end_page = header.wal_start_page + header.wal_page_count;
    let new_snapshot_start_page = wal_end_page;
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

    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      header.active_snapshot_gen = new_gen;
      header.snapshot_start_page = new_snapshot_start_page;
      header.snapshot_page_count = new_snapshot_page_count;
      header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      header.wal_head = 0;
      header.wal_tail = 0;
      wal_buffer.reset();

      header.change_counter += 1;

      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes)?;
      pager.sync()?;

      if old_snapshot_page_count > 0 && old_snapshot_start_page != new_snapshot_start_page {
        pager.free_pages(
          old_snapshot_start_page as u32,
          old_snapshot_page_count as u32,
        );
      }
    }

    self.delta.write().clear();
    self.reload_snapshot()?;

    Ok(())
  }

  /// Vacuum operation - shrink file by reclaiming free pages.
  pub fn vacuum_single_file(&self, options: Option<VacuumOptions>) -> Result<()> {
    if self.read_only {
      return Err(RayError::ReadOnly);
    }

    if self.has_transaction() {
      return Err(RayError::TransactionInProgress);
    }

    let options = options.unwrap_or_default();

    let header = self.header.read().clone();
    let page_size = header.page_size as u64;

    let min_wal_pages = if let Some(min_wal_size) = options.min_wal_size {
      min_wal_size.div_ceil(page_size)
    } else {
      MIN_WAL_PAGES
    };

    let wal_is_empty =
      header.wal_head == header.wal_tail || (header.wal_head == 0 && header.wal_tail == 0);
    let can_shrink_wal =
      options.shrink_wal && wal_is_empty && header.wal_page_count > min_wal_pages;

    if header.snapshot_page_count == 0 && !can_shrink_wal {
      return Ok(());
    }

    let new_wal_page_count = if can_shrink_wal {
      min_wal_pages
    } else {
      header.wal_page_count
    };
    let new_wal_end_page = header.wal_start_page + new_wal_page_count;

    let mut new_header = header.clone();

    if header.snapshot_page_count > 0 {
      let current_snapshot_start = header.snapshot_start_page;
      let new_snapshot_start = new_wal_end_page;

      if current_snapshot_start != new_snapshot_start {
        let snapshot_bytes = {
          let mut pager = self.pager.lock();
          let slice = pager.mmap_range(
            current_snapshot_start as u32,
            header.snapshot_page_count as u32,
          )?;
          slice.to_vec()
        };

        let mut pager = self.pager.lock();
        self.write_snapshot_pages(
          &mut pager,
          new_snapshot_start as u32,
          &snapshot_bytes,
          header.page_size as usize,
        )?;
      }

      new_header.snapshot_start_page = new_snapshot_start;
    }

    if can_shrink_wal {
      new_header.wal_page_count = new_wal_page_count;
    }

    new_header.db_size_pages = if new_header.snapshot_page_count > 0 {
      new_header.snapshot_start_page + new_header.snapshot_page_count
    } else {
      new_header.wal_start_page + new_header.wal_page_count
    };
    new_header.change_counter += 1;

    {
      let mut pager = self.pager.lock();
      let header_bytes = new_header.serialize_to_page();
      pager.write_page(0, &header_bytes)?;
      pager.sync()?;
      pager.truncate_pages(new_header.db_size_pages as u32)?;
    }

    {
      let mut header_guard = self.header.write();
      *header_guard = new_header.clone();
    }

    {
      let mut wal_buffer = self.wal_buffer.lock();
      *wal_buffer = WalBuffer::from_header(&new_header);
    }

    self.reload_snapshot()?;

    Ok(())
  }
}
