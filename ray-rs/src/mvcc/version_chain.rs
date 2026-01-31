//! MVCC Version Chain Store
//!
//! Manages version chains for nodes, edges, and properties.
//! Uses SOA (struct-of-arrays) storage for property versions to reduce memory overhead.
//!
//! Ported from src/mvcc/version-chain.ts

use std::collections::HashMap;

use crate::mvcc::visibility::VersionedRecord;
use crate::types::{
  ETypeId, EdgeVersionData, NodeDelta, NodeId, NodeVersionData, PropKeyId, PropValue, Timestamp,
  TxId,
};

// ============================================================================
// SOA Property Versions
// ============================================================================

/// Index into the SOA arrays (u32::MAX = null)
const NULL_IDX: u32 = u32::MAX;

/// SOA storage for property versions - stores version metadata in parallel arrays
/// This reduces memory overhead compared to storing full VersionedRecord structs
#[derive(Debug)]
pub struct SoaPropertyVersions<T> {
  /// Data values
  data: Vec<T>,
  /// Transaction IDs
  txids: Vec<TxId>,
  /// Commit timestamps
  commit_ts: Vec<Timestamp>,
  /// Previous version indices (NULL_IDX = no previous)
  prev_idx: Vec<u32>,
  /// Deleted flags
  deleted: Vec<bool>,
  /// Key -> head index mapping
  heads: HashMap<u64, u32>,
  /// Free list for reusing slots
  free_list: Vec<u32>,
}

impl<T: Clone> SoaPropertyVersions<T> {
  pub fn new() -> Self {
    Self {
      data: Vec::new(),
      txids: Vec::new(),
      commit_ts: Vec::new(),
      prev_idx: Vec::new(),
      deleted: Vec::new(),
      heads: HashMap::new(),
      free_list: Vec::new(),
    }
  }

  /// Append a new version to a key's version chain
  pub fn append(&mut self, key: u64, value: T, txid: TxId, commit_ts: Timestamp) {
    let prev = self.heads.get(&key).copied().unwrap_or(NULL_IDX);

    // Try to reuse a free slot
    let idx = if let Some(free_idx) = self.free_list.pop() {
      self.data[free_idx as usize] = value;
      self.txids[free_idx as usize] = txid;
      self.commit_ts[free_idx as usize] = commit_ts;
      self.prev_idx[free_idx as usize] = prev;
      self.deleted[free_idx as usize] = false;
      free_idx
    } else {
      let idx = self.data.len() as u32;
      self.data.push(value);
      self.txids.push(txid);
      self.commit_ts.push(commit_ts);
      self.prev_idx.push(prev);
      self.deleted.push(false);
      idx
    };

    self.heads.insert(key, idx);
  }

  /// Get the head version for a key
  pub fn get_head(&self, key: u64) -> Option<PooledVersion<&T>> {
    let idx = *self.heads.get(&key)?;
    self.get_at(idx)
  }

  /// Get version at a specific index
  pub fn get_at(&self, idx: u32) -> Option<PooledVersion<&T>> {
    if idx == NULL_IDX || idx as usize >= self.data.len() {
      return None;
    }
    let i = idx as usize;
    Some(PooledVersion {
      data: &self.data[i],
      txid: self.txids[i],
      commit_ts: self.commit_ts[i],
      prev_idx: self.prev_idx[i],
      deleted: self.deleted[i],
    })
  }

  /// Prune old versions older than the given timestamp
  /// Returns the number of versions pruned
  pub fn prune_old_versions(&mut self, horizon_ts: Timestamp) -> usize {
    let mut pruned = 0;
    let mut keys_to_remove = Vec::new();

    for (&key, &head_idx) in &self.heads {
      // Walk the chain to find versions to prune
      let mut current_idx = head_idx;
      let mut keep_idx = NULL_IDX;
      let mut prev_keep_idx = NULL_IDX;

      while current_idx != NULL_IDX {
        let i = current_idx as usize;
        let ts = self.commit_ts[i];

        if ts < horizon_ts {
          // This version is old
          if keep_idx == NULL_IDX {
            // First old version - might need to keep it
            keep_idx = current_idx;
          } else {
            // Older than keep_idx - can be pruned
            self.free_list.push(current_idx);
            pruned += 1;
          }
        } else {
          prev_keep_idx = current_idx;
        }

        current_idx = self.prev_idx[i];
      }

      // If the entire chain is old, mark for removal
      if head_idx != NULL_IDX && self.commit_ts[head_idx as usize] < horizon_ts {
        keys_to_remove.push(key);
        self.free_list.push(head_idx);
        pruned += 1;
      } else if keep_idx != NULL_IDX && prev_keep_idx != NULL_IDX {
        // Truncate the chain at keep_idx
        self.prev_idx[keep_idx as usize] = NULL_IDX;
      }
    }

    // Remove entirely old chains
    for key in keys_to_remove {
      self.heads.remove(&key);
    }

    pruned
  }

  /// Truncate deep chains to limit worst-case traversal time
  pub fn truncate_deep_chains(
    &mut self,
    max_depth: usize,
    min_active_ts: Option<Timestamp>,
  ) -> usize {
    let mut truncated = 0;

    for &head_idx in self.heads.values() {
      let mut depth = 0;
      let mut current_idx = head_idx;
      let mut truncate_at = NULL_IDX;

      while current_idx != NULL_IDX && depth < max_depth {
        let i = current_idx as usize;

        // Track the last version that's safe to truncate after
        if let Some(min_ts) = min_active_ts {
          if self.commit_ts[i] >= min_ts {
            truncate_at = current_idx;
          }
        } else {
          truncate_at = current_idx;
        }

        depth += 1;
        current_idx = self.prev_idx[i];
      }

      // If we exceeded max_depth, truncate
      if current_idx != NULL_IDX && truncate_at != NULL_IDX {
        // Free all versions after truncate_at
        let mut to_free = self.prev_idx[truncate_at as usize];
        self.prev_idx[truncate_at as usize] = NULL_IDX;

        while to_free != NULL_IDX {
          let next = self.prev_idx[to_free as usize];
          self.free_list.push(to_free);
          to_free = next;
        }

        truncated += 1;
      }
    }

    truncated
  }

  /// Clear all versions
  pub fn clear(&mut self) {
    self.data.clear();
    self.txids.clear();
    self.commit_ts.clear();
    self.prev_idx.clear();
    self.deleted.clear();
    self.heads.clear();
    self.free_list.clear();
  }

  /// Get memory usage estimate in bytes
  pub fn get_memory_usage(&self) -> usize {
    let data_size = std::mem::size_of::<T>() * self.data.capacity();
    let meta_size = (std::mem::size_of::<TxId>()
      + std::mem::size_of::<Timestamp>()
      + std::mem::size_of::<u32>()
      + std::mem::size_of::<bool>())
      * self.data.capacity();
    let heads_size = std::mem::size_of::<(u64, u32)>() * self.heads.capacity();
    let free_list_size = std::mem::size_of::<u32>() * self.free_list.capacity();

    data_size + meta_size + heads_size + free_list_size
  }

  /// Get number of tracked keys
  pub fn len(&self) -> usize {
    self.heads.len()
  }

  /// Check if empty
  pub fn is_empty(&self) -> bool {
    self.heads.is_empty()
  }
}

impl<T: Clone> Default for SoaPropertyVersions<T> {
  fn default() -> Self {
    Self::new()
  }
}

/// A version from the pooled SOA storage
#[derive(Debug, Clone)]
pub struct PooledVersion<T> {
  pub data: T,
  pub txid: TxId,
  pub commit_ts: Timestamp,
  pub prev_idx: u32,
  pub deleted: bool,
}

// ============================================================================
// Version Chain Manager
// ============================================================================

/// Version chain manager for MVCC
///
/// Stores version chains for:
/// - Node versions (creation, modification, deletion)
/// - Edge versions (add/delete)
/// - Node property versions (using SOA storage)
/// - Edge property versions (using SOA storage)
#[derive(Debug)]
pub struct VersionChainManager {
  /// Node version chains: nodeId -> head version
  node_versions: HashMap<NodeId, Box<VersionedRecord<NodeVersionData>>>,
  /// Edge version chains: packed(src, etype, dst) -> head version
  edge_versions: HashMap<u64, Box<VersionedRecord<EdgeVersionData>>>,
  /// SOA-backed storage for node property versions
  soa_node_props: SoaPropertyVersions<Option<PropValue>>,
  /// SOA-backed storage for edge property versions
  soa_edge_props: SoaPropertyVersions<Option<PropValue>>,
  /// Whether SOA storage is enabled (for benchmarking/compatibility)
  use_soa: bool,
  /// Legacy node property versions (when SOA is disabled)
  legacy_node_props: HashMap<u64, Box<VersionedRecord<Option<PropValue>>>>,
  /// Legacy edge property versions (when SOA is disabled)
  legacy_edge_props: HashMap<u64, Box<VersionedRecord<Option<PropValue>>>>,
}

impl VersionChainManager {
  /// Create a new version chain manager with SOA storage enabled
  pub fn new() -> Self {
    Self::with_soa(true)
  }

  /// Create a new version chain manager with optional SOA storage
  pub fn with_soa(use_soa: bool) -> Self {
    Self {
      node_versions: HashMap::new(),
      edge_versions: HashMap::new(),
      soa_node_props: SoaPropertyVersions::new(),
      soa_edge_props: SoaPropertyVersions::new(),
      use_soa,
      legacy_node_props: HashMap::new(),
      legacy_edge_props: HashMap::new(),
    }
  }

  // ========================================================================
  // Key computation helpers
  // ========================================================================

  /// Compute numeric composite key for edge lookups
  /// Uses bit packing: src (20 bits) | etype (20 bits) | dst (20 bits)
  /// Supports NodeID/ETypeID up to ~1M values each
  #[inline]
  fn edge_key(src: NodeId, etype: ETypeId, dst: NodeId) -> u64 {
    ((src & 0xFFFFF) << 40) | ((etype as u64 & 0xFFFFF) << 20) | (dst & 0xFFFFF)
  }

  /// Compute numeric composite key for node property lookups
  /// Uses bit packing: nodeId (40 bits) | propKeyId (24 bits)
  #[inline]
  pub fn node_prop_key(node_id: NodeId, prop_key_id: PropKeyId) -> u64 {
    (node_id << 24) | (prop_key_id as u64)
  }

  /// Compute numeric composite key for edge property lookups
  /// Uses bit packing: src (20 bits) | etype (12 bits) | dst (20 bits) | propKeyId (12 bits)
  #[inline]
  pub fn edge_prop_key(src: NodeId, etype: ETypeId, dst: NodeId, prop_key_id: PropKeyId) -> u64 {
    ((src & 0xFFFFF) << 44)
      | ((etype as u64 & 0xFFF) << 32)
      | ((dst & 0xFFFFF) << 12)
      | (prop_key_id as u64 & 0xFFF)
  }

  // ========================================================================
  // Node versions
  // ========================================================================

  /// Append a new version to a node's version chain
  pub fn append_node_version(
    &mut self,
    node_id: NodeId,
    data: NodeVersionData,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let existing = self.node_versions.remove(&node_id);
    let new_version = Box::new(VersionedRecord {
      data,
      txid,
      commit_ts,
      prev: existing,
      deleted: false,
    });
    self.node_versions.insert(node_id, new_version);
  }

  /// Mark a node as deleted
  pub fn delete_node_version(&mut self, node_id: NodeId, txid: TxId, commit_ts: Timestamp) {
    let existing = self.node_versions.remove(&node_id);
    let deleted_version = Box::new(VersionedRecord {
      data: NodeVersionData {
        node_id,
        delta: NodeDelta::default(),
      },
      txid,
      commit_ts,
      prev: existing,
      deleted: true,
    });
    self.node_versions.insert(node_id, deleted_version);
  }

  /// Get the latest version for a node
  pub fn get_node_version(&self, node_id: NodeId) -> Option<&VersionedRecord<NodeVersionData>> {
    self.node_versions.get(&node_id).map(|b| b.as_ref())
  }

  // ========================================================================
  // Edge versions
  // ========================================================================

  /// Append a new version to an edge's version chain
  pub fn append_edge_version(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    added: bool,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::edge_key(src, etype, dst);
    let existing = self.edge_versions.remove(&key);
    let new_version = Box::new(VersionedRecord {
      data: EdgeVersionData {
        src,
        etype,
        dst,
        added,
      },
      txid,
      commit_ts,
      prev: existing,
      deleted: false,
    });
    self.edge_versions.insert(key, new_version);
  }

  /// Get the latest version for an edge
  pub fn get_edge_version(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<&VersionedRecord<EdgeVersionData>> {
    let key = Self::edge_key(src, etype, dst);
    self.edge_versions.get(&key).map(|b| b.as_ref())
  }

  // ========================================================================
  // Node property versions
  // ========================================================================

  /// Append a new version to a node property's version chain
  pub fn append_node_prop_version(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::node_prop_key(node_id, prop_key_id);

    if self.use_soa {
      self.soa_node_props.append(key, value, txid, commit_ts);
    } else {
      let existing = self.legacy_node_props.remove(&key);
      let new_version = Box::new(VersionedRecord {
        data: value,
        txid,
        commit_ts,
        prev: existing,
        deleted: false,
      });
      self.legacy_node_props.insert(key, new_version);
    }
  }

  /// Get the latest version for a node property
  /// Returns a VersionedRecord for API compatibility
  pub fn get_node_prop_version(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<VersionedRecord<Option<PropValue>>> {
    let key = Self::node_prop_key(node_id, prop_key_id);

    if self.use_soa {
      self
        .soa_node_props
        .get_head(key)
        .map(|pooled| Self::pooled_to_versioned(&self.soa_node_props, pooled))
    } else {
      self.legacy_node_props.get(&key).map(|b| {
        // Clone the versioned record for API compatibility
        Self::clone_versioned_record(b.as_ref())
      })
    }
  }

  // ========================================================================
  // Edge property versions
  // ========================================================================

  /// Append a new version to an edge property's version chain
  #[allow(clippy::too_many_arguments)]
  pub fn append_edge_prop_version(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::edge_prop_key(src, etype, dst, prop_key_id);

    if self.use_soa {
      self.soa_edge_props.append(key, value, txid, commit_ts);
    } else {
      let existing = self.legacy_edge_props.remove(&key);
      let new_version = Box::new(VersionedRecord {
        data: value,
        txid,
        commit_ts,
        prev: existing,
        deleted: false,
      });
      self.legacy_edge_props.insert(key, new_version);
    }
  }

  /// Get the latest version for an edge property
  pub fn get_edge_prop_version(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<VersionedRecord<Option<PropValue>>> {
    let key = Self::edge_prop_key(src, etype, dst, prop_key_id);

    if self.use_soa {
      self
        .soa_edge_props
        .get_head(key)
        .map(|pooled| Self::pooled_to_versioned(&self.soa_edge_props, pooled))
    } else {
      self
        .legacy_edge_props
        .get(&key)
        .map(|b| Self::clone_versioned_record(b.as_ref()))
    }
  }

  // ========================================================================
  // Helper methods
  // ========================================================================

  /// Convert a pooled version to a VersionedRecord (for API compatibility)
  fn pooled_to_versioned(
    store: &SoaPropertyVersions<Option<PropValue>>,
    pooled: PooledVersion<&Option<PropValue>>,
  ) -> VersionedRecord<Option<PropValue>> {
    let prev = if pooled.prev_idx != NULL_IDX {
      store
        .get_at(pooled.prev_idx)
        .map(|prev_pooled| Box::new(Self::pooled_to_versioned(store, prev_pooled)))
    } else {
      None
    };

    VersionedRecord {
      data: pooled.data.clone(),
      txid: pooled.txid,
      commit_ts: pooled.commit_ts,
      prev,
      deleted: pooled.deleted,
    }
  }

  /// Clone a versioned record (for API compatibility)
  fn clone_versioned_record(
    record: &VersionedRecord<Option<PropValue>>,
  ) -> VersionedRecord<Option<PropValue>> {
    VersionedRecord {
      data: record.data.clone(),
      txid: record.txid,
      commit_ts: record.commit_ts,
      prev: record
        .prev
        .as_ref()
        .map(|p| Box::new(Self::clone_versioned_record(p))),
      deleted: record.deleted,
    }
  }

  // ========================================================================
  // Pruning and GC
  // ========================================================================

  /// Prune old versions older than the given timestamp
  /// Returns the number of versions pruned
  pub fn prune_old_versions(&mut self, horizon_ts: Timestamp) -> usize {
    let mut pruned = 0;

    // Prune node versions
    let node_ids: Vec<_> = self.node_versions.keys().copied().collect();
    for node_id in node_ids {
      if let Some(version) = self.node_versions.get_mut(&node_id) {
        let result = Self::prune_chain(version, horizon_ts);
        if result == -1 {
          self.node_versions.remove(&node_id);
          pruned += 1;
        } else {
          pruned += result as usize;
        }
      }
    }

    // Prune edge versions
    let edge_keys: Vec<_> = self.edge_versions.keys().copied().collect();
    for key in edge_keys {
      if let Some(version) = self.edge_versions.get_mut(&key) {
        let result = Self::prune_chain(version, horizon_ts);
        if result == -1 {
          self.edge_versions.remove(&key);
          pruned += 1;
        } else {
          pruned += result as usize;
        }
      }
    }

    // Prune property versions
    if self.use_soa {
      pruned += self.soa_node_props.prune_old_versions(horizon_ts);
      pruned += self.soa_edge_props.prune_old_versions(horizon_ts);
    } else {
      // Legacy path
      let node_prop_keys: Vec<_> = self.legacy_node_props.keys().copied().collect();
      for key in node_prop_keys {
        if let Some(version) = self.legacy_node_props.get_mut(&key) {
          let result = Self::prune_chain(version, horizon_ts);
          if result == -1 {
            self.legacy_node_props.remove(&key);
            pruned += 1;
          } else {
            pruned += result as usize;
          }
        }
      }

      let edge_prop_keys: Vec<_> = self.legacy_edge_props.keys().copied().collect();
      for key in edge_prop_keys {
        if let Some(version) = self.legacy_edge_props.get_mut(&key) {
          let result = Self::prune_chain(version, horizon_ts);
          if result == -1 {
            self.legacy_edge_props.remove(&key);
            pruned += 1;
          } else {
            pruned += result as usize;
          }
        }
      }
    }

    pruned
  }

  /// Prune a version chain, removing versions older than horizonTs
  /// Returns: -1 if entire chain should be deleted, otherwise count of pruned versions
  fn prune_chain<T>(version: &mut Box<VersionedRecord<T>>, horizon_ts: Timestamp) -> i32 {
    // Find the first version we need to keep (newest version < horizonTs)
    // and count versions to prune
    let mut pruned_count = 0;
    let mut keep_found = false;

    // Count old versions in the tail
    let mut current = version.prev.as_ref();
    while let Some(v) = current {
      if v.commit_ts < horizon_ts {
        if !keep_found {
          keep_found = true;
        } else {
          pruned_count += 1;
        }
      }
      current = v.prev.as_ref();
    }

    // If the head is old, the entire chain can be deleted
    if version.commit_ts < horizon_ts {
      return -1;
    }

    // Truncate the chain
    if keep_found {
      // Walk to find where to truncate
      let mut prev_ref = &mut version.prev;
      while let Some(ref mut v) = prev_ref {
        if v.commit_ts < horizon_ts {
          // Truncate here
          v.prev = None;
          break;
        }
        prev_ref = &mut v.prev;
      }
    }

    pruned_count
  }

  /// Truncate version chains that exceed the max depth limit
  pub fn truncate_deep_chains(
    &mut self,
    max_depth: usize,
    min_active_ts: Option<Timestamp>,
  ) -> usize {
    let mut truncated = 0;

    // Truncate node version chains
    for version in self.node_versions.values_mut() {
      if Self::truncate_chain_at_depth(version, max_depth, min_active_ts) {
        truncated += 1;
      }
    }

    // Truncate edge version chains
    for version in self.edge_versions.values_mut() {
      if Self::truncate_chain_at_depth(version, max_depth, min_active_ts) {
        truncated += 1;
      }
    }

    // Truncate property version chains
    if self.use_soa {
      truncated += self
        .soa_node_props
        .truncate_deep_chains(max_depth, min_active_ts);
      truncated += self
        .soa_edge_props
        .truncate_deep_chains(max_depth, min_active_ts);
    } else {
      for version in self.legacy_node_props.values_mut() {
        if Self::truncate_chain_at_depth(version, max_depth, min_active_ts) {
          truncated += 1;
        }
      }
      for version in self.legacy_edge_props.values_mut() {
        if Self::truncate_chain_at_depth(version, max_depth, min_active_ts) {
          truncated += 1;
        }
      }
    }

    truncated
  }

  /// Truncate a single chain at the given depth
  /// The chain will have at most max_depth versions after truncation
  fn truncate_chain_at_depth<T>(
    head: &mut Box<VersionedRecord<T>>,
    max_depth: usize,
    min_active_ts: Option<Timestamp>,
  ) -> bool {
    // First, count total depth
    let mut total_depth = 1;
    let mut current = head.prev.as_ref();
    while let Some(v) = current {
      total_depth += 1;
      current = v.prev.as_ref();
    }

    // If chain is not too deep, nothing to do
    if total_depth <= max_depth {
      return false;
    }

    // Walk to position (max_depth - 1) and truncate there
    // We want to keep exactly max_depth versions
    let mut current_depth = 1;
    let mut node: &mut Box<VersionedRecord<T>> = head;

    // Walk to the node at position (max_depth - 1)
    while current_depth < max_depth {
      if node.prev.is_none() {
        return false;
      }
      current_depth += 1;
      node = node.prev.as_mut().unwrap();
    }

    // Check if we can safely truncate (respecting min_active_ts)
    if let Some(min_ts) = min_active_ts {
      // Check if any version in the tail is needed
      let mut check = node.prev.as_ref();
      while let Some(c) = check {
        if c.commit_ts < min_ts {
          // This version might be needed by active readers
          return false;
        }
        check = c.prev.as_ref();
      }
    }

    // Truncate the chain
    node.prev = None;
    true
  }

  // ========================================================================
  // Utility methods
  // ========================================================================

  /// Check if any edge versions exist
  pub fn has_any_edge_versions(&self) -> bool {
    !self.edge_versions.is_empty()
  }

  /// Check if SOA storage is enabled
  pub fn is_soa_enabled(&self) -> bool {
    self.use_soa
  }

  /// Get memory usage estimate for SOA stores
  pub fn get_soa_memory_usage(&self) -> (usize, usize) {
    (
      self.soa_node_props.get_memory_usage(),
      self.soa_edge_props.get_memory_usage(),
    )
  }

  /// Clear all versions
  pub fn clear(&mut self) {
    self.node_versions.clear();
    self.edge_versions.clear();
    self.soa_node_props.clear();
    self.soa_edge_props.clear();
    self.legacy_node_props.clear();
    self.legacy_edge_props.clear();
  }

  /// Get counts for statistics
  pub fn get_counts(&self) -> VersionChainCounts {
    VersionChainCounts {
      node_versions: self.node_versions.len(),
      edge_versions: self.edge_versions.len(),
      node_prop_versions: if self.use_soa {
        self.soa_node_props.len()
      } else {
        self.legacy_node_props.len()
      },
      edge_prop_versions: if self.use_soa {
        self.soa_edge_props.len()
      } else {
        self.legacy_edge_props.len()
      },
    }
  }
}

impl Default for VersionChainManager {
  fn default() -> Self {
    Self::new()
  }
}

/// Version chain counts for statistics
#[derive(Debug, Clone, Default)]
pub struct VersionChainCounts {
  pub node_versions: usize,
  pub edge_versions: usize,
  pub node_prop_versions: usize,
  pub edge_prop_versions: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_soa_property_versions_new() {
    let store: SoaPropertyVersions<i32> = SoaPropertyVersions::new();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
  }

  #[test]
  fn test_soa_append_and_get() {
    let mut store: SoaPropertyVersions<i32> = SoaPropertyVersions::new();

    store.append(1, 42, 1, 10);

    let head = store.get_head(1);
    assert!(head.is_some());
    let v = head.unwrap();
    assert_eq!(*v.data, 42);
    assert_eq!(v.txid, 1);
    assert_eq!(v.commit_ts, 10);
  }

  #[test]
  fn test_soa_version_chain() {
    let mut store: SoaPropertyVersions<i32> = SoaPropertyVersions::new();

    // Create version chain: v1 -> v2 -> v3
    store.append(1, 10, 1, 10);
    store.append(1, 20, 2, 20);
    store.append(1, 30, 3, 30);

    let head = store.get_head(1).unwrap();
    assert_eq!(*head.data, 30);
    assert_eq!(head.commit_ts, 30);
    assert_ne!(head.prev_idx, NULL_IDX);

    // Follow chain
    let prev = store.get_at(head.prev_idx).unwrap();
    assert_eq!(*prev.data, 20);
  }

  #[test]
  fn test_version_chain_manager_new() {
    let mgr = VersionChainManager::new();
    assert!(mgr.is_soa_enabled());
    let counts = mgr.get_counts();
    assert_eq!(counts.node_versions, 0);
    assert_eq!(counts.edge_versions, 0);
  }

  #[test]
  fn test_node_version_append_and_get() {
    let mut mgr = VersionChainManager::new();

    let data = NodeVersionData {
      node_id: 1,
      delta: NodeDelta::default(),
    };
    mgr.append_node_version(1, data, 1, 10);

    let version = mgr.get_node_version(1);
    assert!(version.is_some());
    assert_eq!(version.unwrap().data.node_id, 1);
  }

  #[test]
  fn test_node_version_chain() {
    let mut mgr = VersionChainManager::new();

    // Append multiple versions
    for i in 1..=3 {
      let data = NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      };
      mgr.append_node_version(1, data, i, i * 10);
    }

    let version = mgr.get_node_version(1).unwrap();
    assert_eq!(version.commit_ts, 30);
    assert!(version.prev.is_some());
    assert_eq!(version.prev.as_ref().unwrap().commit_ts, 20);
  }

  #[test]
  fn test_delete_node_version() {
    let mut mgr = VersionChainManager::new();

    let data = NodeVersionData {
      node_id: 1,
      delta: NodeDelta::default(),
    };
    mgr.append_node_version(1, data, 1, 10);
    mgr.delete_node_version(1, 2, 20);

    let version = mgr.get_node_version(1).unwrap();
    assert!(version.deleted);
    assert_eq!(version.commit_ts, 20);
  }

  #[test]
  fn test_edge_version_append_and_get() {
    let mut mgr = VersionChainManager::new();

    mgr.append_edge_version(1, 1, 2, true, 1, 10);

    let version = mgr.get_edge_version(1, 1, 2);
    assert!(version.is_some());
    let v = version.unwrap();
    assert_eq!(v.data.src, 1);
    assert_eq!(v.data.etype, 1);
    assert_eq!(v.data.dst, 2);
    assert!(v.data.added);
  }

  #[test]
  fn test_edge_version_delete() {
    let mut mgr = VersionChainManager::new();

    mgr.append_edge_version(1, 1, 2, true, 1, 10);
    mgr.append_edge_version(1, 1, 2, false, 2, 20);

    let version = mgr.get_edge_version(1, 1, 2).unwrap();
    assert!(!version.data.added);
  }

  #[test]
  fn test_node_prop_version_soa() {
    let mut mgr = VersionChainManager::new();
    assert!(mgr.is_soa_enabled());

    mgr.append_node_prop_version(1, 1, Some(PropValue::I64(42)), 1, 10);

    let version = mgr.get_node_prop_version(1, 1);
    assert!(version.is_some());
    assert_eq!(version.unwrap().data, Some(PropValue::I64(42)));
  }

  #[test]
  fn test_node_prop_version_legacy() {
    let mut mgr = VersionChainManager::with_soa(false);
    assert!(!mgr.is_soa_enabled());

    mgr.append_node_prop_version(1, 1, Some(PropValue::I64(42)), 1, 10);

    let version = mgr.get_node_prop_version(1, 1);
    assert!(version.is_some());
    assert_eq!(version.unwrap().data, Some(PropValue::I64(42)));
  }

  #[test]
  fn test_edge_prop_version() {
    let mut mgr = VersionChainManager::new();

    mgr.append_edge_prop_version(1, 1, 2, 1, Some(PropValue::F64(3.14)), 1, 10);

    let version = mgr.get_edge_prop_version(1, 1, 2, 1);
    assert!(version.is_some());
    assert_eq!(version.unwrap().data, Some(PropValue::F64(3.14)));
  }

  #[test]
  fn test_has_any_edge_versions() {
    let mut mgr = VersionChainManager::new();

    assert!(!mgr.has_any_edge_versions());

    mgr.append_edge_version(1, 1, 2, true, 1, 10);

    assert!(mgr.has_any_edge_versions());
  }

  #[test]
  fn test_clear() {
    let mut mgr = VersionChainManager::new();

    mgr.append_node_version(
      1,
      NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      },
      1,
      10,
    );
    mgr.append_edge_version(1, 1, 2, true, 1, 10);
    mgr.append_node_prop_version(1, 1, Some(PropValue::I64(42)), 1, 10);

    mgr.clear();

    let counts = mgr.get_counts();
    assert_eq!(counts.node_versions, 0);
    assert_eq!(counts.edge_versions, 0);
    assert_eq!(counts.node_prop_versions, 0);
  }

  #[test]
  fn test_edge_key_packing() {
    // Test that edge keys pack correctly
    let key1 = VersionChainManager::edge_key(1, 2, 3);
    let key2 = VersionChainManager::edge_key(1, 2, 4);
    let key3 = VersionChainManager::edge_key(2, 2, 3);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
    assert_ne!(key2, key3);
  }

  #[test]
  fn test_node_prop_key_packing() {
    let key1 = VersionChainManager::node_prop_key(1, 1);
    let key2 = VersionChainManager::node_prop_key(1, 2);
    let key3 = VersionChainManager::node_prop_key(2, 1);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
  }

  #[test]
  fn test_edge_prop_key_packing() {
    let key1 = VersionChainManager::edge_prop_key(1, 1, 2, 1);
    let key2 = VersionChainManager::edge_prop_key(1, 1, 2, 2);
    let key3 = VersionChainManager::edge_prop_key(1, 1, 3, 1);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
  }

  #[test]
  fn test_soa_memory_usage() {
    let mut mgr = VersionChainManager::new();

    // Add some versions
    for i in 0..100 {
      mgr.append_node_prop_version(i, 1, Some(PropValue::I64(i as i64)), 1, 10);
    }

    let (node_mem, _edge_mem) = mgr.get_soa_memory_usage();
    assert!(node_mem > 0);
  }

  #[test]
  fn test_version_chain_depth() {
    let mut mgr = VersionChainManager::new();

    // Create a deep chain
    for i in 1..=20 {
      let data = NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      };
      mgr.append_node_version(1, data, i, i * 10);
    }

    // Truncate at depth 5
    let truncated = mgr.truncate_deep_chains(5, None);
    assert!(truncated > 0);

    // Verify chain is now limited
    let mut depth = 0;
    let mut current = mgr.get_node_version(1);
    while let Some(v) = current {
      depth += 1;
      current = v.prev.as_deref();
    }
    assert!(depth <= 5);
  }

  #[test]
  fn test_prune_old_versions() {
    let mut mgr = VersionChainManager::new();

    // Create versions at different timestamps
    for i in 1..=5 {
      let data = NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      };
      mgr.append_node_version(1, data, i, i * 10);
    }

    // Prune versions older than ts=35
    let pruned = mgr.prune_old_versions(35);

    // Should have pruned some versions
    assert!(pruned > 0);
  }
}
