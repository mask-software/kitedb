//! Vector operations for SingleFileDB
//!
//! Handles vector embedding storage and retrieval for nodes.

use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::record::{
  build_del_node_vector_payload, build_set_node_vector_payload, parse_set_node_vector_payload,
  ParsedWalRecord, WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::{read_u32, read_u64};
use crate::util::binary::{read_u32_at, read_u64_at};
use crate::vector::distance::normalize_in_place;
use crate::vector::ivf::serialize::deserialize_manifest;
use crate::vector::store::{
  create_vector_store, validate_vector, vector_store_delete, vector_store_has, vector_store_insert,
  vector_store_node_vector, VectorStoreError,
};
use crate::vector::types::{VectorManifest, VectorStoreConfig};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use super::{SingleFileDB, SingleFileTxState};

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

    // A lazy entry is an offset into the snapshot installed with it, and a
    // checkpoint install replaces both while holding `snapshot.write()`; so
    // read the entry, decode it, and store the result under one snapshot
    // guard (lock order `snapshot` -> vector stores, as in the install).
    let snapshot_guard = self.snapshot.read();
    let entry = {
      let lazy_entries = self.vector_store_lazy_entries.read();
      lazy_entries.get(&prop_key_id).cloned()
    };
    let Some(entry) = entry else {
      return Ok(());
    };
    let snapshot = snapshot_guard.as_ref().ok_or_else(|| {
      KiteError::Internal("lazy vector-store entry present without loaded snapshot".to_string())
    })?;
    let manifest = deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?;
    self
      .vector_stores
      .write()
      .entry(prop_key_id)
      .or_insert(manifest);
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

  /// Property keys under which `node_id` holds a vector, as the transaction
  /// sees it: its pending operations, then committed ones, then the stores.
  pub(crate) fn node_vector_keys(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    node_id: NodeId,
  ) -> Result<Vec<PropKeyId>> {
    let node_ops = |ops: &HashMap<(NodeId, PropKeyId), Option<VectorRef>>| {
      ops
        .iter()
        .filter(|((op_node_id, _), _)| *op_node_id == node_id)
        .map(|(&(_, prop_key_id), op)| (prop_key_id, op.is_some()))
        .collect::<HashMap<PropKeyId, bool>>()
    };
    let mut ops = node_ops(&self.delta.read().pending_vectors);
    ops.extend(node_ops(&tx_handle.lock().pending.pending_vectors));

    let mut keys = self.vector_prop_keys();
    keys.extend(ops.keys().copied());
    let mut with_vector = Vec::new();
    for prop_key_id in keys {
      let has_vector = match ops.get(&prop_key_id) {
        Some(&is_set) => is_set,
        None => {
          self.ensure_vector_store_loaded(prop_key_id)?;
          self
            .vector_stores
            .read()
            .get(&prop_key_id)
            .is_some_and(|store| vector_store_has(store, node_id))
        }
      };
      if has_vector {
        with_vector.push(prop_key_id);
      }
    }
    Ok(with_vector)
  }

  /// Set a vector embedding for a node
  ///
  /// Each property key can have its own vector store with different dimensions.
  /// The first vector set for a property key determines the dimension.
  ///
  /// Fails with `NodeNotFound` if the transaction does not see the node.
  pub fn set_node_vector(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    vector: &[f32],
  ) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;
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
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.pending_vectors.insert(
        (node_id, prop_key_id),
        Some(VectorRef::from(vector.to_vec())),
      );
      tx.bulk_load
    };
    // The vector needs its node: a concurrent delete_node conflicts.
    if !bulk_load {
      self.record_read(txid, TxKey::Node(node_id));
    }

    Ok(())
  }

  /// Delete a vector embedding for a node
  ///
  /// Returns Ok(()) even if the vector doesn't exist (idempotent), but fails
  /// with `NodeNotFound` if the transaction does not see the node.
  pub fn delete_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;
    self.log_delete_node_vector(txid, &tx_handle, node_id, prop_key_id)?;
    if !tx_handle.lock().bulk_load {
      self.record_read(txid, TxKey::Node(node_id));
    }
    Ok(())
  }

  /// Log and queue a vector delete, without checking the node.
  pub(super) fn log_delete_node_vector(
    &self,
    txid: TxId,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Result<()> {
    let record = WalRecord::new(
      WalRecordType::DelNodeVector,
      txid,
      build_del_node_vector_payload(node_id, prop_key_id),
    );
    self.write_wal_tx(tx_handle, record)?;

    // Queue delete in pending delta
    tx_handle
      .lock()
      .pending
      .pending_vectors
      .insert((node_id, prop_key_id), None); // None means delete
    Ok(())
  }

  /// Get a vector embedding for a node, as its store holds it (normalized
  /// for the cosine stores this API creates), also inside the transaction
  /// that set it.
  ///
  /// Checks pending operations first, then falls back to committed storage.
  pub fn node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> Option<VectorRef> {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let pending = {
        let tx = handle.lock();
        if tx.pending.is_node_removed(node_id) {
          return None;
        }
        tx.pending
          .pending_vectors
          .get(&(node_id, prop_key_id))
          .cloned()
      };
      if let Some(pending) = pending {
        return pending.map(|vector| self.stored_vector(prop_key_id, vector));
      }
    }

    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_removed(node_id) {
      return None;
    }

    // Check pending operations from committed replay (startup)
    if let Some(pending) = delta.pending_vectors.get(&(node_id, prop_key_id)) {
      // Some(vec) = set, None = delete
      return pending
        .clone()
        .map(|vector| self.stored_vector(prop_key_id, vector));
    }

    if self.ensure_vector_store_loaded(prop_key_id).is_err() {
      return None;
    }

    // Fall back to committed storage
    let stores = self.vector_stores.read();
    let store = stores.get(&prop_key_id)?;
    vector_store_node_vector(store, node_id).map(Arc::from)
  }

  /// `vector`, not in its store yet (set by a transaction, or replayed), as
  /// the store will hold it: `vector_store_insert` normalizes it in place
  /// when the store normalizes on insert, as every store this API creates
  /// does. The same computation, so reads before and after the commit agree.
  fn stored_vector(&self, prop_key_id: PropKeyId, vector: VectorRef) -> VectorRef {
    let store_normalizes = self
      .ensure_vector_store_loaded(prop_key_id)
      .ok()
      .and_then(|()| {
        self
          .vector_stores
          .read()
          .get(&prop_key_id)
          .map(|store| store.config.normalize_on_insert)
      })
      .unwrap_or(VectorStoreConfig::new(vector.len()).normalize_on_insert);
    if !store_normalizes {
      return vector;
    }
    let mut stored = vector.to_vec();
    normalize_in_place(&mut stored);
    VectorRef::from(stored)
  }

  /// Check if a node has a vector embedding
  pub fn has_node_vector(&self, node_id: NodeId, prop_key_id: PropKeyId) -> bool {
    let tx_handle = self.current_tx_handle();
    if let Some(handle) = tx_handle.as_ref() {
      let tx = handle.lock();
      if tx.pending.is_node_removed(node_id) {
        return false;
      }
      if let Some(pending) = tx.pending.pending_vectors.get(&(node_id, prop_key_id)) {
        return pending.is_some();
      }
    }

    let delta = self.delta.read();

    // Check if node is deleted
    if delta.is_node_removed(node_id) {
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
    // A commit checks its vectors' dimensions against the stores before its
    // COMMIT record and applies them after, both under the commit lock; a
    // store created in between would make that apply fail.
    let _commit_guard = self.commit_lock.lock();
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
    return Ok((stores, HashMap::new()));
  };
  let Some(node_prop_keys) = snapshot.section_data_shared(SectionId::NodePropKeys) else {
    return Ok((stores, HashMap::new()));
  };
  let Some(node_prop_vals) = snapshot.section_data_shared(SectionId::NodePropVals) else {
    return Ok((stores, HashMap::new()));
  };
  let Some(vector_offsets) = snapshot.section_data_shared(SectionId::VectorOffsets) else {
    return Ok((stores, HashMap::new()));
  };
  let Some(vector_data) = snapshot.section_data_shared(SectionId::VectorData) else {
    return Ok((stores, HashMap::new()));
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

/// Apply the vector operations WAL replay collected (`pending`: the last
/// operation per node and property, from the transactions in `committed`) to
/// `stores`. A property's dimensions are its store's or, for a property
/// without one, those of its first vector in commit order, as the live
/// database fixed them.
///
/// A vector that disagrees with them, or that the store refuses as invalid,
/// is skipped with a warning rather than failing the open. Versions without
/// the commit-time dimension check could commit two transactions that gave a
/// new property different dimensions; the second never reached the live
/// store, and a WAL holding it must still open.
pub(crate) fn apply_replayed_vectors(
  pending: HashMap<(NodeId, PropKeyId), Option<VectorRef>>,
  committed: &[(TxId, Vec<&ParsedWalRecord>)],
  snapshot: Option<&SnapshotData>,
  stores: &mut HashMap<PropKeyId, VectorManifest>,
  lazy_entries: &mut HashMap<PropKeyId, VectorStoreLazyEntry>,
) -> Result<()> {
  let mut first_dimensions: HashMap<PropKeyId, usize> = HashMap::new();
  let set_vectors = committed
    .iter()
    .flat_map(|(_txid, records)| records)
    .filter(|record| record.record_type == WalRecordType::SetNodeVector)
    .filter_map(|record| parse_set_node_vector_payload(&record.payload));
  for set_vector in set_vectors {
    first_dimensions
      .entry(set_vector.prop_key_id)
      .or_insert(set_vector.vector.len());
  }

  let mut skipped = 0usize;
  for ((node_id, prop_key_id), operation) in pending {
    if let Some(snapshot) = snapshot {
      materialize_vector_store_from_lazy_entries(snapshot, stores, lazy_entries, prop_key_id)?;
    }
    match operation {
      Some(vector) => {
        let dimensions = first_dimensions
          .get(&prop_key_id)
          .copied()
          .unwrap_or(vector.len());
        let store = stores
          .entry(prop_key_id)
          .or_insert_with(|| create_vector_store(VectorStoreConfig::new(dimensions)));
        match vector_store_insert(store, node_id, vector.as_ref()) {
          Ok(_) => {}
          Err(VectorStoreError::DimensionMismatch { .. } | VectorStoreError::InvalidVector(_)) => {
            skipped += 1;
          }
          Err(error) => {
            return Err(KiteError::InvalidWal(format!(
              "Failed to apply vector insert during WAL replay for node {node_id} (prop \
               {prop_key_id}): {error}"
            )));
          }
        }
      }
      None => {
        if let Some(store) = stores.get_mut(&prop_key_id) {
          vector_store_delete(store, node_id);
        }
      }
    }
  }
  if skipped > 0 {
    eprintln!(
      "Warning: skipped {skipped} committed vector operations in the WAL that do not fit their \
       property's vector store (other dimensions than its first vector, or an invalid vector)"
    );
  }
  Ok(())
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
        "Vector store entry {i} out of bounds: {payload_offset}..{payload_end} exceeds blob size {blob_len}"
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
}

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
  if !bytes.len().is_multiple_of(4) {
    return Err(KiteError::InvalidSnapshot(format!(
      "Vector byte length is not multiple of 4 for idx={idx}: {}",
      bytes.len()
    )));
  }

  let mut vec = Vec::with_capacity(bytes.len() / 4);
  for chunk in bytes.as_chunks::<4>().0 {
    vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
  }
  Ok(vec)
}

#[cfg(test)]
mod tests {
  use super::{
    decode_vector_payload, deserialize_vector_store_entry, vector_store_state_from_snapshot,
  };
  use crate::error::Result;
  use crate::types::PropKeyId;
  use crate::vector::types::VectorManifest;

  /// Every vector store of `snapshot`, decoded.
  fn vector_stores_from_snapshot(
    snapshot: &SnapshotData,
  ) -> Result<HashMap<PropKeyId, VectorManifest>> {
    let (mut stores, lazy_entries) = vector_store_state_from_snapshot(snapshot)?;
    for (prop_key_id, entry) in lazy_entries {
      let manifest = deserialize_vector_store_entry(snapshot, prop_key_id, &entry)?;
      stores.insert(prop_key_id, manifest);
    }
    Ok(stores)
  }
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::core::snapshot::reader::SnapshotData;
  use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, SnapshotBuildInput};
  use crate::types::{
    PropValue, SectionId, SnapshotFlags, SECTION_ENTRY_SIZE, SNAPSHOT_HEADER_SIZE,
  };
  use crate::util::binary::{read_u64, write_u32, write_u64};
  use crate::util::crc::crc32;
  use crate::vector::distance::normalize;
  use crate::vector::store::{create_vector_store, vector_store_has, vector_store_insert};
  use crate::vector::types::VectorStoreConfig;
  use std::collections::HashMap;
  use std::io::Write;
  use tempfile::{tempdir, NamedTempFile};

  #[test]
  fn test_decode_vector_payload_round_trip() {
    let mut offsets = Vec::new();
    for off in [0u64, 8, 20] {
      offsets.extend_from_slice(&off.to_le_bytes());
    }
    let mut data = Vec::new();
    for value in [0.1f32, 0.2, 0.3, 1.0, 2.0] {
      data.extend_from_slice(&value.to_le_bytes());
    }

    let first = decode_vector_payload(&offsets, &data, 0).expect("expected value");
    assert_eq!(first, vec![0.1, 0.2]);

    let second = decode_vector_payload(&offsets, &data, 1).expect("expected value");
    assert_eq!(second, vec![0.3, 1.0, 2.0]);
  }

  /// Vector `idx` spans `offsets[idx]..offsets[idx + 1]`, so it needs
  /// `idx + 2` offsets. The bound checked for `idx + 1`, so the last offset's
  /// index passed and reading its end panicked (and a huge index overflowed).
  /// Unreachable from a loaded snapshot, whose load rejects such an index.
  #[test]
  fn decode_vector_payload_rejects_an_index_without_an_end_offset() {
    let offsets: Vec<u8> = [0u64, 8].iter().flat_map(|off| off.to_le_bytes()).collect();
    let data: Vec<u8> = [0.5f32, 1.5].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(
      decode_vector_payload(&offsets, &data, 0).expect("vector 0"),
      vec![0.5, 1.5]
    );
    for idx in [1, 2, usize::MAX / 8, usize::MAX] {
      let decoded = std::panic::catch_unwind(|| decode_vector_payload(&offsets, &data, idx));
      assert!(
        matches!(decoded, Ok(Err(_))),
        "index {idx} of 2 offsets must be an error, got {decoded:?}"
      );
    }
  }

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
    let fake_payload_offset = prev_end.div_ceil(64) * 64;
    let required_size = ((fake_payload_offset + 1).div_ceil(64) * 64 + 4) as usize;
    if buffer.len() < required_size {
      buffer.resize(required_size, 0);
    }
    write_u64(&mut buffer, entry_offset, fake_payload_offset);
    write_u64(&mut buffer, entry_offset + 8, 1);
    write_u32(&mut buffer, entry_offset + 16, 0);
    write_u32(&mut buffer, entry_offset + 20, 1);
    let crc_offset = buffer.len() - 4;
    let crc = crc32(&buffer[..crc_offset]);
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
