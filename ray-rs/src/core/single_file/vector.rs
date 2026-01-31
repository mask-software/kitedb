//! Vector operations for SingleFileDB
//!
//! Handles vector embedding storage and retrieval for nodes.

use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::record::{
  build_del_node_vector_payload, build_set_node_vector_payload, WalRecord,
};
use crate::error::{RayError, Result};
use crate::types::*;
use crate::vector::store::{
  create_vector_store, vector_store_delete, vector_store_get, vector_store_has, vector_store_insert,
};
use crate::vector::types::{VectorManifest, VectorStoreConfig};
use std::collections::HashMap;

use super::SingleFileDB;

impl SingleFileDB {
  /// Set a vector embedding for a node
  ///
  /// Each property key can have its own vector store with different dimensions.
  /// The first vector set for a property key determines the dimension.
  pub fn set_node_vector(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    vector: &[f32],
  ) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Check dimensions if store already exists
    {
      let stores = self.vector_stores.read();
      if let Some(store) = stores.get(&prop_key_id) {
        if store.config.dimensions != vector.len() {
          return Err(RayError::VectorDimensionMismatch {
            expected: store.config.dimensions,
            got: vector.len(),
          });
        }
      }
    }

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetNodeVector,
      txid,
      build_set_node_vector_payload(node_id, prop_key_id, vector),
    );
    self.write_wal(record)?;

    // Queue in delta for commit
    self
      .delta
      .write()
      .pending_vectors
      .insert((node_id, prop_key_id), Some(vector.to_vec()));

    Ok(())
  }

  /// Delete a vector embedding for a node
  ///
  /// Returns Ok(()) even if the vector doesn't exist (idempotent).
  pub fn delete_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelNodeVector,
      txid,
      build_del_node_vector_payload(node_id, prop_key_id),
    );
    self.write_wal(record)?;

    // Queue delete in delta
    self
      .delta
      .write()
      .pending_vectors
      .insert((node_id, prop_key_id), None); // None means delete

    Ok(())
  }

  /// Get a vector embedding for a node
  ///
  /// Checks pending operations first, then falls back to committed storage.
  pub fn get_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Option<Vec<f32>> {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return None;
    }

    // Check pending operations first
    if let Some(pending) = delta.pending_vectors.get(&(node_id, prop_key_id)) {
      // Some(vec) = set, None = delete
      return pending.clone();
    }

    // Fall back to committed storage
    let stores = self.vector_stores.read();
    let store = stores.get(&prop_key_id)?;
    vector_store_get(store, node_id).map(|v| v.to_vec())
  }

  /// Check if a node has a vector embedding
  pub fn has_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> bool {
    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return false;
    }

    // Check pending operations first
    if let Some(pending) = delta.pending_vectors.get(&(node_id, prop_key_id)) {
      return pending.is_some();
    }

    // Fall back to committed storage
    let stores = self.vector_stores.read();
    if let Some(store) = stores.get(&prop_key_id) {
      return vector_store_has(store, node_id);
    }

    false
  }

  /// Get or create a vector store for a property key
  ///
  /// Creates a new store with the given dimensions if it doesn't exist.
  pub fn get_or_create_vector_store(
    &self,
    prop_key_id: PropKeyId,
    dimensions: usize,
  ) -> Result<()> {
    let mut stores = self.vector_stores.write();
    if stores.contains_key(&prop_key_id) {
      let store = stores.get(&prop_key_id).unwrap();
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

  /// Apply pending vector operations (called during commit)
  pub(crate) fn apply_pending_vectors(&self) {
    let mut delta = self.delta.write();
    let pending: Vec<_> = delta.pending_vectors.drain().collect();
    drop(delta);

    let mut stores = self.vector_stores.write();

    for ((node_id, prop_key_id), operation) in pending {
      match operation {
        Some(vector) => {
          // Set operation - get or create store
          let store = stores.entry(prop_key_id).or_insert_with(|| {
            let config = VectorStoreConfig::new(vector.len());
            create_vector_store(config)
          });

          // Insert (this handles replacement of existing vectors)
          let _ = vector_store_insert(store, node_id, &vector);
        }
        None => {
          // Delete operation
          if let Some(store) = stores.get_mut(&prop_key_id) {
            vector_store_delete(store, node_id);
          }
        }
      }
    }
  }
}

pub(crate) fn vector_stores_from_snapshot(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  let mut stores: HashMap<PropKeyId, VectorManifest> = HashMap::new();

  if !snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
    return Ok(stores);
  }

  let num_nodes = snapshot.header.num_nodes as usize;
  for phys in 0..num_nodes {
    let node_id = match snapshot.get_node_id(phys as u32) {
      Some(id) => id,
      None => continue,
    };

    let Some(props) = snapshot.get_node_props(phys as u32) else {
      continue;
    };

    for (key_id, value) in props {
      if let PropValue::VectorF32(vec) = value {
        let store = stores.entry(key_id).or_insert_with(|| {
          let config = VectorStoreConfig::new(vec.len());
          create_vector_store(config)
        });

        if store.config.dimensions != vec.len() {
          return Err(RayError::InvalidSnapshot(format!(
            "Vector dimension mismatch for prop key {key_id}: expected {}, got {}",
            store.config.dimensions,
            vec.len()
          )));
        }

        vector_store_insert(store, node_id, &vec).map_err(|e| {
          RayError::InvalidSnapshot(format!(
            "Failed to insert vector for node {node_id} (prop {key_id}): {e}"
          ))
        })?;
      }
    }
  }

  Ok(stores)
}

#[cfg(test)]
mod tests {
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::vector::distance::normalize;
  use tempfile::tempdir;

  #[test]
  fn test_vector_persistence_across_checkpoint() {
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("vectors.raydb");

    // Create DB and insert a vector
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    db.begin(false).unwrap();
    let node_id = db.create_node(None).unwrap();
    let prop_key_id = db.define_propkey("embedding").unwrap();
    db.set_node_vector(node_id, prop_key_id, &[0.1, 0.2, 0.3])
      .unwrap();
    db.commit().unwrap();

    // Force checkpoint to persist snapshot
    db.checkpoint().unwrap();
    close_single_file(db).unwrap();

    // Reopen and verify vector is restored from snapshot
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    let vec = db.get_node_vector(node_id, prop_key_id).unwrap();
    let expected = normalize(&[0.1, 0.2, 0.3]);
    assert_eq!(vec.len(), expected.len());
    for (got, exp) in vec.iter().zip(expected.iter()) {
      assert!((got - exp).abs() < 1e-6);
    }
    close_single_file(db).unwrap();
  }
}
