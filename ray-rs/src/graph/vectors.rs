//! Vector embedding operations for multi-file GraphDB
//!
//! Mirrors the single-file vector API but uses GraphDB WAL + delta.

use crate::error::{RayError, Result};
use crate::types::{NodeId, PropKeyId};
use crate::vector::store::{
  create_vector_store, vector_store_get, vector_store_has, vector_store_stats,
};
use crate::vector::{VectorManifest, VectorStoreConfig, VectorStoreStats};

use super::db::GraphDB;
use super::tx::TxHandle;

/// Set a vector embedding for a node
pub fn set_node_vector(
  handle: &mut TxHandle,
  node_id: NodeId,
  prop_key_id: PropKeyId,
  vector: &[f32],
) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  // Check dimensions if store already exists
  {
    let stores = handle.db.vector_stores.read();
    if let Some(store) = stores.get(&prop_key_id) {
      if store.config.dimensions != vector.len() {
        return Err(RayError::VectorDimensionMismatch {
          expected: store.config.dimensions,
          got: vector.len(),
        });
      }
    }
  }

  handle
    .tx
    .pending_vector_deletes
    .remove(&(node_id, prop_key_id));

  handle
    .tx
    .pending_vector_sets
    .insert((node_id, prop_key_id), vector.to_vec());

  Ok(())
}

/// Delete a vector embedding for a node
pub fn delete_node_vector(
  handle: &mut TxHandle,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> Result<()> {
  if handle.tx.read_only {
    return Err(RayError::ReadOnly);
  }

  handle
    .tx
    .pending_vector_sets
    .remove(&(node_id, prop_key_id));

  handle
    .tx
    .pending_vector_deletes
    .insert((node_id, prop_key_id));

  Ok(())
}

/// Get a vector embedding for a node (committed state)
pub fn get_node_vector_db(
  db: &GraphDB,
  node_id: NodeId,
  prop_key_id: PropKeyId,
) -> Option<Vec<f32>> {
  let delta = db.delta.read();
  if delta.is_node_deleted(node_id) {
    return None;
  }

  let stores = db.vector_stores.read();
  let store = stores.get(&prop_key_id)?;
  vector_store_get(store, node_id).map(|v| v.to_vec())
}

/// Check if a node has a vector embedding (committed state)
pub fn has_node_vector_db(db: &GraphDB, node_id: NodeId, prop_key_id: PropKeyId) -> bool {
  let delta = db.delta.read();
  if delta.is_node_deleted(node_id) {
    return false;
  }

  let stores = db.vector_stores.read();
  if let Some(store) = stores.get(&prop_key_id) {
    return vector_store_has(store, node_id);
  }
  false
}

/// Get a vector store manifest by property key
pub fn get_vector_store(db: &GraphDB, prop_key_id: PropKeyId) -> Option<VectorManifest> {
  let stores = db.vector_stores.read();
  stores.get(&prop_key_id).cloned()
}

/// Get vector store statistics by property key
pub fn get_vector_stats(db: &GraphDB, prop_key_id: PropKeyId) -> Option<VectorStoreStats> {
  let stores = db.vector_stores.read();
  stores.get(&prop_key_id).map(vector_store_stats)
}

/// Ensure a vector store exists with the given dimensions
pub fn get_or_create_vector_store(
  db: &GraphDB,
  prop_key_id: PropKeyId,
  dimensions: usize,
) -> Result<()> {
  let mut stores = db.vector_stores.write();
  if let Some(store) = stores.get(&prop_key_id) {
    if store.config.dimensions != dimensions {
      return Err(RayError::VectorDimensionMismatch {
        expected: store.config.dimensions,
        got: dimensions,
      });
    }
    return Ok(());
  }

  let config = VectorStoreConfig::new(dimensions);
  let manifest = create_vector_store(config);
  stores.insert(prop_key_id, manifest);
  Ok(())
}
