//! Compactor - merges snapshot + delta into new snapshot
//!
//! The compaction process:
//! 1. Collect all live nodes and edges from snapshot + delta
//! 2. Build a new snapshot with the merged data
//! 3. Update manifest to point to new snapshot
//! 4. Clear WAL and delta
//! 5. Garbage collect old snapshots
//!
//! Ported from src/core/compactor.ts

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::constants::*;
use crate::error::Result;
use crate::types::*;
use crate::util::compression::CompressionOptions;

use super::manifest::{update_manifest_for_compaction, write_manifest};
use super::snapshot::reader::SnapshotData;
use super::snapshot::writer::{build_snapshot, EdgeData, NodeData, SnapshotBuildInput};
use super::wal::writer::create_wal_segment;

// ============================================================================
// Types
// ============================================================================

/// Options for the optimize operation
#[derive(Debug, Clone, Default)]
pub struct OptimizeOptions {
  /// Compression options for the new snapshot
  pub compression: Option<CompressionOptions>,
}

/// Collected graph data for building a new snapshot
pub struct CollectedGraphData {
  pub nodes: Vec<NodeData>,
  pub edges: Vec<EdgeData>,
  pub labels: HashMap<LabelId, String>,
  pub etypes: HashMap<ETypeId, String>,
  pub propkeys: HashMap<PropKeyId, String>,
}

// ============================================================================
// Compaction
// ============================================================================

/// Perform compaction - merge snapshot + delta into new snapshot
///
/// This is for multi-file format databases.
///
/// # Arguments
/// * `db_path` - Path to the database directory
/// * `snapshot` - Current snapshot data (optional if no snapshot yet)
/// * `delta` - Current delta state
/// * `manifest` - Current manifest
/// * `options` - Optimize options
///
/// # Returns
/// * New manifest after compaction
/// * Path to new snapshot file
pub fn optimize(
  db_path: &Path,
  snapshot: Option<&SnapshotData>,
  delta: &DeltaState,
  manifest: &ManifestV1,
  options: &OptimizeOptions,
) -> Result<(ManifestV1, PathBuf)> {
  // Collect all graph data
  let collected = collect_graph_data(snapshot, delta)?;

  // Calculate new generation numbers
  let new_gen = manifest.active_snapshot_gen + 1;
  let new_wal_seg = manifest.active_wal_seg + 1;

  // Build new snapshot
  let input = SnapshotBuildInput {
    generation: new_gen,
    nodes: collected.nodes,
    edges: collected.edges,
    labels: collected.labels,
    etypes: collected.etypes,
    propkeys: collected.propkeys,
    compression: options.compression.clone(),
  };

  let snapshot_path = build_snapshot(db_path, input)?;

  // Create new WAL segment
  create_wal_segment(db_path, new_wal_seg)?;

  // Update manifest
  let new_manifest = update_manifest_for_compaction(manifest, new_gen, new_wal_seg);
  write_manifest(db_path, &new_manifest)?;

  // Garbage collect old snapshots (keep last 2)
  gc_snapshots(
    db_path,
    new_manifest.active_snapshot_gen,
    new_manifest.prev_snapshot_gen,
  )?;

  Ok((new_manifest, PathBuf::from(snapshot_path)))
}

/// Collect all graph data from snapshot + delta
///
/// This merges the snapshot data with delta modifications to produce
/// a complete set of nodes and edges for the new snapshot.
pub fn collect_graph_data(
  snapshot: Option<&SnapshotData>,
  delta: &DeltaState,
) -> Result<CollectedGraphData> {
  let mut nodes: Vec<NodeData> = Vec::new();
  let mut edges: Vec<EdgeData> = Vec::new();
  let mut labels: HashMap<LabelId, String> = HashMap::new();
  let mut etypes: HashMap<ETypeId, String> = HashMap::new();
  let mut propkeys: HashMap<PropKeyId, String> = HashMap::new();

  // First, add data from snapshot
  if let Some(snap) = snapshot {
    let num_nodes = snap.header.num_nodes as usize;

    // Copy labels from snapshot
    for label_id in 1..=snap.header.num_labels as LabelId {
      if let Some(name) = snap.get_label_name(label_id) {
        labels.insert(label_id, name.to_string());
      }
    }

    // Copy etypes from snapshot
    for etype_id in 1..=snap.header.num_etypes as ETypeId {
      if let Some(name) = snap.get_etype_name(etype_id) {
        etypes.insert(etype_id, name.to_string());
      }
    }

    // Copy propkeys from snapshot
    for propkey_id in 1..=snap.header.num_propkeys as PropKeyId {
      if let Some(name) = snap.get_propkey_name(propkey_id) {
        propkeys.insert(propkey_id, name.to_string());
      }
    }

    // Collect nodes from snapshot
    for phys in 0..num_nodes {
      let node_id = match snap.get_node_id(phys as PhysNode) {
        Some(id) => id,
        None => continue,
      };

      // Skip deleted nodes
      if delta.deleted_nodes.contains(&node_id) {
        continue;
      }

      // Get key
      let key = snap.get_node_key(phys as PhysNode);

      // Get properties from snapshot
      let mut props: HashMap<PropKeyId, PropValue> = HashMap::new();
      if let Some(snapshot_props) = snap.get_node_props(phys as PhysNode) {
        for (key_id, value) in snapshot_props {
          props.insert(key_id, value);
        }
      }

      // Apply delta modifications
      let mut node_labels: std::collections::HashSet<LabelId> = std::collections::HashSet::new();

      if let Some(snapshot_labels) = snap.get_node_labels(phys as PhysNode) {
        node_labels.extend(snapshot_labels.into_iter());
      }

      if let Some(node_delta) = delta.modified_nodes.get(&node_id) {
        // Apply property changes
        if let Some(delta_props) = &node_delta.props {
          for (key_id, value) in delta_props {
            if let Some(v) = value {
              props.insert(*key_id, v.clone());
            } else {
              props.remove(key_id);
            }
          }
        }
        // Apply label changes
        if let Some(delta_labels) = &node_delta.labels {
          node_labels.extend(delta_labels.iter().copied());
        }
        if let Some(deleted) = &node_delta.labels_deleted {
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

      // Collect edges from this node (from snapshot)
      for edge in snap.get_out_edges(phys as PhysNode) {
        let dst_node_id = match snap.get_node_id(edge.dst) {
          Some(id) => id,
          None => continue,
        };

        // Skip edges to deleted nodes
        if delta.deleted_nodes.contains(&dst_node_id) {
          continue;
        }

        // Skip deleted edges
        if delta.is_edge_deleted(node_id, edge.etype, dst_node_id) {
          continue;
        }

        // Collect edge props from snapshot
        let mut edge_props: HashMap<PropKeyId, PropValue> = HashMap::new();
        if let Some(edge_idx) = snap.find_edge_index(phys as PhysNode, edge.etype, edge.dst) {
          if let Some(snapshot_edge_props) = snap.get_edge_props(edge_idx) {
            for (key_id, value) in snapshot_edge_props {
              edge_props.insert(key_id, value);
            }
          }
        }

        // Apply delta edge prop modifications
        let edge_key = (node_id, edge.etype, dst_node_id);
        if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
          for (key_id, value) in delta_edge_props {
            if let Some(v) = value {
              edge_props.insert(*key_id, v.clone());
            } else {
              edge_props.remove(key_id);
            }
          }
        }

        edges.push(EdgeData {
          src: node_id,
          etype: edge.etype,
          dst: dst_node_id,
          props: edge_props,
        });
      }
    }
  }

  // Add new labels from delta
  for (label_id, name) in &delta.new_labels {
    labels.insert(*label_id, name.clone());
  }

  // Add new etypes from delta
  for (etype_id, name) in &delta.new_etypes {
    etypes.insert(*etype_id, name.clone());
  }

  // Add new propkeys from delta
  for (propkey_id, name) in &delta.new_propkeys {
    propkeys.insert(*propkey_id, name.clone());
  }

  // Add nodes created in delta
  for (node_id, node_delta) in &delta.created_nodes {
    let mut props: HashMap<PropKeyId, PropValue> = HashMap::new();
    if let Some(delta_props) = &node_delta.props {
      for (key_id, value) in delta_props {
        if let Some(v) = value {
          props.insert(*key_id, v.clone());
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
      node_id: *node_id,
      key: node_delta.key.clone(),
      labels: node_labels,
      props,
    });
  }

  // Add edges from delta
  for (src, patches) in &delta.out_add {
    for patch in patches {
      // Check if either endpoint is deleted
      if delta.deleted_nodes.contains(src) || delta.deleted_nodes.contains(&patch.other) {
        continue;
      }

      // Collect edge props from delta
      let edge_key = (*src, patch.etype, patch.other);
      let mut edge_props: HashMap<PropKeyId, PropValue> = HashMap::new();

      if let Some(delta_edge_props) = delta.edge_props.get(&edge_key) {
        for (key_id, value) in delta_edge_props {
          if let Some(v) = value {
            edge_props.insert(*key_id, v.clone());
          }
        }
      }

      edges.push(EdgeData {
        src: *src,
        etype: patch.etype,
        dst: patch.other,
        props: edge_props,
      });
    }
  }

  Ok(CollectedGraphData {
    nodes,
    edges,
    labels,
    etypes,
    propkeys,
  })
}

// ============================================================================
// Garbage Collection
// ============================================================================

/// Garbage collect old snapshots (keep last 2)
fn gc_snapshots(db_path: &Path, active_gen: u64, prev_gen: u64) -> Result<()> {
  let snapshots_dir = db_path.join(SNAPSHOTS_DIR);

  if !snapshots_dir.exists() {
    return Ok(());
  }

  let entries = match fs::read_dir(&snapshots_dir) {
    Ok(e) => e,
    Err(_) => return Ok(()), // Ignore errors
  };

  for entry in entries.flatten() {
    let filename = entry.file_name();
    let filename_str = filename.to_string_lossy();

    // Parse generation from filename (format: "snap_{:016}.gds")
    if let Some(gen) = parse_snapshot_gen(&filename_str) {
      // Keep active and prev
      if gen == active_gen || gen == prev_gen {
        continue;
      }

      // Delete older snapshots
      let filepath = entry.path();
      if fs::remove_file(&filepath).is_err() {
        // On Windows, file might be in use - try moving to trash
        let trash_dir = db_path.join(TRASH_DIR);
        let _ = fs::create_dir_all(&trash_dir);
        let _ = fs::rename(&filepath, trash_dir.join(&filename));
      }
    }
  }

  Ok(())
}

/// Clean up trash directory
pub fn clean_trash(db_path: &Path) -> Result<()> {
  let trash_dir = db_path.join(TRASH_DIR);

  if !trash_dir.exists() {
    return Ok(());
  }

  let entries = match fs::read_dir(&trash_dir) {
    Ok(e) => e,
    Err(_) => return Ok(()),
  };

  for entry in entries.flatten() {
    let _ = fs::remove_file(entry.path());
  }

  Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_collect_graph_data_empty() {
    let delta = DeltaState::new();
    let result = collect_graph_data(None, &delta).unwrap();

    assert!(result.nodes.is_empty());
    assert!(result.edges.is_empty());
    assert!(result.labels.is_empty());
    assert!(result.etypes.is_empty());
    assert!(result.propkeys.is_empty());
  }

  #[test]
  fn test_collect_graph_data_delta_only() {
    let mut delta = DeltaState::new();

    // Add a label
    delta.new_labels.insert(1, "Person".to_string());

    // Add an etype
    delta.new_etypes.insert(1, "KNOWS".to_string());

    // Add a propkey
    delta.new_propkeys.insert(1, "name".to_string());

    // Create a node
    delta.create_node(1, Some("alice"));
    delta.set_node_prop(1, 1, PropValue::String("Alice".to_string()));

    // Create another node
    delta.create_node(2, Some("bob"));
    delta.set_node_prop(2, 1, PropValue::String("Bob".to_string()));

    // Add an edge
    delta.add_edge(1, 1, 2);

    let result = collect_graph_data(None, &delta).unwrap();

    assert_eq!(result.nodes.len(), 2);
    assert_eq!(result.edges.len(), 1);
    assert_eq!(result.labels.len(), 1);
    assert_eq!(result.etypes.len(), 1);
    assert_eq!(result.propkeys.len(), 1);

    // Verify edge
    let edge = &result.edges[0];
    assert_eq!(edge.src, 1);
    assert_eq!(edge.etype, 1);
    assert_eq!(edge.dst, 2);
  }

  #[test]
  fn test_collect_graph_data_with_deletion() {
    let mut delta = DeltaState::new();

    // Create nodes
    delta.create_node(1, None);
    delta.create_node(2, None);
    delta.create_node(3, None);

    // Add edges
    delta.new_etypes.insert(1, "LINK".to_string());
    delta.add_edge(1, 1, 2);
    delta.add_edge(2, 1, 3);

    // Delete node 2
    delta.delete_node(2);

    let result = collect_graph_data(None, &delta).unwrap();

    // Node 2 should be excluded, and edges involving it should be excluded
    assert_eq!(result.nodes.len(), 2); // nodes 1 and 3
    assert_eq!(result.edges.len(), 0); // both edges involve node 2
  }

  #[test]
  fn test_collect_graph_data_edge_deletion() {
    let mut delta = DeltaState::new();

    // Create nodes
    delta.create_node(1, None);
    delta.create_node(2, None);

    // Add and then delete an edge
    delta.new_etypes.insert(1, "LINK".to_string());
    delta.add_edge(1, 1, 2);
    delta.delete_edge(1, 1, 2);

    let result = collect_graph_data(None, &delta).unwrap();

    assert_eq!(result.nodes.len(), 2);
    assert_eq!(result.edges.len(), 0); // Edge was deleted
  }

  #[test]
  fn test_optimize_options_default() {
    let opts = OptimizeOptions::default();
    assert!(opts.compression.is_none());
  }
}
