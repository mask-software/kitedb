//! Checkpoint operations for SingleFileDB
//!
//! Handles merging snapshot + delta into a new snapshot, clearing WAL.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use crate::core::pager::{pages_to_store, FilePager};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use crate::error::{RayError, Result};
use crate::types::*;
use crate::util::mmap::map_file;
use crate::vector::store::vector_store_get;

use super::vector::vector_stores_from_snapshot;
use super::{CheckpointStatus, SingleFileDB};

type GraphData = (
  Vec<NodeData>,
  Vec<EdgeData>,
  HashMap<LabelId, String>,
  HashMap<ETypeId, String>,
  HashMap<PropKeyId, String>,
);

impl SingleFileDB {
  // ========================================================================
  // Blocking Checkpoint
  // ========================================================================

  /// Perform a checkpoint - merge snapshot + delta into new snapshot
  ///
  /// This:
  /// 1. Collects all graph data from snapshot + delta
  /// 2. Builds a new snapshot in memory
  /// 3. Writes the new snapshot to disk (after WAL)
  /// 4. Updates header to point to new snapshot
  /// 5. Clears WAL and delta
  pub fn checkpoint(&self) -> Result<()> {
    if self.read_only {
      return Err(RayError::ReadOnly);
    }

    // Don't checkpoint with active transaction
    if self.has_transaction() {
      return Err(RayError::TransactionInProgress);
    }

    // Collect all graph data
    let (nodes, edges, labels, etypes, propkeys) = self.collect_graph_data();

    // Get current header state
    let header = self.header.read().clone();
    let new_gen = header.active_snapshot_gen + 1;

    // Build new snapshot in memory
    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      compression: None, // TODO: Add compression support
    })?;

    // Calculate where to place new snapshot (after WAL)
    let wal_end_page = header.wal_start_page + header.wal_page_count;
    let new_snapshot_start_page = wal_end_page;
    let new_snapshot_page_count =
      pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;

    // Write snapshot to file
    {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(
        &mut pager,
        new_snapshot_start_page as u32,
        &snapshot_buffer,
        header.page_size as usize,
      )?;
    }

    // Update header
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      // Update header fields
      header.active_snapshot_gen = new_gen;
      header.snapshot_start_page = new_snapshot_start_page;
      header.snapshot_page_count = new_snapshot_page_count;
      header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      // Reset WAL
      header.wal_head = 0;
      header.wal_tail = 0;
      wal_buffer.reset();

      // Increment change counter
      header.change_counter += 1;

      // Write header to disk
      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes)?;
      pager.sync()?;
    }

    // Clear delta
    self.delta.write().clear();

    // Reload the new snapshot
    self.reload_snapshot()?;

    Ok(())
  }

  /// Reload snapshot from disk after checkpoint
  pub(crate) fn reload_snapshot(&self) -> Result<()> {
    let header = self.header.read();

    if header.snapshot_page_count == 0 {
      // No snapshot to load
      *self.snapshot.write() = None;
      self.vector_stores.write().clear();
      return Ok(());
    }

    // Calculate snapshot offset in bytes
    let snapshot_offset = (header.snapshot_start_page * header.page_size as u64) as usize;

    // Re-mmap the file and parse snapshot
    let pager = self.pager.lock();
    let new_snapshot = SnapshotData::parse_at_offset(
      std::sync::Arc::new({
        // Safety handled inside map_file (native mmap) or in-memory read (wasm).
        map_file(pager.file())?
      }),
      snapshot_offset,
      &crate::core::snapshot::reader::ParseSnapshotOptions::default(),
    )?;

    // Update the snapshot
    *self.snapshot.write() = Some(new_snapshot);

    // Rebuild vector stores from the new snapshot
    if let Some(ref snapshot) = *self.snapshot.read() {
      let stores = vector_stores_from_snapshot(snapshot)?;
      *self.vector_stores.write() = stores;
    }

    Ok(())
  }

  // ========================================================================
  // Background Checkpoint (Non-Blocking)
  // ========================================================================

  /// Check if a background checkpoint is currently running
  pub fn is_checkpoint_running(&self) -> bool {
    let status = *self.checkpoint_status.lock();
    matches!(
      status,
      CheckpointStatus::Running | CheckpointStatus::Completing
    )
  }

  /// Get current checkpoint status
  pub fn checkpoint_status(&self) -> CheckpointStatus {
    *self.checkpoint_status.lock()
  }

  /// Trigger a background checkpoint (non-blocking)
  ///
  /// This switches writes to secondary WAL region immediately and starts
  /// the checkpoint process. Writes can continue while checkpoint is running.
  ///
  /// Steps:
  /// 1. Switch writes to secondary WAL region
  /// 2. Set checkpointInProgress flag (for crash recovery)
  /// 3. Build new snapshot from primary WAL + current snapshot + delta
  /// 4. Write new snapshot to disk
  /// 5. Merge secondary into primary, update header
  /// 6. Clear checkpointInProgress flag
  pub fn background_checkpoint(&self) -> Result<()> {
    if self.read_only {
      return Err(RayError::ReadOnly);
    }

    // Check if already running
    {
      let mut status = self.checkpoint_status.lock();
      match *status {
        CheckpointStatus::Running => {
          // Already running, just return
          return Ok(());
        }
        CheckpointStatus::Completing => {
          // Wait for completion by returning
          return Ok(());
        }
        CheckpointStatus::Idle => {
          *status = CheckpointStatus::Running;
        }
      }
    }

    // Step 1: Switch writes to secondary region
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();

      // Switch WAL to secondary region
      wal_buffer.switch_to_secondary();

      // Update header to reflect the switch
      header.active_wal_region = 1;
      header.checkpoint_in_progress = 1;
      header.wal_primary_head = wal_buffer.primary_head();
      header.wal_secondary_head = wal_buffer.secondary_head();
      header.change_counter += 1;

      // Write header to disk
      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes)?;
      pager.sync()?;
    }

    // Step 2-4: Build and write snapshot, get the info
    let snapshot_info = match self.build_and_write_snapshot() {
      Ok(info) => info,
      Err(e) => {
        // On error, try to recover
        self.recover_from_checkpoint_error();
        return Err(e);
      }
    };

    // Step 5: Complete the checkpoint
    self.complete_background_checkpoint(snapshot_info)?;

    Ok(())
  }

  /// Build and write the snapshot (called during background checkpoint)
  /// Returns (new_gen, new_snapshot_start_page, new_snapshot_page_count)
  fn build_and_write_snapshot(&self) -> Result<(u64, u64, u64)> {
    // Collect all graph data (reads from snapshot + delta)
    let (nodes, edges, labels, etypes, propkeys) = self.collect_graph_data();

    // Get current header state
    let header = self.header.read().clone();
    let new_gen = header.active_snapshot_gen + 1;

    // Build new snapshot in memory
    let snapshot_buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: new_gen,
      nodes,
      edges,
      labels,
      etypes,
      propkeys,
      compression: None,
    })?;

    // Calculate where to place new snapshot (after WAL)
    let wal_end_page = header.wal_start_page + header.wal_page_count;
    let new_snapshot_start_page = wal_end_page;
    let new_snapshot_page_count =
      pages_to_store(snapshot_buffer.len(), header.page_size as usize) as u64;

    // Write snapshot to file
    {
      let mut pager = self.pager.lock();
      self.write_snapshot_pages(
        &mut pager,
        new_snapshot_start_page as u32,
        &snapshot_buffer,
        header.page_size as usize,
      )?;
    }

    Ok((new_gen, new_snapshot_start_page, new_snapshot_page_count))
  }

  /// Complete the background checkpoint
  fn complete_background_checkpoint(&self, snapshot_info: (u64, u64, u64)) -> Result<()> {
    let (new_gen, new_snapshot_start_page, new_snapshot_page_count) = snapshot_info;

    // Mark as completing (brief lock period)
    *self.checkpoint_status.lock() = CheckpointStatus::Completing;

    // Merge secondary records into primary and update header
    {
      let mut pager = self.pager.lock();
      let mut wal_buffer = self.wal_buffer.lock();
      let mut header = self.header.write();
      let old_snapshot_start_page = header.snapshot_start_page;
      let old_snapshot_page_count = header.snapshot_page_count;

      // Merge secondary WAL records into primary
      wal_buffer.merge_secondary_into_primary(&mut pager)?;
      wal_buffer.flush(&mut pager)?;

      // Update header with new snapshot location
      header.active_snapshot_gen = new_gen;
      header.snapshot_start_page = new_snapshot_start_page;
      header.snapshot_page_count = new_snapshot_page_count;
      header.db_size_pages = new_snapshot_start_page + new_snapshot_page_count;
      header.max_node_id = self.next_node_id.load(Ordering::SeqCst).saturating_sub(1);
      header.next_tx_id = self.next_tx_id.load(Ordering::SeqCst);

      // Update WAL state
      header.wal_head = wal_buffer.head();
      header.wal_tail = wal_buffer.tail();
      header.wal_primary_head = wal_buffer.primary_head();
      header.wal_secondary_head = wal_buffer.secondary_head();
      header.active_wal_region = 0;
      header.checkpoint_in_progress = 0;
      header.change_counter += 1;

      // Write header to disk
      let header_bytes = header.serialize_to_page();
      pager.write_page(0, &header_bytes)?;
      pager.sync()?;

      // Mark old snapshot pages as free (for future vacuum)
      if old_snapshot_page_count > 0 && old_snapshot_start_page != new_snapshot_start_page {
        pager.free_pages(
          old_snapshot_start_page as u32,
          old_snapshot_page_count as u32,
        );
      }
    }

    // Clear delta
    self.delta.write().clear();

    // Reload the new snapshot
    self.reload_snapshot()?;

    // Mark as idle
    *self.checkpoint_status.lock() = CheckpointStatus::Idle;

    Ok(())
  }

  /// Recover from a checkpoint error
  fn recover_from_checkpoint_error(&self) {
    // Try to switch back to primary region and clear the checkpoint flag
    if let Some(mut pager) = self.pager.try_lock() {
      if let Some(mut wal_buffer) = self.wal_buffer.try_lock() {
        if let Some(mut header) = self.header.try_write() {
          // Switch back to primary
          wal_buffer.switch_to_primary(false);

          // Clear checkpoint flag
          header.active_wal_region = 0;
          header.checkpoint_in_progress = 0;

          // Try to write header
          let header_bytes = header.serialize_to_page();
          let _ = pager.write_page(0, &header_bytes);
          let _ = pager.sync();
        }
      }
    }

    // Mark as idle
    *self.checkpoint_status.lock() = CheckpointStatus::Idle;
  }

  /// Write snapshot buffer to file pages
  pub(crate) fn write_snapshot_pages(
    &self,
    pager: &mut FilePager,
    start_page: u32,
    buffer: &[u8],
    page_size: usize,
  ) -> Result<()> {
    let num_pages = pages_to_store(buffer.len(), page_size);

    // Ensure file is large enough
    let required_pages = start_page + num_pages;
    let current_pages = (pager.file_size() as usize).div_ceil(page_size);

    if required_pages as usize > current_pages {
      pager.allocate_pages(required_pages - current_pages as u32)?;
    }

    // Write pages
    for i in 0..num_pages {
      let mut page_data = vec![0u8; page_size];
      let src_offset = i as usize * page_size;
      let src_end = std::cmp::min(src_offset + page_size, buffer.len());
      page_data[..src_end - src_offset].copy_from_slice(&buffer[src_offset..src_end]);
      pager.write_page(start_page + i, &page_data)?;
    }

    // Sync to disk
    pager.sync()?;

    Ok(())
  }

  /// Collect all graph data from snapshot + delta
  pub(crate) fn collect_graph_data(&self) -> GraphData {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut labels = HashMap::new();
    let mut etypes = HashMap::new();
    let mut propkeys = HashMap::new();

    let delta = self.delta.read();

    // First, copy schema from our in-memory maps
    for (&id, name) in self.label_ids.read().iter() {
      labels.insert(id, name.clone());
    }
    for (&id, name) in self.etype_ids.read().iter() {
      etypes.insert(id, name.clone());
    }
    for (&id, name) in self.propkey_ids.read().iter() {
      propkeys.insert(id, name.clone());
    }

    // Collect nodes from snapshot
    if let Some(ref snapshot) = *self.snapshot.read() {
      let num_nodes = snapshot.header.num_nodes as usize;

      for phys in 0..num_nodes {
        let node_id = match snapshot.get_node_id(phys as u32) {
          Some(id) => id,
          None => continue,
        };

        // Skip deleted nodes
        if delta.is_node_deleted(node_id) {
          continue;
        }

        // Get key
        let key = snapshot.get_node_key(phys as u32);

        // Get properties from snapshot
        let mut props = HashMap::new();
        if let Some(snapshot_props) = snapshot.get_node_props(phys as u32) {
          for (key_id, value) in snapshot_props {
            props.insert(key_id, value);
          }
        }

        // Apply delta modifications
        if let Some(node_delta) = delta.get_node_delta(node_id) {
          if let Some(ref delta_props) = node_delta.props {
            for (&key_id, value) in delta_props {
              match value {
                Some(v) => {
                  props.insert(key_id, v.clone());
                }
                None => {
                  props.remove(&key_id);
                }
              }
            }
          }
        }

        // Collect node labels (snapshot + delta)
        let mut node_labels: std::collections::HashSet<LabelId> = std::collections::HashSet::new();

        if let Some(snapshot_labels) = snapshot.get_node_labels(phys as u32) {
          node_labels.extend(snapshot_labels.into_iter());
        }

        if let Some(node_delta) = delta.get_node_delta(node_id) {
          if let Some(ref labels) = node_delta.labels {
            node_labels.extend(labels.iter().copied());
          }
          if let Some(ref deleted) = node_delta.labels_deleted {
            for label_id in deleted {
              node_labels.remove(label_id);
            }
          }
        }

        let mut node_labels: Vec<LabelId> = node_labels.into_iter().collect();
        node_labels.sort_unstable();

        nodes.push(NodeData {
          node_id,
          key,
          labels: node_labels,
          props,
        });

        // Collect edges from this node
        for edge_info in snapshot.get_out_edges(phys as u32) {
          let dst_node_id = match snapshot.get_node_id(edge_info.dst) {
            Some(id) => id,
            None => continue,
          };

          // Skip edges to deleted nodes
          if delta.is_node_deleted(dst_node_id) {
            continue;
          }

          // Skip deleted edges
          if delta.is_edge_deleted(node_id, edge_info.etype, dst_node_id) {
            continue;
          }

          // Get edge props from snapshot
          let mut edge_props = HashMap::new();
          if let Some(edge_idx) =
            snapshot.find_edge_index(phys as u32, edge_info.etype, edge_info.dst)
          {
            if let Some(snapshot_edge_props) = snapshot.get_edge_props(edge_idx) {
              edge_props = snapshot_edge_props;
            }
          }

          // Apply delta edge prop modifications
          let edge_key = (node_id, edge_info.etype, dst_node_id);
          if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
            for (&key_id, value) in delta_edge_props {
              match value {
                Some(v) => {
                  edge_props.insert(key_id, v.clone());
                }
                None => {
                  edge_props.remove(&key_id);
                }
              }
            }
          }

          edges.push(EdgeData {
            src: node_id,
            etype: edge_info.etype,
            dst: dst_node_id,
            props: edge_props,
          });
        }
      }
    }

    // Add nodes created in delta
    for (&node_id, node_delta) in &delta.created_nodes {
      let mut props = HashMap::new();
      if let Some(ref delta_props) = node_delta.props {
        for (&key_id, value) in delta_props {
          if let Some(v) = value {
            props.insert(key_id, v.clone());
          }
        }
      }

      let mut node_labels: Vec<LabelId> = node_delta
        .labels
        .as_ref()
        .map(|l| l.iter().copied().collect())
        .unwrap_or_default();
      node_labels.sort_unstable();

      nodes.push(NodeData {
        node_id,
        key: node_delta.key.clone(),
        labels: node_labels,
        props,
      });
    }

    // Add edges from delta
    for (&src, patches) in &delta.out_add {
      // Skip edges from deleted nodes
      if delta.is_node_deleted(src) {
        continue;
      }

      for patch in patches {
        // Skip edges to deleted nodes
        if delta.is_node_deleted(patch.other) {
          continue;
        }

        // Get edge props from delta
        let mut edge_props = HashMap::new();
        let edge_key = (src, patch.etype, patch.other);
        if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
          for (&key_id, value) in delta_edge_props {
            if let Some(v) = value {
              edge_props.insert(key_id, v.clone());
            }
          }
        }

        edges.push(EdgeData {
          src,
          etype: patch.etype,
          dst: patch.other,
          props: edge_props,
        });
      }
    }

    // Merge vector embeddings into node props for snapshot persistence
    if !self.vector_stores.read().is_empty() {
      let mut node_index: HashMap<NodeId, usize> = HashMap::new();
      for (idx, node) in nodes.iter().enumerate() {
        node_index.insert(node.node_id, idx);
      }

      let stores = self.vector_stores.read();
      for (&prop_key_id, store) in stores.iter() {
        for &node_id in store.node_to_vector.keys() {
          if delta.is_node_deleted(node_id) {
            continue;
          }

          let Some(&idx) = node_index.get(&node_id) else {
            continue;
          };

          if let Some(vec) = vector_store_get(store, node_id) {
            nodes[idx]
              .props
              .insert(prop_key_id, PropValue::VectorF32(vec.to_vec()));
          }
        }
      }
    }

    (nodes, edges, labels, etypes, propkeys)
  }

  /// Check if checkpoint is recommended based on WAL usage
  pub fn should_checkpoint(&self, threshold: f64) -> bool {
    let stats = self.wal_stats();
    stats.used as f64 / stats.capacity as f64 >= threshold
  }
}
