//! MVCC Version Chain Store
//!
//! Manages version chains for nodes, edges, properties, labels and key owners.
//! Uses SOA (struct-of-arrays) storage for property versions to reduce memory overhead.
//!
//! # Chains hold history, never the current state
//!
//! The committed delta and snapshot always hold the latest committed state. A chain records
//! what its key looked like before the changes that committed while another transaction was
//! open, because only such a transaction can still need the old state: a baseline version
//! (txid 0, commit_ts 0) with the state before the first recorded change, then one version
//! per recorded change. A version holds the state from its commit until the next version's
//! commit: the `record_*` methods rewrite the newest version to the state they replace when
//! they append a new one, so a change committed with no other transaction open, which
//! records nothing, cannot leave an older version stale.
//!
//! A reader that sees a chain's newest version reads the delta and snapshot (the `*_at`
//! lookups return `None`), so a chain never shadows newer committed state, also not after a
//! checkpoint moves that state into a new snapshot. Only a reader whose snapshot predates
//! the newest version reads the chain. Seeing no version there means the key was absent at
//! its snapshot, so a baseline is only stored for a present state.
//!
//! Ported from src/mvcc/version-chain.ts

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use crate::mvcc::visibility::{history_version, is_visible_at, VersionedRecord};
use crate::types::{
  ETypeId, EdgeVersionData, LabelId, NodeDelta, NodeId, NodeVersionData, PropKeyId, PropValueRef,
  Timestamp, TxId, TxKey,
};

// ============================================================================
// SOA Property Versions
// ============================================================================

/// Index into the SOA arrays (u32::MAX = null)
const NULL_IDX: u32 = u32::MAX;

/// SOA storage for property versions - stores version metadata in parallel arrays
/// This reduces memory overhead compared to storing full VersionedRecord structs
#[derive(Debug)]
pub struct SoaPropertyVersions<T, K = u64> {
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
  heads: HashMap<K, u32>,
  /// Free list for reusing slots
  free_list: Vec<u32>,
}

impl<T: Clone, K: Eq + Hash + Clone> SoaPropertyVersions<T, K> {
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
  pub fn append(&mut self, key: K, value: T, txid: TxId, commit_ts: Timestamp) {
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
  pub fn head(&self, key: K) -> Option<PooledVersion<&T>> {
    let idx = *self.heads.get(&key)?;
    self.at(idx)
  }

  /// Get version at a specific index
  pub fn at(&self, idx: u32) -> Option<PooledVersion<&T>> {
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

  pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
    self.heads.keys().cloned()
  }

  /// Prune old versions older than the given timestamp
  /// Returns the number of versions pruned
  pub fn prune_old_versions(&mut self, horizon_ts: Timestamp) -> usize {
    let mut pruned = 0;
    let mut keys_to_remove = Vec::new();

    for (key, &head_idx) in &self.heads {
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
        keys_to_remove.push(key.clone());
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
  ///
  /// The newest version with `commit_ts < min_active_ts` is what the oldest active reader
  /// sees, so a chain is never cut above it, even if that leaves it deeper than `max_depth`.
  pub fn truncate_deep_chains(
    &mut self,
    max_depth: usize,
    min_active_ts: Option<Timestamp>,
  ) -> usize {
    let mut truncated = 0;

    for &head_idx in self.heads.values() {
      // Walk to the oldest version to keep: at least `max_depth` deep, and deep enough to
      // include the oldest reader's version.
      let mut keep_idx = head_idx;
      let mut depth = 1;
      let mut oldest_reader_covered = min_active_ts.is_none();
      loop {
        let i = keep_idx as usize;
        if min_active_ts.is_some_and(|min_ts| self.commit_ts[i] < min_ts) {
          oldest_reader_covered = true;
        }
        let prev = self.prev_idx[i];
        if prev == NULL_IDX || (depth >= max_depth && oldest_reader_covered) {
          break;
        }
        keep_idx = prev;
        depth += 1;
      }

      // Free all versions after keep_idx
      let mut to_free = self.prev_idx[keep_idx as usize];
      if to_free == NULL_IDX {
        continue;
      }
      self.prev_idx[keep_idx as usize] = NULL_IDX;
      while to_free != NULL_IDX {
        let next = self.prev_idx[to_free as usize];
        self.free_list.push(to_free);
        to_free = next;
      }
      truncated += 1;
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
  pub fn memory_usage(&self) -> usize {
    let data_size = std::mem::size_of::<T>() * self.data.capacity();
    let meta_size = (std::mem::size_of::<TxId>()
      + std::mem::size_of::<Timestamp>()
      + std::mem::size_of::<u32>()
      + std::mem::size_of::<bool>())
      * self.data.capacity();
    let heads_size = std::mem::size_of::<(K, u32)>() * self.heads.capacity();
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

impl<T: Clone, K: Eq + Hash + Clone> Default for SoaPropertyVersions<T, K> {
  fn default() -> Self {
    Self::new()
  }
}

/// History chains of optional values (`None`: absent), see the module docs.
impl<V: Clone + PartialEq, K: Eq + Hash + Clone> SoaPropertyVersions<Option<V>, K> {
  /// Record that commit `commit_ts` changed `key` from `before` to `after`.
  pub fn record(
    &mut self,
    key: K,
    before: Option<V>,
    after: Option<V>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    match self.heads.get(&key).map(|&idx| idx as usize) {
      // Another change of the same commit: it replaces the first.
      Some(head) if self.commit_ts[head] == commit_ts => {
        self.data[head] = after;
        return;
      }
      _ if before == after => return,
      Some(head) => self.data[head] = before,
      None => {
        if before.is_some() {
          self.append(key.clone(), before, 0, 0);
        }
      }
    }
    self.append(key, after, txid, commit_ts);
  }

  /// The value a reader at `snapshot_ts` sees in `key`'s history: `None` when it sees the
  /// newest version (the current state), `Some(None)` when the key was absent.
  pub fn history<Q>(&self, key: &Q, snapshot_ts: Timestamp, txid: TxId) -> Option<Option<&V>>
  where
    K: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let mut idx = *self.heads.get(key)?;
    let visible = |idx: u32| {
      let i = idx as usize;
      is_visible_at(self.txids[i], self.commit_ts[i], snapshot_ts, txid)
    };
    if visible(idx) {
      return None;
    }
    idx = self.prev_idx[idx as usize];
    while idx != NULL_IDX {
      if visible(idx) {
        return Some(self.data[idx as usize].as_ref());
      }
      idx = self.prev_idx[idx as usize];
    }
    Some(None)
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
/// - Node label versions (using SOA storage)
/// - Key owners (the node holding a key)
#[derive(Debug)]
pub struct VersionChainManager {
  /// Node version chains: TxKey::Node(node_id) -> head version
  node_versions: HashMap<TxKey, Box<VersionedRecord<NodeVersionData>>>,
  /// Edge version chains: TxKey::Edge { src, etype, dst } -> head version
  edge_versions: HashMap<TxKey, Box<VersionedRecord<EdgeVersionData>>>,
  /// The `(src, etype, dst)` of every edge chain, by endpoint
  edge_chains_by_node: HashMap<NodeId, HashSet<(NodeId, ETypeId, NodeId)>>,
  /// Key owner chains: key -> head version (`None`: no live node holds the key)
  key_owners: HashMap<Arc<str>, Box<VersionedRecord<Option<NodeId>>>>,
  /// SOA-backed storage for node property versions
  soa_node_props: SoaPropertyVersions<Option<PropValueRef>, TxKey>,
  /// SOA-backed storage for edge property versions
  soa_edge_props: SoaPropertyVersions<Option<PropValueRef>, TxKey>,
  /// SOA-backed storage for node label versions
  soa_node_labels: SoaPropertyVersions<Option<bool>, TxKey>,
  /// Whether SOA storage is enabled (for benchmarking/compatibility)
  use_soa: bool,
  /// Legacy node property versions (when SOA is disabled)
  legacy_node_props: HashMap<TxKey, Box<VersionedRecord<Option<PropValueRef>>>>,
  /// Legacy edge property versions (when SOA is disabled)
  legacy_edge_props: HashMap<TxKey, Box<VersionedRecord<Option<PropValueRef>>>>,
  /// Legacy node label versions (when SOA is disabled)
  legacy_node_labels: HashMap<TxKey, Box<VersionedRecord<Option<bool>>>>,
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
      edge_chains_by_node: HashMap::new(),
      key_owners: HashMap::new(),
      soa_node_props: SoaPropertyVersions::new(),
      soa_edge_props: SoaPropertyVersions::new(),
      soa_node_labels: SoaPropertyVersions::new(),
      use_soa,
      legacy_node_props: HashMap::new(),
      legacy_edge_props: HashMap::new(),
      legacy_node_labels: HashMap::new(),
    }
  }

  // ========================================================================
  // Key computation helpers
  // ========================================================================

  /// Compute a collision-free key for edge lookups.
  #[inline]
  fn edge_key(src: NodeId, etype: ETypeId, dst: NodeId) -> TxKey {
    TxKey::Edge { src, etype, dst }
  }

  /// Compute a collision-free key for node property lookups.
  #[inline]
  pub fn node_prop_key(node_id: NodeId, prop_key_id: PropKeyId) -> TxKey {
    TxKey::NodeProp {
      node_id,
      key_id: prop_key_id,
    }
  }

  /// Compute a collision-free key for node label lookups.
  #[inline]
  pub fn node_label_key(node_id: NodeId, label_id: LabelId) -> TxKey {
    TxKey::NodeLabel { node_id, label_id }
  }

  /// Compute a collision-free key for edge property lookups.
  #[inline]
  pub fn edge_prop_key(src: NodeId, etype: ETypeId, dst: NodeId, prop_key_id: PropKeyId) -> TxKey {
    TxKey::EdgeProp {
      src,
      dst,
      etype,
      key_id: prop_key_id,
    }
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
    let key = TxKey::Node(node_id);
    let existing = self.node_versions.remove(&key);
    let new_version = Box::new(VersionedRecord {
      data,
      txid,
      commit_ts,
      prev: existing,
      deleted: false,
    });
    self.node_versions.insert(key, new_version);
  }

  /// Mark a node as deleted
  pub fn delete_node_version(&mut self, node_id: NodeId, txid: TxId, commit_ts: Timestamp) {
    let key = TxKey::Node(node_id);
    let existing = self.node_versions.remove(&key);
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
    self.node_versions.insert(key, deleted_version);
  }

  /// Get the latest version for a node
  pub fn node_version(&self, node_id: NodeId) -> Option<&VersionedRecord<NodeVersionData>> {
    self
      .node_versions
      .get(&TxKey::Node(node_id))
      .map(|b| b.as_ref())
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
    if existing.is_none() {
      self.index_edge_chain(src, etype, dst);
    }
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
  pub fn edge_version(
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
    value: Option<PropValueRef>,
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
  pub fn node_prop_version(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<VersionedRecord<Option<PropValueRef>>> {
    let key = Self::node_prop_key(node_id, prop_key_id);

    if self.use_soa {
      self
        .soa_node_props
        .head(key)
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
    value: Option<PropValueRef>,
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
  pub fn edge_prop_version(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<VersionedRecord<Option<PropValueRef>>> {
    let key = Self::edge_prop_key(src, etype, dst, prop_key_id);

    if self.use_soa {
      self
        .soa_edge_props
        .head(key)
        .map(|pooled| Self::pooled_to_versioned(&self.soa_edge_props, pooled))
    } else {
      self
        .legacy_edge_props
        .get(&key)
        .map(|b| Self::clone_versioned_record(b.as_ref()))
    }
  }

  // ========================================================================
  // Node label versions
  // ========================================================================

  /// Append a new version to a node label's version chain
  pub fn append_node_label_version(
    &mut self,
    node_id: NodeId,
    label_id: LabelId,
    value: Option<bool>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::node_label_key(node_id, label_id);

    if self.use_soa {
      self.soa_node_labels.append(key, value, txid, commit_ts);
    } else {
      let existing = self.legacy_node_labels.remove(&key);
      let new_version = Box::new(VersionedRecord {
        data: value,
        txid,
        commit_ts,
        prev: existing,
        deleted: false,
      });
      self.legacy_node_labels.insert(key, new_version);
    }
  }

  /// Get the latest version for a node label
  pub fn node_label_version(
    &self,
    node_id: NodeId,
    label_id: LabelId,
  ) -> Option<VersionedRecord<Option<bool>>> {
    let key = Self::node_label_key(node_id, label_id);

    if self.use_soa {
      self
        .soa_node_labels
        .head(key)
        .map(|pooled| Self::pooled_to_versioned(&self.soa_node_labels, pooled))
    } else {
      self.legacy_node_labels.get(&key).map(|b| {
        // Clone the versioned record for API compatibility
        Self::clone_versioned_record(b.as_ref())
      })
    }
  }

  pub fn node_prop_keys(&self, node_id: NodeId) -> Vec<PropKeyId> {
    let mut keys = Vec::new();

    if self.use_soa {
      for key in self.soa_node_props.keys() {
        if let TxKey::NodeProp {
          node_id: key_node_id,
          key_id,
        } = key
        {
          if key_node_id == node_id {
            keys.push(key_id);
          }
        }
      }
    } else {
      for key in self.legacy_node_props.keys() {
        if let TxKey::NodeProp {
          node_id: key_node_id,
          key_id,
        } = key
        {
          if *key_node_id == node_id {
            keys.push(*key_id);
          }
        }
      }
    }

    keys.sort_unstable();
    keys.dedup();
    keys
  }

  pub fn node_label_keys(&self, node_id: NodeId) -> Vec<LabelId> {
    let mut keys = Vec::new();

    if self.use_soa {
      for key in self.soa_node_labels.keys() {
        if let TxKey::NodeLabel {
          node_id: key_node_id,
          label_id,
        } = key
        {
          if key_node_id == node_id {
            keys.push(label_id);
          }
        }
      }
    } else {
      for key in self.legacy_node_labels.keys() {
        if let TxKey::NodeLabel {
          node_id: key_node_id,
          label_id,
        } = key
        {
          if *key_node_id == node_id {
            keys.push(*label_id);
          }
        }
      }
    }

    keys.sort_unstable();
    keys.dedup();
    keys
  }

  pub fn edge_prop_keys(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Vec<PropKeyId> {
    let mut keys = Vec::new();

    if self.use_soa {
      for key in self.soa_edge_props.keys() {
        if let TxKey::EdgeProp {
          src: key_src,
          etype: key_etype,
          dst: key_dst,
          key_id,
        } = key
        {
          if key_src == src && key_etype == etype && key_dst == dst {
            keys.push(key_id);
          }
        }
      }
    } else {
      for key in self.legacy_edge_props.keys() {
        if let TxKey::EdgeProp {
          src: key_src,
          etype: key_etype,
          dst: key_dst,
          key_id,
        } = key
        {
          if *key_src == src && *key_etype == etype && *key_dst == dst {
            keys.push(*key_id);
          }
        }
      }
    }

    keys.sort_unstable();
    keys.dedup();
    keys
  }

  // ========================================================================
  // History: recording commits
  // ========================================================================
  //
  // Each `record_*` method records that commit `commit_ts` (by `txid`) changed a key from
  // `before`, the committed state it replaced, to `after` (see the module docs). A second
  // change by the same commit replaces the first; a change to the same state records
  // nothing.

  /// Record a change of node `node_id`'s existence (`None`: absent) or key.
  pub fn record_node(
    &mut self,
    node_id: NodeId,
    before: Option<NodeVersionData>,
    after: Option<NodeVersionData>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = TxKey::Node(node_id);
    if let Some(head) = self.node_versions.get_mut(&key) {
      if head.commit_ts == commit_ts {
        (head.data, head.deleted) = Self::node_state(node_id, after);
        return;
      }
    }
    if before.as_ref().map(|data| &data.delta.key) == after.as_ref().map(|data| &data.delta.key) {
      return;
    }
    let prev = match self.node_versions.remove(&key) {
      Some(mut head) => {
        (head.data, head.deleted) = Self::node_state(node_id, before);
        Some(head)
      }
      None => before.map(|data| Box::new(VersionedRecord::new(data, 0, 0))),
    };
    let (data, deleted) = Self::node_state(node_id, after);
    let version = VersionedRecord {
      data,
      txid,
      commit_ts,
      prev,
      deleted,
    };
    self.node_versions.insert(key, Box::new(version));
  }

  /// The version data and deleted flag for a node state (`None`: absent).
  fn node_state(node_id: NodeId, state: Option<NodeVersionData>) -> (NodeVersionData, bool) {
    match state {
      Some(data) => (data, false),
      None => (
        NodeVersionData {
          node_id,
          delta: NodeDelta::default(),
        },
        true,
      ),
    }
  }

  /// Record a change of an edge's existence.
  #[allow(clippy::too_many_arguments)]
  pub fn record_edge(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    before: bool,
    after: bool,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::edge_key(src, etype, dst);
    if let Some(head) = self.edge_versions.get_mut(&key) {
      if head.commit_ts == commit_ts {
        head.data.added = after;
        return;
      }
    }
    if before == after {
      return;
    }
    let data = |added| EdgeVersionData {
      src,
      etype,
      dst,
      added,
    };
    let prev = match self.edge_versions.remove(&key) {
      Some(mut head) => {
        head.data.added = before;
        Some(head)
      }
      None => {
        self.index_edge_chain(src, etype, dst);
        before.then(|| Box::new(VersionedRecord::new(data(true), 0, 0)))
      }
    };
    let version = VersionedRecord {
      data: data(after),
      txid,
      commit_ts,
      prev,
      deleted: false,
    };
    self.edge_versions.insert(key, Box::new(version));
  }

  /// Record a change of a node property (`None`: unset).
  pub fn record_node_prop(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    before: Option<PropValueRef>,
    after: Option<PropValueRef>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::node_prop_key(node_id, prop_key_id);
    if self.use_soa {
      self
        .soa_node_props
        .record(key, before, after, txid, commit_ts);
    } else {
      Self::record_in(
        &mut self.legacy_node_props,
        key,
        before,
        after,
        txid,
        commit_ts,
      );
    }
  }

  /// Record a change of an edge property (`None`: unset).
  #[allow(clippy::too_many_arguments)]
  pub fn record_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
    before: Option<PropValueRef>,
    after: Option<PropValueRef>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::edge_prop_key(src, etype, dst, prop_key_id);
    if self.use_soa {
      self
        .soa_edge_props
        .record(key, before, after, txid, commit_ts);
    } else {
      Self::record_in(
        &mut self.legacy_edge_props,
        key,
        before,
        after,
        txid,
        commit_ts,
      );
    }
  }

  /// Record a change of whether node `node_id` has label `label_id`.
  pub fn record_node_label(
    &mut self,
    node_id: NodeId,
    label_id: LabelId,
    before: bool,
    after: bool,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    let key = Self::node_label_key(node_id, label_id);
    let (before, after) = (before.then_some(true), after.then_some(true));
    if self.use_soa {
      self
        .soa_node_labels
        .record(key, before, after, txid, commit_ts);
    } else {
      Self::record_in(
        &mut self.legacy_node_labels,
        key,
        before,
        after,
        txid,
        commit_ts,
      );
    }
  }

  /// Record a change of the live node holding `key` (`None`: none).
  pub fn record_key_owner(
    &mut self,
    key: &str,
    before: Option<NodeId>,
    after: Option<NodeId>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    Self::record_in(
      &mut self.key_owners,
      Arc::from(key),
      before,
      after,
      txid,
      commit_ts,
    );
  }

  /// `record_*` for a map of boxed chains of optional values (`None`: absent).
  fn record_in<K: Eq + Hash, V: PartialEq>(
    chains: &mut HashMap<K, Box<VersionedRecord<Option<V>>>>,
    key: K,
    before: Option<V>,
    after: Option<V>,
    txid: TxId,
    commit_ts: Timestamp,
  ) {
    if let Some(head) = chains.get_mut(&key) {
      if head.commit_ts == commit_ts {
        head.data = after;
        return;
      }
    }
    if before == after {
      return;
    }
    let prev = match chains.remove(&key) {
      Some(mut head) => {
        head.data = before;
        Some(head)
      }
      None => before
        .is_some()
        .then(|| Box::new(VersionedRecord::new(before, 0, 0))),
    };
    let version = VersionedRecord {
      data: after,
      txid,
      commit_ts,
      prev,
      deleted: false,
    };
    chains.insert(key, Box::new(version));
  }

  // ========================================================================
  // History: reads
  // ========================================================================
  //
  // What a reader at `snapshot_ts` (in transaction `txid`, 0 outside one) sees of a key in
  // its chain. `None` when there is no chain or the reader sees its newest version: the
  // reader then reads the committed delta and snapshot.

  /// Node `node_id` (`Some(None)`: absent).
  pub fn node_at(
    &self,
    node_id: NodeId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<Option<&NodeVersionData>> {
    let head = self.node_versions.get(&TxKey::Node(node_id))?;
    history_version(head, snapshot_ts, txid)
      .map(|version| version.filter(|v| !v.deleted).map(|v| &v.data))
  }

  /// Whether node `node_id` exists.
  pub fn node_exists_at(
    &self,
    node_id: NodeId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    self
      .node_at(node_id, snapshot_ts, txid)
      .map(|node| node.is_some())
  }

  /// Whether the edge `src -[etype]-> dst` exists.
  pub fn edge_exists_at(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    let head = self.edge_versions.get(&Self::edge_key(src, etype, dst))?;
    history_version(head, snapshot_ts, txid).map(|version| version.is_some_and(|v| v.data.added))
  }

  /// A node property (`Some(None)`: unset).
  pub fn node_prop_at(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<Option<PropValueRef>> {
    let key = Self::node_prop_key(node_id, prop_key_id);
    let value = if self.use_soa {
      self.soa_node_props.history(&key, snapshot_ts, txid)
    } else {
      Self::history_in(&self.legacy_node_props, &key, snapshot_ts, txid)
    };
    value.map(|value| value.cloned())
  }

  /// An edge property (`Some(None)`: unset).
  pub fn edge_prop_at(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<Option<PropValueRef>> {
    let key = Self::edge_prop_key(src, etype, dst, prop_key_id);
    let value = if self.use_soa {
      self.soa_edge_props.history(&key, snapshot_ts, txid)
    } else {
      Self::history_in(&self.legacy_edge_props, &key, snapshot_ts, txid)
    };
    value.map(|value| value.cloned())
  }

  /// Whether node `node_id` has label `label_id`.
  pub fn node_label_at(
    &self,
    node_id: NodeId,
    label_id: LabelId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<bool> {
    let key = Self::node_label_key(node_id, label_id);
    let value = if self.use_soa {
      self.soa_node_labels.history(&key, snapshot_ts, txid)
    } else {
      Self::history_in(&self.legacy_node_labels, &key, snapshot_ts, txid)
    };
    value.map(|value| value.copied().unwrap_or(false))
  }

  /// The live node holding `key` (`Some(None)`: none).
  pub fn key_owner_at(
    &self,
    key: &str,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<Option<NodeId>> {
    Self::history_in(&self.key_owners, key, snapshot_ts, txid).map(|owner| owner.copied())
  }

  /// Nodes that a reader sees in their chains, as existing.
  pub fn nodes_at(&self, snapshot_ts: Timestamp, txid: TxId) -> impl Iterator<Item = NodeId> + '_ {
    self.node_versions.values().filter_map(move |head| {
      history_version(head, snapshot_ts, txid)
        .flatten()
        .filter(|version| !version.deleted)
        .map(|version| version.data.node_id)
    })
  }

  /// Edges `(src, etype, dst)` that a reader sees in their chains, as existing.
  pub fn edges_at(
    &self,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> impl Iterator<Item = (NodeId, ETypeId, NodeId)> + '_ {
    self.edge_versions.values().filter_map(move |head| {
      history_version(head, snapshot_ts, txid)
        .flatten()
        .filter(|version| version.data.added)
        .map(|version| (version.data.src, version.data.etype, version.data.dst))
    })
  }

  /// Edges `(src, etype, dst)` with endpoint `node_id` that a reader sees in their chains,
  /// as existing.
  pub fn node_edges_at(
    &self,
    node_id: NodeId,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> impl Iterator<Item = (NodeId, ETypeId, NodeId)> + '_ {
    self
      .edge_chains_by_node
      .get(&node_id)
      .into_iter()
      .flatten()
      .copied()
      .filter(move |&(src, etype, dst)| {
        self.edge_exists_at(src, etype, dst, snapshot_ts, txid) == Some(true)
      })
  }

  /// `*_at` for a map of boxed chains of optional values (`Some(None)`: absent).
  fn history_in<'a, K, Q, V>(
    chains: &'a HashMap<K, Box<VersionedRecord<Option<V>>>>,
    key: &Q,
    snapshot_ts: Timestamp,
    txid: TxId,
  ) -> Option<Option<&'a V>>
  where
    K: Borrow<Q> + Eq + Hash,
    Q: Eq + Hash + ?Sized,
  {
    let head = chains.get(key)?;
    history_version(head, snapshot_ts, txid).map(|version| version.and_then(|v| v.data.as_ref()))
  }

  fn index_edge_chain(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    for node_id in [src, dst] {
      self
        .edge_chains_by_node
        .entry(node_id)
        .or_default()
        .insert((src, etype, dst));
    }
  }

  /// Drop the index entries of edge chains that no longer exist.
  fn unindex_dropped_edge_chains(&mut self) {
    let chains = &self.edge_versions;
    self.edge_chains_by_node.retain(|_, edges| {
      edges.retain(|&(src, etype, dst)| chains.contains_key(&Self::edge_key(src, etype, dst)));
      !edges.is_empty()
    });
  }

  // ========================================================================
  // Helper methods
  // ========================================================================

  /// Convert a pooled version to a VersionedRecord (for API compatibility)
  fn pooled_to_versioned<T: Clone, K: Eq + Hash + Clone>(
    store: &SoaPropertyVersions<Option<T>, K>,
    pooled: PooledVersion<&Option<T>>,
  ) -> VersionedRecord<Option<T>> {
    let prev = if pooled.prev_idx != NULL_IDX {
      store
        .at(pooled.prev_idx)
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
  fn clone_versioned_record<T: Clone>(
    record: &VersionedRecord<Option<T>>,
  ) -> VersionedRecord<Option<T>> {
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

    pruned += Self::prune_chains(&mut self.node_versions, horizon_ts);
    let edge_chains = self.edge_versions.len();
    pruned += Self::prune_chains(&mut self.edge_versions, horizon_ts);
    if self.edge_versions.len() != edge_chains {
      self.unindex_dropped_edge_chains();
    }
    pruned += Self::prune_chains(&mut self.key_owners, horizon_ts);

    // Prune property and label versions
    if self.use_soa {
      pruned += self.soa_node_props.prune_old_versions(horizon_ts);
      pruned += self.soa_edge_props.prune_old_versions(horizon_ts);
      pruned += self.soa_node_labels.prune_old_versions(horizon_ts);
    } else {
      pruned += Self::prune_chains(&mut self.legacy_node_props, horizon_ts);
      pruned += Self::prune_chains(&mut self.legacy_edge_props, horizon_ts);
      pruned += Self::prune_chains(&mut self.legacy_node_labels, horizon_ts);
    }

    pruned
  }

  /// Prune every chain in a map, removing chains that are entirely older than horizon_ts
  fn prune_chains<K, T>(
    chains: &mut HashMap<K, Box<VersionedRecord<T>>>,
    horizon_ts: Timestamp,
  ) -> usize {
    let mut pruned = 0;
    chains.retain(|_, version| {
      let result = Self::prune_chain(version, horizon_ts);
      if result == -1 {
        pruned += 1;
        false
      } else {
        pruned += result as usize;
        true
      }
    });
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

    truncated += Self::truncate_chains(&mut self.node_versions, max_depth, min_active_ts);
    truncated += Self::truncate_chains(&mut self.edge_versions, max_depth, min_active_ts);
    truncated += Self::truncate_chains(&mut self.key_owners, max_depth, min_active_ts);

    // Truncate property and label version chains
    if self.use_soa {
      truncated += self
        .soa_node_props
        .truncate_deep_chains(max_depth, min_active_ts);
      truncated += self
        .soa_edge_props
        .truncate_deep_chains(max_depth, min_active_ts);
      truncated += self
        .soa_node_labels
        .truncate_deep_chains(max_depth, min_active_ts);
    } else {
      truncated += Self::truncate_chains(&mut self.legacy_node_props, max_depth, min_active_ts);
      truncated += Self::truncate_chains(&mut self.legacy_edge_props, max_depth, min_active_ts);
      truncated += Self::truncate_chains(&mut self.legacy_node_labels, max_depth, min_active_ts);
    }

    truncated
  }

  /// Truncate every chain in a map; returns the number of chains truncated
  fn truncate_chains<K, T>(
    chains: &mut HashMap<K, Box<VersionedRecord<T>>>,
    max_depth: usize,
    min_active_ts: Option<Timestamp>,
  ) -> usize {
    let mut truncated = 0;
    for version in chains.values_mut() {
      if Self::truncate_chain_at_depth(version, max_depth, min_active_ts) {
        truncated += 1;
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
      node = match node.prev.as_mut() {
        Some(prev) => prev,
        None => return false,
      };
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
  pub fn soa_memory_usage(&self) -> (usize, usize) {
    (
      self.soa_node_props.memory_usage(),
      self.soa_edge_props.memory_usage(),
    )
  }

  /// Clear all versions
  pub fn clear(&mut self) {
    self.node_versions.clear();
    self.edge_versions.clear();
    self.edge_chains_by_node.clear();
    self.key_owners.clear();
    self.soa_node_props.clear();
    self.soa_edge_props.clear();
    self.soa_node_labels.clear();
    self.legacy_node_props.clear();
    self.legacy_edge_props.clear();
    self.legacy_node_labels.clear();
  }

  /// Get counts for statistics
  pub fn counts(&self) -> VersionChainCounts {
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
      node_label_versions: if self.use_soa {
        self.soa_node_labels.len()
      } else {
        self.legacy_node_labels.len()
      },
      key_owner_versions: self.key_owners.len(),
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
  pub node_label_versions: usize,
  pub key_owner_versions: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mvcc::visibility::visible_version;
  use crate::types::PropValue;

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

    let head = store.head(1);
    assert!(head.is_some());
    let v = head.expect("expected value");
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

    let head = store.head(1).expect("expected value");
    assert_eq!(*head.data, 30);
    assert_eq!(head.commit_ts, 30);
    assert_ne!(head.prev_idx, NULL_IDX);

    // Follow chain
    let prev = store.at(head.prev_idx).expect("expected value");
    assert_eq!(*prev.data, 20);
  }

  #[test]
  fn test_version_chain_manager_new() {
    let mgr = VersionChainManager::new();
    assert!(mgr.is_soa_enabled());
    let counts = mgr.counts();
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

    let version = mgr.node_version(1);
    assert!(version.is_some());
    assert_eq!(version.expect("expected value").data.node_id, 1);
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

    let version = mgr.node_version(1).expect("expected value");
    assert_eq!(version.commit_ts, 30);
    assert!(version.prev.is_some());
    assert_eq!(version.prev.as_ref().expect("expected value").commit_ts, 20);
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

    let version = mgr.node_version(1).expect("expected value");
    assert!(version.deleted);
    assert_eq!(version.commit_ts, 20);
  }

  #[test]
  fn test_edge_version_append_and_get() {
    let mut mgr = VersionChainManager::new();

    mgr.append_edge_version(1, 1, 2, true, 1, 10);

    let version = mgr.edge_version(1, 1, 2);
    assert!(version.is_some());
    let v = version.expect("expected value");
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

    let version = mgr.edge_version(1, 1, 2).expect("expected value");
    assert!(!version.data.added);
  }

  #[test]
  fn test_node_prop_version_soa() {
    let mut mgr = VersionChainManager::new();
    assert!(mgr.is_soa_enabled());

    mgr.append_node_prop_version(1, 1, Some(std::sync::Arc::new(PropValue::I64(42))), 1, 10);

    let version = mgr.node_prop_version(1, 1);
    assert!(version.is_some());
    assert_eq!(
      version.expect("expected value").data.as_deref(),
      Some(&PropValue::I64(42))
    );
  }

  #[test]
  fn test_node_prop_version_legacy() {
    let mut mgr = VersionChainManager::with_soa(false);
    assert!(!mgr.is_soa_enabled());

    mgr.append_node_prop_version(1, 1, Some(std::sync::Arc::new(PropValue::I64(42))), 1, 10);

    let version = mgr.node_prop_version(1, 1);
    assert!(version.is_some());
    assert_eq!(
      version.expect("expected value").data.as_deref(),
      Some(&PropValue::I64(42))
    );
  }

  #[test]
  fn test_edge_prop_version() {
    let mut mgr = VersionChainManager::new();

    mgr.append_edge_prop_version(
      1,
      1,
      2,
      1,
      Some(std::sync::Arc::new(PropValue::F64(std::f64::consts::PI))),
      1,
      10,
    );

    let version = mgr.edge_prop_version(1, 1, 2, 1);
    assert!(version.is_some());
    assert_eq!(
      version.expect("expected value").data.as_deref(),
      Some(&PropValue::F64(std::f64::consts::PI))
    );
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
    mgr.append_node_prop_version(1, 1, Some(std::sync::Arc::new(PropValue::I64(42))), 1, 10);

    mgr.clear();

    let counts = mgr.counts();
    assert_eq!(counts.node_versions, 0);
    assert_eq!(counts.edge_versions, 0);
    assert_eq!(counts.node_prop_versions, 0);
    assert_eq!(counts.node_label_versions, 0);
  }

  #[test]
  fn test_edge_key_identity() {
    // Test that full component values remain part of the key identity.
    let key1 = VersionChainManager::edge_key(1, 2, 3);
    let key2 = VersionChainManager::edge_key(1, 2, 4);
    let key3 = VersionChainManager::edge_key(2, 2, 3);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
    assert_ne!(key2, key3);
  }

  #[test]
  fn test_node_prop_key_identity() {
    let key1 = VersionChainManager::node_prop_key(1, 1);
    let key2 = VersionChainManager::node_prop_key(1, 2);
    let key3 = VersionChainManager::node_prop_key(2, 1);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
  }

  #[test]
  fn test_edge_prop_key_identity() {
    let key1 = VersionChainManager::edge_prop_key(1, 1, 2, 1);
    let key2 = VersionChainManager::edge_prop_key(1, 1, 2, 2);
    let key3 = VersionChainManager::edge_prop_key(1, 1, 3, 1);

    assert_ne!(key1, key2);
    assert_ne!(key1, key3);
  }

  #[test]
  fn test_edge_versions_do_not_collide_for_source_nodes_2pow20_apart() {
    let mut mgr = VersionChainManager::new();
    let far_source = 1 + (1 << 20);

    mgr.append_edge_version(1, 7, 99, true, 1, 10);
    mgr.append_edge_version(far_source, 7, 99, false, 2, 20);

    let first = mgr
      .edge_version(1, 7, 99)
      .expect("first edge version should have its own chain");
    assert_eq!(first.data.src, 1);
    assert!(first.data.added);
    assert_eq!(first.commit_ts, 10);

    let second = mgr
      .edge_version(far_source, 7, 99)
      .expect("second edge version should have its own chain");
    assert_eq!(second.data.src, far_source);
    assert!(!second.data.added);
    assert_eq!(second.commit_ts, 20);

    let first_visible = visible_version(first, 15, 3).expect("first edge should be visible");
    assert_eq!(first_visible.data.src, 1);
    assert!(visible_version(second, 15, 3).is_none());
  }

  #[test]
  fn test_edge_property_versions_do_not_collide_for_property_ids_above_12_bits() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      let value_one = std::sync::Arc::new(PropValue::I64(1));
      let value_four_thousand_ninety_seven = std::sync::Arc::new(PropValue::I64(4097));

      mgr.append_edge_prop_version(11, 3, 19, 1, Some(value_one.clone()), 1, 10);
      mgr.append_edge_prop_version(
        11,
        3,
        19,
        4097,
        Some(value_four_thousand_ninety_seven.clone()),
        2,
        20,
      );

      let first = mgr
        .edge_prop_version(11, 3, 19, 1)
        .expect("first edge property should have its own chain");
      assert_eq!(first.data.as_deref(), Some(value_one.as_ref()));
      assert_eq!(first.commit_ts, 10);

      let second = mgr
        .edge_prop_version(11, 3, 19, 4097)
        .expect("second edge property should have its own chain");
      assert_eq!(
        second.data.as_deref(),
        Some(value_four_thousand_ninety_seven.as_ref())
      );
      assert_eq!(second.commit_ts, 20);
      assert_eq!(mgr.edge_prop_keys(11, 3, 19), vec![1, 4097]);
    }
  }

  #[test]
  fn test_soa_memory_usage() {
    let mut mgr = VersionChainManager::new();

    // Add some versions
    for i in 0..100 {
      mgr.append_node_prop_version(
        i,
        1,
        Some(std::sync::Arc::new(PropValue::I64(i as i64))),
        1,
        10,
      );
    }

    let (node_mem, _edge_mem) = mgr.soa_memory_usage();
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
    let mut current = mgr.node_version(1);
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

  #[test]
  fn test_soa_truncate_with_active_reader() {
    let deep_chain = || {
      let mut mgr = VersionChainManager::new();
      for i in 1..=20u64 {
        let value = Some(std::sync::Arc::new(PropValue::I64(i as i64)));
        mgr.append_node_prop_version(1, 1, value, i, i);
      }
      mgr
    };
    let visible_at = |mgr: &VersionChainManager, ts: Timestamp| {
      let head = mgr.node_prop_version(1, 1).expect("prop chain");
      let visible = visible_version(&head, ts, 999).and_then(|v| v.data.as_deref().cloned());
      (head.chain_depth(), visible)
    };

    // The reader's version (ts=18) is within max_depth: the chain is cut at max_depth.
    let mut mgr = deep_chain();
    assert_eq!(mgr.truncate_deep_chains(5, Some(19)), 1);
    assert_eq!(visible_at(&mgr, 19), (5, Some(PropValue::I64(18))));

    // The reader's version (ts=2) is deeper than max_depth: the chain keeps it.
    let mut mgr = deep_chain();
    assert_eq!(mgr.truncate_deep_chains(5, Some(3)), 1);
    assert_eq!(visible_at(&mgr, 3), (19, Some(PropValue::I64(2))));
  }
}

#[cfg(test)]
mod audit_tests {
  use super::*;
  use crate::mvcc::visibility::visible_version;
  use crate::types::PropValue;
  use std::sync::Arc;

  const NODE: NodeId = 1;
  const LABEL: LabelId = 7;
  const PROP: PropKeyId = 3;
  const READER_TXID: TxId = 999;

  fn label_depth(mgr: &VersionChainManager) -> usize {
    mgr
      .node_label_version(NODE, LABEL)
      .map_or(0, |head| head.chain_depth())
  }

  fn prop_depth(mgr: &VersionChainManager) -> usize {
    mgr
      .node_prop_version(NODE, PROP)
      .map_or(0, |head| head.chain_depth())
  }

  fn i64_value(value: i64) -> Option<PropValueRef> {
    Some(Arc::new(PropValue::I64(value)))
  }

  // M4: label chains must be pruned like property chains.

  #[test]
  fn audit_m4_prune_reclaims_label_chain_older_than_horizon() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      mgr.append_node_prop_version(NODE, PROP, None, 0, 0);
      mgr.append_node_prop_version(NODE, PROP, i64_value(1), 1, 5);
      mgr.append_node_label_version(NODE, LABEL, None, 0, 0);
      mgr.append_node_label_version(NODE, LABEL, Some(true), 1, 5);
      mgr.append_node_label_version(NODE, LABEL, None, 2, 6);

      mgr.prune_old_versions(100);

      assert!(
        mgr.node_prop_version(NODE, PROP).is_none(),
        "control: prop chain below the horizon is reclaimed (soa={use_soa})"
      );
      assert!(
        mgr.node_label_version(NODE, LABEL).is_none(),
        "label chain below the horizon must be reclaimed (soa={use_soa})"
      );
      assert_eq!(mgr.counts().node_label_versions, 0, "soa={use_soa}");
      assert!(mgr.node_label_keys(NODE).is_empty(), "soa={use_soa}");
    }
  }

  #[test]
  fn audit_m4_prune_trims_label_chain_to_newest_below_horizon() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      let history: [(TxId, Timestamp); 5] = [(0, 0), (1, 5), (2, 6), (3, 7), (4, 20)];
      for (i, &(txid, ts)) in history.iter().enumerate() {
        let present = i % 2 == 1;
        mgr.append_node_prop_version(NODE, PROP, i64_value(i as i64), txid, ts);
        mgr.append_node_label_version(NODE, LABEL, present.then_some(true), txid, ts);
      }

      // Readers are at ts >= 10: they need the ts=20 head and the ts=7 version.
      mgr.prune_old_versions(10);

      assert_eq!(prop_depth(&mgr), 2, "control: prop chain (soa={use_soa})");
      assert_eq!(
        label_depth(&mgr),
        2,
        "label chain must keep only the head and the newest version below the horizon (soa={use_soa})"
      );
      let head = mgr
        .node_label_version(NODE, LABEL)
        .expect("label head survives");
      let visible = visible_version(&head, 10, READER_TXID).expect("reader at ts=10 sees ts=7");
      assert_eq!(visible.data, Some(true), "soa={use_soa}");
    }
  }

  #[test]
  fn audit_m4_truncate_bounds_label_chain_depth() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      for i in 1..=20u64 {
        mgr.append_node_prop_version(NODE, PROP, i64_value(i as i64), i, i);
        mgr.append_node_label_version(NODE, LABEL, (i % 2 == 0).then_some(true), i, i);
      }

      mgr.truncate_deep_chains(5, None);

      assert!(
        prop_depth(&mgr) <= 5,
        "control: prop chain truncated (soa={use_soa})"
      );
      assert!(
        label_depth(&mgr) <= 5,
        "label chain depth {} must be truncated to max_depth 5 (soa={use_soa})",
        label_depth(&mgr)
      );
    }
  }

  #[test]
  fn audit_m4_gc_reclaims_label_versions_after_readers_finish() {
    use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
    use std::sync::mpsc;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(
      open_single_file(
        dir.path().join("m4.kitedb"),
        SingleFileOpenOptions::new()
          .mvcc(true)
          .mvcc_gc_interval_ms(50)
          .mvcc_retention_ms(0)
          .auto_checkpoint(false),
      )
      .expect("open db"),
    );

    db.begin(false).expect("begin");
    let node = db.create_node(Some("n")).expect("create node");
    let label = db.define_label("Tag").expect("define label");
    let prop = db.define_propkey("p").expect("define propkey");
    db.set_node_prop(node, prop, PropValue::I64(0))
      .expect("set prop");
    db.commit().expect("commit");

    // An open reader makes the following commits publish versions.
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let reader_db = Arc::clone(&db);
    let reader = std::thread::spawn(move || {
      reader_db.begin(true).expect("begin read tx");
      ready_tx.send(()).expect("ready");
      let _ = release_rx.recv();
      reader_db.commit().expect("commit read tx");
    });
    ready_rx.recv().expect("reader ready");

    for (i, add) in [true, false, true, false].into_iter().enumerate() {
      db.begin(false).expect("begin");
      if add {
        db.add_node_label(node, label).expect("add label");
      } else {
        db.remove_node_label(node, label).expect("remove label");
      }
      db.set_node_prop(node, prop, PropValue::I64(i as i64 + 1))
        .expect("set prop");
      db.commit().expect("commit");
    }

    let mvcc = db.mvcc.as_ref().expect("mvcc enabled").clone();
    {
      let vc = mvcc.version_chain.lock();
      assert!(vc.counts().node_prop_versions > 0, "prop versions created");
      assert!(
        vc.counts().node_label_versions > 0,
        "label versions created"
      );
    }

    release_tx.send(()).expect("release reader");
    reader.join().expect("reader thread");
    std::thread::sleep(std::time::Duration::from_millis(20));

    {
      let mut tx_mgr = mvcc.tx_manager.lock();
      let mut vc = mvcc.version_chain.lock();
      let mut gc = mvcc.gc.lock();
      let _ = gc.run_gc(&mut tx_mgr, &mut vc);
    }

    {
      let vc = mvcc.version_chain.lock();
      let counts = vc.counts();
      assert_eq!(
        counts.node_prop_versions, 0,
        "control: prop versions reclaimed once no reader needs them"
      );
      assert_eq!(
        counts.node_label_versions, 0,
        "label versions must be reclaimed once no reader needs them"
      );
    }

    drop(mvcc);
    let db = match Arc::try_unwrap(db) {
      Ok(db) => db,
      Err(_) => panic!("db still shared"),
    };
    close_single_file(db).expect("close db");
  }

  // M5: truncation must keep the newest version below min_active_ts.

  #[test]
  fn audit_m5_truncate_keeps_node_prop_version_of_oldest_reader() {
    const READER_TS: Timestamp = 5;
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      for (txid, ts, value) in [(1, 1, 100), (2, 2, 200), (3, 3, 300)] {
        mgr.append_node_prop_version(NODE, PROP, i64_value(value), txid, ts);
      }
      // Twelve commits after the reader's snapshot (more than max_depth 10).
      for i in 0..12u64 {
        mgr.append_node_prop_version(NODE, PROP, i64_value(1000 + i as i64), 10 + i, 6 + i);
      }

      mgr.truncate_deep_chains(10, Some(READER_TS));

      let head = mgr.node_prop_version(NODE, PROP).expect("prop chain");
      let visible = visible_version(&head, READER_TS, READER_TXID);
      assert_eq!(
        visible.and_then(|v| v.data.as_deref().cloned()),
        Some(PropValue::I64(300)),
        "reader at ts={READER_TS} must still see the ts=3 value (soa={use_soa})"
      );
    }
  }

  #[test]
  fn audit_m5_truncate_keeps_edge_prop_version_of_oldest_reader() {
    const READER_TS: Timestamp = 5;
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      mgr.append_edge_prop_version(1, 2, 3, PROP, None, 0, 0);
      mgr.append_edge_prop_version(1, 2, 3, PROP, i64_value(300), 3, 3);
      for i in 0..12u64 {
        mgr.append_edge_prop_version(1, 2, 3, PROP, i64_value(1000 + i as i64), 10 + i, 6 + i);
      }

      mgr.truncate_deep_chains(10, Some(READER_TS));

      let head = mgr
        .edge_prop_version(1, 2, 3, PROP)
        .expect("edge prop chain");
      let visible = visible_version(&head, READER_TS, READER_TXID);
      assert_eq!(
        visible.and_then(|v| v.data.as_deref().cloned()),
        Some(PropValue::I64(300)),
        "reader at ts={READER_TS} must still see the ts=3 edge value (soa={use_soa})"
      );
    }
  }
}

#[cfg(test)]
mod history_tests {
  use super::*;
  use crate::types::PropValue;

  const NODE: NodeId = 1;
  const OTHER: NodeId = 2;
  const ETYPE: ETypeId = 3;
  const PROP: PropKeyId = 4;
  const LABEL: LabelId = 5;
  const READER_TXID: TxId = 999;

  fn value(value: i64) -> Option<PropValueRef> {
    Some(Arc::new(PropValue::I64(value)))
  }

  fn node(key: &str) -> Option<NodeVersionData> {
    Some(NodeVersionData {
      node_id: NODE,
      delta: NodeDelta {
        key: Some(key.to_string()),
        ..NodeDelta::default()
      },
    })
  }

  #[test]
  fn prop_history_answers_only_readers_older_than_the_newest_version() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      mgr.record_node_prop(NODE, PROP, value(1), value(2), 10, 10);

      let at = |mgr: &VersionChainManager, ts| mgr.node_prop_at(NODE, PROP, ts, READER_TXID);
      assert_eq!(at(&mgr, 10), Some(value(1)), "soa={use_soa}");
      assert_eq!(
        at(&mgr, 11),
        None,
        "current state from ts 11 (soa={use_soa})"
      );
      assert_eq!(mgr.node_prop_at(NODE, PROP, 5, 10), None, "own commit");
    }
  }

  #[test]
  fn record_rewrites_the_newest_version_to_the_state_it_replaces() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      mgr.record_node_prop(NODE, PROP, None, value(1), 10, 10);
      // Commit 20 set the prop to 2 with no other transaction open, recording nothing.
      mgr.record_node_prop(NODE, PROP, value(2), value(3), 30, 30);

      let at = |ts| mgr.node_prop_at(NODE, PROP, ts, READER_TXID);
      assert_eq!(at(10), Some(None), "absent before ts 10 (soa={use_soa})");
      assert_eq!(at(21), Some(value(2)), "soa={use_soa}");
      assert_eq!(at(31), None, "soa={use_soa}");
    }
  }

  #[test]
  fn record_by_the_same_commit_replaces_and_unchanged_state_records_nothing() {
    for use_soa in [true, false] {
      let mut mgr = VersionChainManager::with_soa(use_soa);
      mgr.record_node_label(NODE, LABEL, true, true, 10, 10);
      assert_eq!(mgr.counts().node_label_versions, 0, "soa={use_soa}");

      mgr.record_node_label(NODE, LABEL, true, false, 10, 10);
      mgr.record_node_label(NODE, LABEL, true, true, 10, 10);
      assert_eq!(mgr.node_label_at(NODE, LABEL, 10, READER_TXID), Some(true));
      let head = mgr.node_label_version(NODE, LABEL).expect("label chain");
      assert_eq!(
        (head.data, head.chain_depth()),
        (Some(true), 2),
        "soa={use_soa}"
      );
    }
  }

  #[test]
  fn node_and_edge_history() {
    let mut mgr = VersionChainManager::new();
    mgr.record_node(NODE, node("a"), None, 10, 10);
    mgr.record_edge(NODE, ETYPE, OTHER, true, false, 10, 10);
    mgr.record_edge(OTHER, ETYPE, NODE, false, true, 10, 10);
    mgr.record_key_owner("a", Some(NODE), None, 10, 10);

    let key = mgr
      .node_at(NODE, 10, READER_TXID)
      .map(|node| node.and_then(|node| node.delta.key.clone()));
    assert_eq!(key, Some(Some("a".to_string())));
    assert_eq!(mgr.node_exists_at(NODE, 11, READER_TXID), None);
    assert_eq!(mgr.key_owner_at("a", 10, READER_TXID), Some(Some(NODE)));
    assert_eq!(mgr.key_owner_at("a", 11, READER_TXID), None);
    assert_eq!(
      mgr.nodes_at(10, READER_TXID).collect::<Vec<_>>(),
      vec![NODE]
    );

    // Added at ts 10: absent before, with no baseline version.
    assert_eq!(
      mgr.edge_exists_at(OTHER, ETYPE, NODE, 10, READER_TXID),
      Some(false)
    );
    let edges: Vec<_> = mgr.node_edges_at(OTHER, 10, READER_TXID).collect();
    assert_eq!(edges, vec![(NODE, ETYPE, OTHER)]);
    assert_eq!(mgr.edges_at(10, READER_TXID).count(), 1);
    assert_eq!(mgr.node_edges_at(OTHER, 11, READER_TXID).count(), 0);
  }

  #[test]
  fn prune_drops_the_endpoint_index_of_dropped_edge_chains() {
    let mut mgr = VersionChainManager::new();
    mgr.record_edge(NODE, ETYPE, OTHER, true, false, 10, 10);
    mgr.record_edge(OTHER, ETYPE, OTHER, true, false, 20, 20);

    mgr.prune_old_versions(15);

    assert!(!mgr.edge_chains_by_node.contains_key(&NODE));
    let other: Vec<_> = mgr.node_edges_at(OTHER, 20, READER_TXID).collect();
    assert_eq!(other, vec![(OTHER, ETYPE, OTHER)]);
  }
}
