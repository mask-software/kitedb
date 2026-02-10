//! Vector operations for SingleFileDB
//!
//! Handles vector embedding storage and retrieval for nodes.

use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::record::{
  build_del_node_vector_payload, build_set_node_vector_payload, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::{read_u32, read_u64};
use crate::vector::ivf::serialize::deserialize_manifest;
use crate::vector::store::{
  create_vector_store, validate_vector, vector_store_delete, vector_store_has, vector_store_insert,
  vector_store_node_vector,
};
use crate::vector::types::{VectorManifest, VectorStoreConfig};
use std::collections::HashMap;
use std::sync::Arc;

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
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Check dimensions if store already exists
    {
      let stores = self.vector_stores.read();
      if let Some(store) = stores.get(&prop_key_id) {
        if store.config.dimensions != vector.len() {
          return Err(KiteError::VectorDimensionMismatch {
            expected: store.config.dimensions,
            got: vector.len(),
          });
        }
      }
    }

    // If the store doesn't exist yet, enforce dimensions against any pending vector
    // operations for the same property key in this transaction.
    {
      let tx = tx_handle.lock();
      for (&(_pending_node_id, pending_prop_key_id), pending_op) in &tx.pending.pending_vectors {
        if pending_prop_key_id != prop_key_id {
          continue;
        }
        let Some(existing) = pending_op.as_ref() else {
          continue;
        };
        if existing.len() != vector.len() {
          return Err(KiteError::VectorDimensionMismatch {
            expected: existing.len(),
            got: vector.len(),
          });
        }
        break;
      }
    }

    // Validate vector before WAL write / queuing pending ops.
    validate_vector(vector).map_err(|e| KiteError::InvalidQuery(e.to_string().into()))?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetNodeVector,
      txid,
      build_set_node_vector_payload(node_id, prop_key_id, vector),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Queue in pending delta for commit
    {
      let mut tx = tx_handle.lock();
      tx.pending.pending_vectors.insert(
        (node_id, prop_key_id),
        Some(VectorRef::from(vector.to_vec())),
      );
    }

    Ok(())
  }

  /// Delete a vector embedding for a node
  ///
  /// Returns Ok(()) even if the vector doesn't exist (idempotent).
  pub fn delete_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelNodeVector,
      txid,
      build_del_node_vector_payload(node_id, prop_key_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Queue delete in pending delta
    {
      let mut tx = tx_handle.lock();
      tx.pending
        .pending_vectors
        .insert((node_id, prop_key_id), None); // None means delete
    }

    Ok(())
  }

  /// Get a vector embedding for a node
  ///
  /// Checks pending operations first, then falls back to committed storage.
  pub fn node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Option<VectorRef> {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_deleted(node_id) {
        return None;
      }
      if let Some(pending) = tx.pending.pending_vectors.get(&(node_id, prop_key_id)) {
        return pending.as_ref().map(Arc::clone);
      }
    }

    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return None;
    }

    // Check pending operations from committed replay (startup)
    if let Some(pending) = delta.pending_vectors.get(&(node_id, prop_key_id)) {
      // Some(vec) = set, None = delete
      return pending.as_ref().map(Arc::clone);
    }

    // Fall back to committed storage
    let stores = self.vector_stores.read();
    let store = stores.get(&prop_key_id)?;
    vector_store_node_vector(store, node_id).map(Arc::from)
  }

  /// Check if a node has a vector embedding
  pub fn has_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> bool {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_deleted(node_id) {
        return false;
      }
      if let Some(pending) = tx.pending.pending_vectors.get(&(node_id, prop_key_id)) {
        return pending.is_some();
      }
    }

    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_deleted(node_id) {
      return false;
    }

    // Check pending operations from committed replay (startup)
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
  pub fn vector_store_or_create(&self, prop_key_id: PropKeyId, dimensions: usize) -> Result<()> {
    let mut stores = self.vector_stores.write();
    if stores.contains_key(&prop_key_id) {
      let store = stores.get(&prop_key_id).ok_or_else(|| {
        KiteError::Internal("vector store missing after contains_key".to_string())
      })?;
      if store.config.dimensions != dimensions {
        return Err(KiteError::VectorDimensionMismatch {
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
  pub(crate) fn apply_pending_vectors(
    &self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  ) -> Result<()> {
    let mut stores = self.vector_stores.write();

    for (&(node_id, prop_key_id), operation) in pending_vectors {
      match operation {
        Some(vector) => {
          // Set operation - get or create store
          let store = stores.entry(prop_key_id).or_insert_with(|| {
            let config = VectorStoreConfig::new(vector.len());
            create_vector_store(config)
          });

          // Insert (this handles replacement of existing vectors)
          vector_store_insert(store, node_id, vector.as_ref()).map_err(|e| {
            KiteError::Internal(format!(
              "Failed to apply vector insert during commit for node {node_id} (prop {prop_key_id}): {e}"
            ))
          })?;
        }
        None => {
          // Delete operation
          if let Some(store) = stores.get_mut(&prop_key_id) {
            vector_store_delete(store, node_id);
          }
        }
      }
    }

    Ok(())
  }
}

pub(crate) fn vector_stores_from_snapshot(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  if snapshot
    .header
    .flags
    .contains(SnapshotFlags::HAS_VECTOR_STORES)
  {
    return vector_stores_from_sections(snapshot);
  }

  let mut stores = vector_stores_from_sections(snapshot)?;
  if !stores.is_empty() {
    return Ok(stores);
  }

  if !snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
    return Ok(stores);
  }

  let num_nodes = snapshot.header.num_nodes as usize;
  for phys in 0..num_nodes {
    let node_id = match snapshot.node_id(phys as u32) {
      Some(id) => id,
      None => continue,
    };

    let Some(props) = snapshot.node_props(phys as u32) else {
      continue;
    };

    for (key_id, value) in props {
      if let PropValue::VectorF32(vec) = value {
        let store = stores.entry(key_id).or_insert_with(|| {
          let config = VectorStoreConfig::new(vec.len());
          create_vector_store(config)
        });

        if store.config.dimensions != vec.len() {
          return Err(KiteError::InvalidSnapshot(format!(
            "Vector dimension mismatch for prop key {key_id}: expected {}, got {}",
            store.config.dimensions,
            vec.len()
          )));
        }

        vector_store_insert(store, node_id, &vec).map_err(|e| {
          KiteError::InvalidSnapshot(format!(
            "Failed to insert vector for node {node_id} (prop {key_id}): {e}"
          ))
        })?;
      }
    }
  }

  Ok(stores)
}

fn vector_stores_from_sections(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  let mut stores: HashMap<PropKeyId, VectorManifest> = HashMap::new();
  let Some(index_bytes) = snapshot.section_bytes(SectionId::VectorStoreIndex) else {
    return Ok(stores);
  };
  let Some(blob_bytes) = snapshot.section_bytes(SectionId::VectorStoreData) else {
    return Err(KiteError::InvalidSnapshot(
      "Vector store index present but vector store blob section is missing".to_string(),
    ));
  };

  if index_bytes.len() < 4 {
    return Err(KiteError::InvalidSnapshot(
      "Vector store index section too small".to_string(),
    ));
  }

  let count = read_u32(&index_bytes, 0) as usize;
  let expected_len = 4usize
    .checked_add(count.saturating_mul(20))
    .ok_or_else(|| KiteError::InvalidSnapshot("Vector store index size overflow".to_string()))?;
  if index_bytes.len() < expected_len {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector store index truncated: expected at least {expected_len} bytes, found {}",
      index_bytes.len()
    )));
  }

  for i in 0..count {
    let entry_offset = 4 + i * 20;
    let prop_key_id = read_u32(&index_bytes, entry_offset);
    let payload_offset = read_u64(&index_bytes, entry_offset + 4) as usize;
    let payload_len = read_u64(&index_bytes, entry_offset + 12) as usize;
    let payload_end = payload_offset.checked_add(payload_len).ok_or_else(|| {
      KiteError::InvalidSnapshot(format!(
        "Vector store entry {i} overflow: offset={payload_offset}, len={payload_len}"
      ))
    })?;
    if payload_end > blob_bytes.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Vector store entry {i} out of bounds: {}..{} exceeds blob size {}",
        payload_offset,
        payload_end,
        blob_bytes.len()
      )));
    }

    let manifest =
      deserialize_manifest(&blob_bytes[payload_offset..payload_end]).map_err(|err| {
        KiteError::InvalidSnapshot(format!(
          "Failed to deserialize vector store for prop key {prop_key_id}: {err}"
        ))
      })?;

    if stores.insert(prop_key_id, manifest).is_some() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Duplicate vector store entry for prop key {prop_key_id}"
      )));
    }
  }

  Ok(stores)
}

#[cfg(test)]
mod tests {
  use super::vector_stores_from_snapshot;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::core::snapshot::reader::SnapshotData;
  use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, SnapshotBuildInput};
  use crate::types::{PropValue, SnapshotFlags};
  use crate::vector::distance::normalize;
  use crate::vector::store::{create_vector_store, vector_store_has, vector_store_insert};
  use crate::vector::types::VectorStoreConfig;
  use std::collections::HashMap;
  use std::io::Write;
  use tempfile::{tempdir, NamedTempFile};

  #[test]
  fn test_set_node_vector_rejects_invalid_vectors() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("invalid-vectors.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    db.begin(false).expect("expected value");

    let node_id = db.create_node(None).expect("expected value");
    let prop_key_id = db.define_propkey("embedding").expect("expected value");

    // All-zero vector should be rejected (would otherwise be silently dropped on commit).
    assert!(db
      .set_node_vector(node_id, prop_key_id, &[0.0, 0.0, 0.0])
      .is_err());

    // NaN should be rejected.
    assert!(db
      .set_node_vector(node_id, prop_key_id, &[0.1, f32::NAN, 0.3])
      .is_err());

    db.rollback().expect("expected value");
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_vector_persistence_across_checkpoint() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vectors.kitedb");

    // Create DB and insert a vector
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    db.begin(false).expect("expected value");
    let node_id = db.create_node(None).expect("expected value");
    let prop_key_id = db.define_propkey("embedding").expect("expected value");
    db.set_node_vector(node_id, prop_key_id, &[0.1, 0.2, 0.3])
      .expect("expected value");
    db.commit().expect("expected value");

    // Force checkpoint to persist snapshot
    db.checkpoint().expect("expected value");
    close_single_file(db).expect("expected value");

    // Reopen and verify vector is restored from snapshot
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    let vec = db
      .node_vector(node_id, prop_key_id)
      .expect("expected value");
    let expected = normalize(&[0.1, 0.2, 0.3]);
    assert_eq!(vec.len(), expected.len());
    for (got, exp) in vec.iter().zip(expected.iter()) {
      assert!((got - exp).abs() < 1e-6);
    }
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_vector_persistence_across_wal_replay() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vectors-wal.kitedb");

    // Commit without checkpoint; close; reopen; expect WAL replay to restore vectors.
    let options = SingleFileOpenOptions::new().auto_checkpoint(false);
    let db = open_single_file(&db_path, options.clone()).expect("expected value");
    db.begin(false).expect("expected value");
    let node_id = db.create_node(None).expect("expected value");
    let prop_key_id = db.define_propkey("embedding").expect("expected value");
    db.set_node_vector(node_id, prop_key_id, &[0.1, 0.2, 0.3])
      .expect("expected value");
    db.commit().expect("expected value");
    close_single_file(db).expect("expected value");

    let db = open_single_file(&db_path, options).expect("expected value");
    let vec = db
      .node_vector(node_id, prop_key_id)
      .expect("expected value");
    let expected = normalize(&[0.1, 0.2, 0.3]);
    assert_eq!(vec.len(), expected.len());
    for (got, exp) in vec.iter().zip(expected.iter()) {
      assert!((got - exp).abs() < 1e-6);
    }
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_vector_store_sections_round_trip() {
    let mut manifest = create_vector_store(VectorStoreConfig::new(3));
    vector_store_insert(&mut manifest, 42, &[0.1, 0.2, 0.3]).expect("expected value");

    let mut stores = HashMap::new();
    stores.insert(7, manifest);

    let mut propkeys = HashMap::new();
    propkeys.insert(7, "embedding".to_string());

    let buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 42,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys,
      vector_stores: Some(stores),
      compression: None,
    })
    .expect("expected value");

    let mut tmp = NamedTempFile::new().expect("expected value");
    tmp.write_all(&buffer).expect("expected value");
    tmp.flush().expect("expected value");

    let snapshot = SnapshotData::load(tmp.path()).expect("expected value");
    assert!(snapshot
      .header
      .flags
      .contains(SnapshotFlags::HAS_VECTOR_STORES));
    assert!(!snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS));

    let loaded = vector_stores_from_snapshot(&snapshot).expect("expected value");
    let loaded_manifest = loaded.get(&7).expect("expected value");
    assert!(vector_store_has(loaded_manifest, 42));

    // Verify the legacy property path remains empty when vectors are only
    // materialized via persisted vector-store sections.
    let phys = snapshot.phys_node(42).expect("expected value");
    assert!(!matches!(
      snapshot.node_prop(phys, 7),
      Some(PropValue::VectorF32(_))
    ));
  }
}
