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
use crate::util::binary::{read_u32_at, read_u64, read_u64_at};
use crate::vector::store::{
  create_vector_store, validate_vector, vector_store_delete, vector_store_has, vector_store_insert,
  vector_store_node_vector,
};
use crate::vector::types::{VectorManifest, VectorStoreConfig};
use std::collections::HashMap;
use std::sync::Arc;

use super::SingleFileDB;

#[derive(Debug, Clone)]
pub(crate) struct VectorStoreLazyEntry {
  pub(crate) offset: usize,
  pub(crate) len: usize,
}

impl SingleFileDB {
  pub(crate) fn ensure_vector_store_loaded(&self, prop_key_id: PropKeyId) -> Result<()> {
    if self.vector_stores.read().contains_key(&prop_key_id) {
      return Ok(());
    }

    let entry = {
      let lazy_entries = self.vector_store_lazy_entries.read();
      lazy_entries.get(&prop_key_id).cloned()
    };
    let Some(entry) = entry else {
      return Ok(());
    };

    let manifest = {
      let snapshot_guard = self.snapshot.read();
      let snapshot = snapshot_guard.as_ref().ok_or_else(|| {
        KiteError::Internal("lazy vector-store entry present without loaded snapshot".to_string())
      })?;
      deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?
    };

    {
      let mut stores = self.vector_stores.write();
      stores.entry(prop_key_id).or_insert(manifest);
    }
    self.vector_store_lazy_entries.write().remove(&prop_key_id);
    Ok(())
  }

  pub(crate) fn materialize_all_vector_stores(&self) -> Result<()> {
    let prop_keys: Vec<PropKeyId> = self
      .vector_store_lazy_entries
      .read()
      .keys()
      .copied()
      .collect();
    for prop_key_id in prop_keys {
      self.ensure_vector_store_loaded(prop_key_id)?;
    }
    Ok(())
  }

  pub(crate) fn vector_prop_keys(&self) -> std::collections::HashSet<PropKeyId> {
    let mut keys: std::collections::HashSet<PropKeyId> =
      self.vector_stores.read().keys().copied().collect();
    keys.extend(self.vector_store_lazy_entries.read().keys().copied());
    keys
  }

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
    self.ensure_vector_store_loaded(prop_key_id)?;

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

    if self.ensure_vector_store_loaded(prop_key_id).is_err() {
      return None;
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

    if self.ensure_vector_store_loaded(prop_key_id).is_err() {
      return false;
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
    self.ensure_vector_store_loaded(prop_key_id)?;

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
    self.vector_store_lazy_entries.write().remove(&prop_key_id);
    Ok(())
  }

  /// Apply pending vector operations (called during commit)
  pub(crate) fn apply_pending_vectors(
    &self,
    pending_vectors: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  ) -> Result<()> {
    let mut prop_keys = std::collections::HashSet::new();
    for &(_node_id, prop_key_id) in pending_vectors.keys() {
      prop_keys.insert(prop_key_id);
    }
    for prop_key_id in prop_keys {
      self.ensure_vector_store_loaded(prop_key_id)?;
    }

    let mut stores = self.vector_stores.write();

    for (&(node_id, prop_key_id), operation) in pending_vectors {
      match operation {
        Some(vector) => {
          // Set operation - get or create store
          let store = stores.entry(prop_key_id).or_insert_with(|| {
            let config = VectorStoreConfig::new(vector.len());
            create_vector_store(config)
          });
          self.vector_store_lazy_entries.write().remove(&prop_key_id);

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

pub(crate) fn vector_store_state_from_snapshot(
  snapshot: &SnapshotData,
) -> Result<(
  HashMap<PropKeyId, VectorManifest>,
  HashMap<PropKeyId, VectorStoreLazyEntry>,
)> {
  if !snapshot
    .header
    .flags
    .contains(SnapshotFlags::HAS_VECTOR_STORES)
    && !snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS)
  {
    return Ok((HashMap::new(), HashMap::new()));
  }

  if snapshot
    .header
    .flags
    .contains(SnapshotFlags::HAS_VECTOR_STORES)
  {
    let lazy_entries = vector_store_lazy_entries_from_sections(snapshot)?;
    return Ok((HashMap::new(), lazy_entries));
  }

  let mut stores = vector_stores_from_sections(snapshot)?;
  if !stores.is_empty() {
    return Ok((stores, HashMap::new()));
  }

  if !snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
    return Ok((stores, HashMap::new()));
  }

  let Some(node_prop_offsets) = snapshot.section_data_shared(SectionId::NodePropOffsets) else {
    return Ok(stores);
  };
  let Some(node_prop_keys) = snapshot.section_data_shared(SectionId::NodePropKeys) else {
    return Ok(stores);
  };
  let Some(node_prop_vals) = snapshot.section_data_shared(SectionId::NodePropVals) else {
    return Ok(stores);
  };
  let Some(vector_offsets) = snapshot.section_data_shared(SectionId::VectorOffsets) else {
    return Ok(stores);
  };
  let Some(vector_data) = snapshot.section_data_shared(SectionId::VectorData) else {
    return Ok(stores);
  };

  let node_prop_offsets = node_prop_offsets.as_ref();
  let node_prop_keys = node_prop_keys.as_ref();
  let node_prop_vals = node_prop_vals.as_ref();
  let vector_offsets = vector_offsets.as_ref();
  let vector_data = vector_data.as_ref();

  let num_nodes = snapshot.header.num_nodes as usize;
  for phys in 0..num_nodes {
    if phys * 4 + 8 > node_prop_offsets.len() {
      break;
    }

    let node_id = match snapshot.node_id(phys as u32) {
      Some(id) => id,
      None => continue,
    };

    let start = read_u32_at(node_prop_offsets, phys) as usize;
    let end = read_u32_at(node_prop_offsets, phys + 1) as usize;
    if end < start {
      return Err(KiteError::InvalidSnapshot(format!(
        "Node property range invalid for phys={phys}: start={start}, end={end}"
      )));
    }

    for i in start..end {
      let key_offset = i * 4;
      let val_offset = i * PROP_VALUE_DISK_SIZE;
      if key_offset + 4 > node_prop_keys.len()
        || val_offset + PROP_VALUE_DISK_SIZE > node_prop_vals.len()
      {
        return Err(KiteError::InvalidSnapshot(format!(
          "Node property entry out of bounds for phys={phys}, entry={i}"
        )));
      }

      if PropValueTag::from_u8(node_prop_vals[val_offset]) != Some(PropValueTag::VectorF32) {
        continue;
      }

      let key_id = read_u32_at(node_prop_keys, i);
      let vector_idx = read_u64(node_prop_vals, val_offset + 8) as usize;
      let vec = decode_vector_payload(vector_offsets, vector_data, vector_idx)?;

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

  Ok((stores, HashMap::new()))
}

pub(crate) fn vector_stores_from_snapshot(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  let (stores, lazy_entries) = vector_store_state_from_snapshot(snapshot)?;
  if lazy_entries.is_empty() {
    return Ok(stores);
  }

  let mut materialized = stores;
  for (prop_key_id, entry) in lazy_entries {
    let manifest = deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?;
    materialized.insert(prop_key_id, manifest);
  }
  Ok(materialized)
}

pub(crate) fn materialize_vector_store_from_lazy_entries(
  snapshot: &SnapshotData,
  vector_stores: &mut HashMap<PropKeyId, VectorManifest>,
  lazy_entries: &mut HashMap<PropKeyId, VectorStoreLazyEntry>,
  prop_key_id: PropKeyId,
) -> Result<()> {
  if vector_stores.contains_key(&prop_key_id) {
    return Ok(());
  }
  let Some(entry) = lazy_entries.remove(&prop_key_id) else {
    return Ok(());
  };
  let manifest = deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?;
  vector_stores.insert(prop_key_id, manifest);
  Ok(())
}

fn vector_stores_from_sections(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorManifest>> {
  let lazy_entries = vector_store_lazy_entries_from_sections(snapshot)?;
  if lazy_entries.is_empty() {
    return Ok(HashMap::new());
  }

  let mut stores: HashMap<PropKeyId, VectorManifest> = HashMap::new();
  for (prop_key_id, entry) in lazy_entries {
    let manifest = deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?;
    stores.insert(prop_key_id, manifest);
  }
  Ok(stores)
}

fn vector_store_lazy_entries_from_sections(
  snapshot: &SnapshotData,
) -> Result<HashMap<PropKeyId, VectorStoreLazyEntry>> {
  let mut entries: HashMap<PropKeyId, VectorStoreLazyEntry> = HashMap::new();
  let Some(index_bytes) = snapshot.section_data_shared(SectionId::VectorStoreIndex) else {
    return Ok(entries);
  };
  let Some(blob_bytes) = snapshot.section_data_shared(SectionId::VectorStoreData) else {
    return Err(KiteError::InvalidSnapshot(
      "Vector store index present but vector store blob section is missing".to_string(),
    ));
  };

  let index_bytes = index_bytes.as_ref();
  let blob_len = blob_bytes.as_ref().len();

  if index_bytes.len() < 4 {
    return Err(KiteError::InvalidSnapshot(
      "Vector store index section too small".to_string(),
    ));
  }

  let count = read_u32(index_bytes, 0) as usize;
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
    let prop_key_id = read_u32(index_bytes, entry_offset);
    let payload_offset = read_u64(index_bytes, entry_offset + 4) as usize;
    let payload_len = read_u64(index_bytes, entry_offset + 12) as usize;
    let payload_end = payload_offset.checked_add(payload_len).ok_or_else(|| {
      KiteError::InvalidSnapshot(format!(
        "Vector store entry {i} overflow: offset={payload_offset}, len={payload_len}"
      ))
    })?;
    if payload_end > blob_len {
      return Err(KiteError::InvalidSnapshot(format!(
        "Vector store entry {i} out of bounds: {}..{} exceeds blob size {}",
        payload_offset, payload_end, blob_len
      )));
    }

    let entry = VectorStoreLazyEntry {
      offset: payload_offset,
      len: payload_len,
    };
    if entries.insert(prop_key_id, entry).is_some() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Duplicate vector store entry for prop key {prop_key_id}"
      )));
    }
  }

  Ok(entries)
}

fn deserialize_vector_store_entry(
  snapshot: &SnapshotData,
  prop_key_id: PropKeyId,
  entry: &VectorStoreLazyEntry,
) -> Result<VectorManifest> {
  let blob_bytes = snapshot
    .section_data_shared(SectionId::VectorStoreData)
    .ok_or_else(|| {
      KiteError::InvalidSnapshot(
        "Vector store entry present but vector store blob section is missing".to_string(),
      )
    })?;
  let blob_bytes = blob_bytes.as_ref();

  let payload_end = entry.offset.checked_add(entry.len).ok_or_else(|| {
    KiteError::InvalidSnapshot(format!(
      "Vector store entry overflow for prop key {prop_key_id}: offset={}, len={}",
      entry.offset, entry.len
    ))
  })?;
  if payload_end > blob_bytes.len() {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector store entry for prop key {prop_key_id} out of bounds: {}..{} exceeds blob size {}",
      entry.offset,
      payload_end,
      blob_bytes.len()
    )));
  }

  deserialize_manifest(&blob_bytes[entry.offset..payload_end]).map_err(|err| {
    KiteError::InvalidSnapshot(format!(
      "Failed to deserialize vector store for prop key {prop_key_id}: {err}"
    ))
  })
fn decode_vector_payload(
  vector_offsets: &[u8],
  vector_data: &[u8],
  idx: usize,
) -> Result<Vec<f32>> {
  if (idx + 1) * 8 > vector_offsets.len() {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector index out of range: {idx}"
    )));
  }

  let start = read_u64_at(vector_offsets, idx) as usize;
  let end = read_u64_at(vector_offsets, idx + 1) as usize;
  if start > end || end > vector_data.len() {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector range invalid for idx={idx}: start={start}, end={end}, data_len={}",
      vector_data.len()
    )));
  }

  let bytes = &vector_data[start..end];
  if bytes.len() % 4 != 0 {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector byte length is not multiple of 4 for idx={idx}: {}",
      bytes.len()
    )));
  }

  let mut vec = Vec::with_capacity(bytes.len() / 4);
  for chunk in bytes.chunks_exact(4) {
    vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
  }
  Ok(vec)
}

#[cfg(test)]
mod tests {
  use super::{vector_store_state_from_snapshot, vector_stores_from_snapshot};
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::core::snapshot::reader::SnapshotData;
  use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, SnapshotBuildInput};
  use crate::types::{
    PropValue, SectionId, SnapshotFlags, SECTION_ENTRY_SIZE, SNAPSHOT_HEADER_SIZE,
  };
  use crate::util::binary::{read_u64, write_u32, write_u64};
  use crate::util::crc::crc32c;
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
  fn test_open_keeps_vector_store_lazy_until_first_access() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vectors-lazy-open.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    db.begin(false).expect("expected value");
    let node_id = db.create_node(None).expect("expected value");
    let prop_key_id = db.define_propkey("embedding").expect("expected value");
    db.set_node_vector(node_id, prop_key_id, &[0.1, 0.2, 0.3])
      .expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");
    close_single_file(db).expect("expected value");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    assert!(db.vector_stores.read().is_empty());

    let vec = db
      .node_vector(node_id, prop_key_id)
      .expect("expected value");
    let expected = normalize(&[0.1, 0.2, 0.3]);
    assert_eq!(vec.len(), expected.len());
    for (got, exp) in vec.iter().zip(expected.iter()) {
      assert!((got - exp).abs() < 1e-6);
    }
    assert!(db.vector_stores.read().contains_key(&prop_key_id));
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

  #[test]
  fn test_checkpoint_does_not_duplicate_vectors_into_node_props() {
    let temp_dir = tempdir().expect("expected value");
    let db_path = temp_dir.path().join("vectors-no-dup-node-prop.kitedb");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    db.begin(false).expect("expected value");
    let node_id = db.create_node(None).expect("expected value");
    let prop_key_id = db.define_propkey("embedding").expect("expected value");
    db.set_node_vector(node_id, prop_key_id, &[0.1, 0.2, 0.3])
      .expect("expected value");
    db.commit().expect("expected value");
    db.checkpoint().expect("expected value");
    close_single_file(db).expect("expected value");

    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("expected value");
    let snapshot_guard = db.snapshot.read();
    let snapshot = snapshot_guard.as_ref().expect("expected value");
    let phys = snapshot.phys_node(node_id).expect("expected value");
    assert!(!matches!(
      snapshot.node_prop(phys, prop_key_id),
      Some(PropValue::VectorF32(_))
    ));
    drop(snapshot_guard);
    close_single_file(db).expect("expected value");
  }

  #[test]
  fn test_no_vector_flags_ignore_vector_sections() {
    let mut buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 1,
        key: None,
        labels: vec![],
        props: HashMap::new(),
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::new(),
      vector_stores: None,
      compression: None,
    })
    .expect("expected value");

    // Corrupt-insert a VectorStoreIndex section table entry while keeping
    // HAS_VECTOR_STORES/HAS_VECTORS flags unset. Loader should ignore it.
    let entry_offset =
      SNAPSHOT_HEADER_SIZE + (SectionId::VectorStoreIndex as usize) * SECTION_ENTRY_SIZE;
    let mut prev_end = 0u64;
    for section_idx in 0..(SectionId::VectorStoreIndex as usize) {
      let sec_entry = SNAPSHOT_HEADER_SIZE + section_idx * SECTION_ENTRY_SIZE;
      let offset = read_u64(&buffer, sec_entry);
      let len = read_u64(&buffer, sec_entry + 8);
      if len > 0 {
        prev_end = prev_end.max(offset + len);
      }
    }
    let fake_payload_offset = ((prev_end + 63) / 64) * 64;
    let required_size = (((fake_payload_offset + 1 + 63) / 64) * 64 + 4) as usize;
    if buffer.len() < required_size {
      buffer.resize(required_size, 0);
    }
    write_u64(&mut buffer, entry_offset, fake_payload_offset);
    write_u64(&mut buffer, entry_offset + 8, 1);
    write_u32(&mut buffer, entry_offset + 16, 0);
    write_u32(&mut buffer, entry_offset + 20, 1);
    let crc_offset = buffer.len() - 4;
    let crc = crc32c(&buffer[..crc_offset]);
    write_u32(&mut buffer, crc_offset, crc);

    let mut tmp = NamedTempFile::new().expect("expected value");
    tmp.write_all(&buffer).expect("expected value");
    tmp.flush().expect("expected value");

    let snapshot = SnapshotData::load(tmp.path()).expect("expected value");
    assert!(!snapshot
      .header
      .flags
      .contains(SnapshotFlags::HAS_VECTOR_STORES));
    assert!(!snapshot.header.flags.contains(SnapshotFlags::HAS_VECTORS));

    let (stores, lazy_entries) =
      vector_store_state_from_snapshot(&snapshot).expect("expected value");
    assert!(stores.is_empty());
    assert!(lazy_entries.is_empty());
  }
}
