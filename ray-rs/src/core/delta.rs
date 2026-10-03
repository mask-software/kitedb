//! In-memory delta overlay for uncommitted changes
//!
//! Ported from src/core/delta.ts

use crate::core::snapshot::reader::SnapshotData;
use crate::types::*;
use hashbrown::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::hash::Hash;
use std::sync::OnceLock;

/// A table of a delta (one of its maps or sets), for `DeltaState::grow_tables_for`.
trait Table {
  fn len(&self) -> usize;
  fn capacity(&self) -> usize;
  fn reserve(&mut self, additional: usize);
}

impl<K: Eq + Hash, V> Table for DeltaMap<K, V> {
  fn len(&self) -> usize {
    hashbrown::HashMap::len(self)
  }
  fn capacity(&self) -> usize {
    hashbrown::HashMap::capacity(self)
  }
  fn reserve(&mut self, additional: usize) {
    hashbrown::HashMap::reserve(self, additional)
  }
}

impl<T: Eq + Hash> Table for DeltaSet<T> {
  fn len(&self) -> usize {
    hashbrown::HashSet::len(self)
  }
  fn capacity(&self) -> usize {
    hashbrown::HashSet::capacity(self)
  }
  fn reserve(&mut self, additional: usize) {
    hashbrown::HashSet::reserve(self, additional)
  }
}

/// A table this full (or fuller) may grow ahead of need (see
/// `DeltaState::grow_tables_for`), as a fraction of its capacity.
const GROW_AHEAD_LOAD: (usize, usize) = (3, 4);
/// Tables smaller than this grow when they fill: their growth takes no time.
const GROW_AHEAD_MIN_CAPACITY: usize = 4096;

/// Whether `snapshot` holds the edge `src -[etype]-> dst`.
pub fn snapshot_has_edge(
  snapshot: Option<&SnapshotData>,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> bool {
  let Some(snap) = snapshot else {
    return false;
  };
  match (snap.phys_node(src), snap.phys_node(dst)) {
    (Some(src_phys), Some(dst_phys)) => snap.has_edge(src_phys, etype, dst_phys),
    _ => false,
  }
}

/// Node ids per chunk of a `NodeMap` dense part.
const CHUNK_IDS: usize = 64;
/// A `NodeMap` takes a dense part once it holds this many entries in a map...
const DENSE_MIN_NODES: usize = 1024;
/// ...at least half of them among the newest ids of a range this many times
/// their number.
const DENSE_MAX_SPREAD: u64 = 4;
/// A dense part covers at most this many ids from its first.
const DENSE_MAX_IDS: u64 = 1 << 26;
/// A `NodeMap` read in order while it holds more than this many sparse ids
/// keeps them in order from then on; reads in order sort fewer as they go.
const ORDER_MIN_SPARSE: usize = 64;

/// The nodes a delta created (or recreated), with their state.
pub type CreatedNodes = NodeMap<NodeDelta>;

/// The nodes a delta added each label to: those of its created and modified
/// nodes whose `labels` name it, and possibly more, as removing a label or a
/// node leaves the entry (readers check the node). `nodes_with_label` reads it
/// instead of every node the delta holds.
pub type LabelIndex = DeltaMap<LabelId, Vec<NodeId>>;

/// A delta's map from node ids to `V`: its created nodes, and its edge patches
/// by node.
///
/// Node ids mostly come from a counter, so a delta that creates many nodes
/// holds a dense range of ids: once it does, those live in chunks of
/// `CHUNK_IDS` ids indexed by id, and the rest in a hash map. A lookup in the
/// dense part reads one slot, with no hashing; merging an entry into it writes
/// its slot and moves nothing else; the part grows a chunk at a time, never
/// moving what it holds; and it lists its entries in id order.
///
/// It offers what code uses of a map: `get`, `get_key_value`, `get_mut`,
/// `contains_key`, `insert`, `remove`, `entry(..).or_default()`, `iter`,
/// `keys`, `values`, `drain`, `len`, `is_empty`, `clear`; and `keys_from`,
/// its ids in order from one on, which pages read instead of every entry.
#[derive(Debug, Clone)]
pub struct NodeMap<V> {
  /// The dense part, if taken: ids `[base, base + DENSE_MAX_IDS)` (some of them).
  dense: Option<DenseNodes<V>>,
  /// Entries outside the dense part.
  sparse: DeltaMap<NodeId, V>,
  /// The ids of `sparse` in order, from the first read in order that found
  /// more than `ORDER_MIN_SPARSE` of them on (see `sparse_order`). Maps never
  /// read in order (a transaction's, mostly) never pay for it.
  order: OnceLock<BTreeSet<NodeId>>,
}

impl<V> Default for NodeMap<V> {
  fn default() -> Self {
    Self {
      dense: None,
      sparse: DeltaMap::default(),
      order: OnceLock::new(),
    }
  }
}

#[derive(Debug, Clone)]
struct DenseNodes<V> {
  /// The first id, a multiple of `CHUNK_IDS`.
  base: NodeId,
  /// Chunk `i` holds ids `base + i * CHUNK_IDS ..`; `None` while it holds none.
  chunks: Vec<Option<Box<NodeChunk<V>>>>,
  len: usize,
}

#[derive(Debug, Clone)]
struct NodeChunk<V> {
  len: usize,
  slots: [Option<(NodeId, V)>; CHUNK_IDS],
}

impl<V> NodeChunk<V> {
  fn new() -> Box<Self> {
    Box::new(Self {
      len: 0,
      slots: std::array::from_fn(|_| None),
    })
  }
}

impl<V> DenseNodes<V> {
  /// The chunk and slot of `id`, if the dense part covers it.
  #[inline]
  fn slot_of(&self, id: NodeId) -> Option<(usize, usize)> {
    let offset = id.checked_sub(self.base)?;
    (offset < DENSE_MAX_IDS).then(|| {
      let offset = offset as usize;
      (offset / CHUNK_IDS, offset % CHUNK_IDS)
    })
  }
}

impl<V> NodeMap<V> {
  /// Number of entries.
  #[inline]
  pub fn len(&self) -> usize {
    self.dense.as_ref().map_or(0, |dense| dense.len) + self.sparse.len()
  }

  #[inline]
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Whether the dense part, if any, covers `id`: then `id` is there or nowhere.
  fn dense_covers(&self, id: NodeId) -> bool {
    self
      .dense
      .as_ref()
      .is_some_and(|dense| dense.slot_of(id).is_some())
  }

  #[inline]
  pub fn contains_key(&self, id: &NodeId) -> bool {
    match &self.dense {
      None => self.sparse.contains_key(id),
      Some(dense) => self.get_key_value_dense(dense, id).is_some(),
    }
  }

  #[inline]
  pub fn get(&self, id: &NodeId) -> Option<&V> {
    match &self.dense {
      None => self.sparse.get(id),
      Some(dense) => self.get_key_value_dense(dense, id).map(|(_, value)| value),
    }
  }

  #[inline]
  pub fn get_key_value(&self, id: &NodeId) -> Option<(&NodeId, &V)> {
    match &self.dense {
      None => self.sparse.get_key_value(id),
      Some(dense) => self.get_key_value_dense(dense, id),
    }
  }

  /// `get_key_value` with a dense part: kept out of line, so the many reads
  /// that look up a map that has none (most maps of most deltas) stay small.
  #[inline(never)]
  fn get_key_value_dense<'a>(
    &'a self,
    dense: &'a DenseNodes<V>,
    id: &NodeId,
  ) -> Option<(&'a NodeId, &'a V)> {
    match dense.slot_of(*id) {
      Some((chunk, slot)) => dense.chunks.get(chunk)?.as_ref()?.slots[slot]
        .as_ref()
        .map(|(id, value)| (id, value)),
      None => self.sparse.get_key_value(id),
    }
  }

  pub fn get_mut(&mut self, id: &NodeId) -> Option<&mut V> {
    if !self.dense_covers(*id) {
      return self.sparse.get_mut(id);
    }
    let dense = self.dense.as_mut()?;
    let (chunk, slot) = dense.slot_of(*id)?;
    let (_, value) = dense.chunks.get_mut(chunk)?.as_mut()?.slots[slot].as_mut()?;
    Some(value)
  }

  /// Insert `value` for `id`, returning the value it replaces.
  pub fn insert(&mut self, id: NodeId, value: V) -> Option<V> {
    if !self.dense_covers(id) {
      let replaced = self.sparse.insert(id, value);
      if replaced.is_none() {
        if let Some(order) = self.order.get_mut() {
          order.insert(id);
        }
        self.take_dense_part();
      }
      return replaced;
    }
    let dense = self.dense.as_mut()?;
    let (chunk, slot) = dense.slot_of(id)?;
    if dense.chunks.len() <= chunk {
      dense.chunks.resize_with(chunk + 1, || None);
    }
    let chunk = dense.chunks[chunk].get_or_insert_with(NodeChunk::new);
    let replaced = chunk.slots[slot]
      .replace((id, value))
      .map(|(_, value)| value);
    if replaced.is_none() {
      chunk.len += 1;
      dense.len += 1;
    }
    replaced
  }

  pub fn remove(&mut self, id: &NodeId) -> Option<V> {
    if !self.dense_covers(*id) {
      let removed = self.sparse.remove(id);
      if let (Some(_), Some(order)) = (&removed, self.order.get_mut()) {
        order.remove(id);
      }
      return removed;
    }
    let dense = self.dense.as_mut()?;
    let (chunk_index, slot) = dense.slot_of(*id)?;
    let chunk = dense.chunks.get_mut(chunk_index)?.as_mut()?;
    let (_, value) = chunk.slots[slot].take()?;
    chunk.len -= 1;
    dense.len -= 1;
    if chunk.len == 0 {
      dense.chunks[chunk_index] = None;
    }
    Some(value)
  }

  /// The entry of `id`, to fill if empty.
  pub fn entry(&mut self, id: NodeId) -> NodeMapEntry<'_, V> {
    NodeMapEntry { map: self, id }
  }

  /// The entries, the dense part's in id order, then the rest.
  pub fn iter(&self) -> NodeMapIter<'_, V> {
    NodeMapIter {
      chunks: self
        .dense
        .as_ref()
        .map_or(&[][..], |dense| &dense.chunks)
        .iter(),
      slots: [].iter(),
      sparse: self.sparse.iter(),
    }
  }

  pub fn keys(&self) -> impl Iterator<Item = &NodeId> + '_ {
    self.iter().map(|(id, _)| id)
  }

  /// The ids from `from` on, in order: a seek into each part, so taking `n`
  /// costs about `n` entries (plus sorting up to `ORDER_MIN_SPARSE` sparse
  /// ids), however many the map holds. The first such read of a map with more
  /// sparse ids puts them in order (see `sparse_order`).
  pub fn keys_from(&self, from: NodeId) -> NodeMapKeysFrom<'_, V> {
    let sparse = match self.sparse_order() {
      Some(order) => SparseKeys::Ordered(order.range(from..)),
      None => {
        let mut ids: Vec<NodeId> = self
          .sparse
          .keys()
          .copied()
          .filter(|&id| id >= from)
          .collect();
        ids.sort_unstable();
        SparseKeys::Sorted(ids.into_iter())
      }
    };
    let dense = match &self.dense {
      None => DenseKeys::default(),
      Some(dense) => {
        let (chunk, slot) = match from.checked_sub(dense.base) {
          None => (0, 0),
          Some(offset) if offset < DENSE_MAX_IDS => {
            let offset = offset as usize;
            (offset / CHUNK_IDS, offset % CHUNK_IDS)
          }
          Some(_) => (dense.chunks.len(), 0),
        };
        DenseKeys {
          chunks: dense.chunks.get(chunk..).unwrap_or_default(),
          slot,
        }
      }
    };
    let mut keys = NodeMapKeysFrom {
      dense,
      sparse,
      next_dense: None,
      next_sparse: None,
    };
    keys.next_dense = keys.dense.next();
    keys.next_sparse = keys.sparse.next();
    keys
  }

  pub fn values(&self) -> impl Iterator<Item = &V> + '_ {
    self.iter().map(|(_, value)| value)
  }

  /// Take every entry out, leaving this empty.
  pub fn drain(&mut self) -> NodeMapDrain<'_, V> {
    self.order = OnceLock::new();
    let chunks = self
      .dense
      .as_mut()
      .map(|dense| {
        dense.len = 0;
        std::mem::take(&mut dense.chunks)
      })
      .unwrap_or_default();
    NodeMapDrain {
      chunks: chunks.into_iter(),
      chunk: None,
      sparse: self.sparse.drain(),
    }
  }

  pub fn clear(&mut self) {
    self.dense = None;
    self.sparse.clear();
    self.order = OnceLock::new();
  }

  /// The sparse ids in order, if kept, or put in order now if there are more
  /// than `ORDER_MIN_SPARSE` of them: then changes keep them in order (a sorted
  /// set insert per sparse id), until the map is cleared or takes a dense part.
  /// Concurrent readers (under the delta's read lock) build it once.
  fn sparse_order(&self) -> Option<&BTreeSet<NodeId>> {
    if let Some(order) = self.order.get() {
      return Some(order);
    }
    (self.sparse.len() > ORDER_MIN_SPARSE).then(|| {
      self
        .order
        .get_or_init(|| self.sparse.keys().copied().collect())
    })
  }

  /// Take the dense part once the map holds `DENSE_MIN_NODES` (and at each
  /// doubling after) and at least half of them lie among the newest ids of a
  /// range at most `DENSE_MAX_SPREAD` times their number: those move there.
  fn take_dense_part(&mut self) {
    let len = self.sparse.len();
    if self.dense.is_some() || len < DENSE_MIN_NODES || !len.is_power_of_two() {
      return;
    }
    let Some(&newest) = self.sparse.keys().max() else {
      return;
    };
    let span = DENSE_MAX_SPREAD * len as u64;
    let base = newest.saturating_sub(span - 1) / CHUNK_IDS as u64 * CHUNK_IDS as u64;
    let dense_ids = self.sparse.keys().filter(|&&id| id >= base).count();
    if dense_ids * 2 < len {
      return;
    }
    self.dense = Some(DenseNodes {
      base,
      chunks: Vec::new(),
      len: 0,
    });
    let moved: Vec<NodeId> = self
      .sparse
      .keys()
      .copied()
      .filter(|&id| self.dense_covers(id))
      .collect();
    for id in moved {
      if let Some(value) = self.sparse.remove(&id) {
        self.insert(id, value);
      }
    }
    // Put the rest in order again when next read so.
    self.order = OnceLock::new();
  }

  /// Capacity of the hash map part (test instrumentation: the dense part never
  /// moves what it holds).
  #[cfg(test)]
  pub(crate) fn map_capacity(&self) -> usize {
    self.sparse.capacity()
  }
}

/// The entry of one id in a `NodeMap` (see `NodeMap::entry`).
pub struct NodeMapEntry<'a, V> {
  map: &'a mut NodeMap<V>,
  id: NodeId,
}

impl<'a, V> NodeMapEntry<'a, V> {
  /// The value, inserted with `default` if there is none.
  pub fn or_insert_with(self, default: impl FnOnce() -> V) -> &'a mut V {
    let Self { map, id } = self;
    if !map.contains_key(&id) {
      map.insert(id, default());
    }
    match map.get_mut(&id) {
      Some(value) => value,
      None => unreachable!("node map entry {id} inserted just now"),
    }
  }

  pub fn or_default(self) -> &'a mut V
  where
    V: Default,
  {
    self.or_insert_with(V::default)
  }
}

/// Draining iterator of a `NodeMap` (see `NodeMap::drain`). It takes a dense
/// chunk's entries one slot at a time, in place.
pub struct NodeMapDrain<'a, V> {
  chunks: std::vec::IntoIter<Option<Box<NodeChunk<V>>>>,
  /// The chunk being drained, and its next slot.
  chunk: Option<(Box<NodeChunk<V>>, usize)>,
  sparse: hashbrown::hash_map::Drain<'a, NodeId, V>,
}

impl<V> Iterator for NodeMapDrain<'_, V> {
  type Item = (NodeId, V);

  fn next(&mut self) -> Option<Self::Item> {
    loop {
      if let Some((chunk, next)) = &mut self.chunk {
        while *next < CHUNK_IDS {
          let slot = chunk.slots[*next].take();
          *next += 1;
          if slot.is_some() {
            return slot;
          }
        }
        self.chunk = None;
      }
      match self.chunks.next() {
        Some(Some(chunk)) => self.chunk = Some((chunk, 0)),
        Some(None) => {}
        None => return self.sparse.next(),
      }
    }
  }
}

/// Iterator over a `NodeMap` (see `NodeMap::iter`).
pub struct NodeMapIter<'a, V> {
  chunks: std::slice::Iter<'a, Option<Box<NodeChunk<V>>>>,
  slots: std::slice::Iter<'a, Option<(NodeId, V)>>,
  sparse: hashbrown::hash_map::Iter<'a, NodeId, V>,
}

impl<'a, V> Iterator for NodeMapIter<'a, V> {
  type Item = (&'a NodeId, &'a V);

  fn next(&mut self) -> Option<Self::Item> {
    loop {
      if let Some((id, value)) = self.slots.by_ref().flatten().next() {
        return Some((id, value));
      }
      match self.chunks.next() {
        Some(Some(chunk)) => self.slots = chunk.slots.iter(),
        Some(None) => {}
        None => return self.sparse.next(),
      }
    }
  }
}

impl<'a, V> IntoIterator for &'a NodeMap<V> {
  type Item = (&'a NodeId, &'a V);
  type IntoIter = NodeMapIter<'a, V>;

  fn into_iter(self) -> Self::IntoIter {
    self.iter()
  }
}

/// Ids of a `NodeMap` in order, from one on (see `NodeMap::keys_from`): its
/// dense part's and its sparse ids', merged.
pub struct NodeMapKeysFrom<'a, V> {
  dense: DenseKeys<'a, V>,
  sparse: SparseKeys<'a>,
  next_dense: Option<NodeId>,
  next_sparse: Option<NodeId>,
}

impl<V> Iterator for NodeMapKeysFrom<'_, V> {
  type Item = NodeId;

  #[inline]
  fn next(&mut self) -> Option<NodeId> {
    // The two parts hold distinct ids.
    match (self.next_dense, self.next_sparse) {
      (Some(dense), Some(sparse)) if sparse < dense => {
        self.next_sparse = self.sparse.next();
        Some(sparse)
      }
      (Some(dense), _) => {
        self.next_dense = self.dense.next();
        Some(dense)
      }
      (None, sparse) => {
        self.next_sparse = self.sparse.next();
        sparse
      }
    }
  }
}

/// The ids of a dense part's chunks in order, from a slot of the first on.
struct DenseKeys<'a, V> {
  chunks: &'a [Option<Box<NodeChunk<V>>>],
  /// The next slot of `chunks[0]`
  slot: usize,
}

impl<V> Default for DenseKeys<'_, V> {
  fn default() -> Self {
    Self {
      chunks: &[],
      slot: 0,
    }
  }
}

impl<V> Iterator for DenseKeys<'_, V> {
  type Item = NodeId;

  fn next(&mut self) -> Option<NodeId> {
    loop {
      let (first, rest) = self.chunks.split_first()?;
      if let Some(chunk) = first {
        while let Some(slot) = chunk.slots.get(self.slot) {
          self.slot += 1;
          if let Some((id, _)) = slot {
            return Some(*id);
          }
        }
      }
      self.chunks = rest;
      self.slot = 0;
    }
  }
}

/// A `NodeMap`'s sparse ids in order: from its order, or sorted as read.
enum SparseKeys<'a> {
  Ordered(std::collections::btree_set::Range<'a, NodeId>),
  Sorted(std::vec::IntoIter<NodeId>),
}

impl Iterator for SparseKeys<'_> {
  type Item = NodeId;

  #[inline]
  fn next(&mut self) -> Option<NodeId> {
    match self {
      Self::Ordered(ids) => ids.next().copied(),
      Self::Sorted(ids) => ids.next(),
    }
  }
}

impl<V> Table for NodeMap<V> {
  fn len(&self) -> usize {
    self.sparse.len()
  }
  /// Entries the hash map part takes before it grows; with a dense part, as
  /// many as needed (the dense part takes new nodes, a chunk at a time).
  fn capacity(&self) -> usize {
    if self.dense.is_some() {
      usize::MAX / 2
    } else {
      self.sparse.capacity()
    }
  }
  fn reserve(&mut self, additional: usize) {
    if self.dense.is_none() {
      self.sparse.reserve(additional);
    }
  }
}

/// A delta's added edge patches in one direction, by node, with their count.
/// (Tombstones, which name edges of nodes committed before, stay in hash
/// maps.) It reads as its `NodeMap` of patch sets; changes go through it, so
/// the count stays right.
#[derive(Debug, Clone, Default)]
pub struct EdgePatches {
  sets: NodeMap<BTreeSet<EdgePatch>>,
  /// The patches of all the sets: `count_edges` takes it instead of adding
  /// them up.
  patches: usize,
}

impl std::ops::Deref for EdgePatches {
  type Target = NodeMap<BTreeSet<EdgePatch>>;

  #[inline]
  fn deref(&self) -> &Self::Target {
    &self.sets
  }
}

impl<'a> IntoIterator for &'a EdgePatches {
  type Item = (&'a NodeId, &'a BTreeSet<EdgePatch>);
  type IntoIter = NodeMapIter<'a, BTreeSet<EdgePatch>>;

  fn into_iter(self) -> Self::IntoIter {
    self.sets.iter()
  }
}

impl EdgePatches {
  /// The number of patches, in all the sets.
  #[inline]
  pub fn patch_count(&self) -> usize {
    self.patches
  }

  /// Add `patch` to `node`'s set; whether it was not there.
  pub fn insert_patch(&mut self, node: NodeId, patch: EdgePatch) -> bool {
    let inserted = self.sets.entry(node).or_default().insert(patch);
    self.patches += usize::from(inserted);
    inserted
  }

  /// Remove `patch` from `node`'s set, and the set once empty; whether it was
  /// there.
  pub fn remove_patch(&mut self, node: NodeId, patch: &EdgePatch) -> bool {
    let Some(set) = self.sets.get_mut(&node) else {
      return false;
    };
    if !set.remove(patch) {
      return false;
    }
    self.patches -= 1;
    if set.is_empty() {
      self.sets.remove(&node);
    }
    true
  }

  /// Add `patches` to `node`'s set: moved in whole if it has none.
  fn extend_patches(&mut self, node: NodeId, patches: BTreeSet<EdgePatch>) {
    match self.sets.get_mut(&node) {
      Some(existing) => {
        let before = existing.len();
        existing.extend(patches);
        self.patches += existing.len() - before;
      }
      None => {
        self.patches += patches.len();
        self.sets.insert(node, patches);
      }
    }
  }

  /// Take every set out, leaving this empty.
  pub fn drain(&mut self) -> NodeMapDrain<'_, BTreeSet<EdgePatch>> {
    self.patches = 0;
    self.sets.drain()
  }

  pub fn clear(&mut self) {
    self.patches = 0;
    self.sets.clear();
  }
}

impl Table for EdgePatches {
  fn len(&self) -> usize {
    Table::len(&self.sets)
  }
  fn capacity(&self) -> usize {
    Table::capacity(&self.sets)
  }
  fn reserve(&mut self, additional: usize) {
    Table::reserve(&mut self.sets, additional)
  }
}

impl DeltaState {
  /// Create empty delta state
  pub fn new() -> Self {
    Self::default()
  }

  // ========================================================================
  // Layered Edge Operations
  // ========================================================================
  //
  // A delta overlays a base layer (the snapshot, or for a transaction the
  // committed state). `in_base` says whether the base holds the edge. These
  // keep `out_add` disjoint from the base and `out_del` inside it, so a
  // re-added base edge is never counted twice and deleting a base edge always
  // hides it.

  /// Whether the edge is visible through this delta.
  pub fn edge_visible(&self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) -> bool {
    self.is_edge_added(src, etype, dst) || (in_base && !self.is_edge_deleted(src, etype, dst))
  }

  /// Add an edge unless it is already visible.
  pub fn add_edge_over(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) {
    if self.edge_visible(src, etype, dst, in_base) {
      return;
    }
    if !in_base {
      // A tombstone for an edge the base lacks must not swallow the add.
      self.remove_edge_patch(src, etype, dst, false);
    }
    self.add_edge(src, etype, dst);
  }

  /// Delete an edge if it is visible. A base edge always gets a tombstone,
  /// even when the delta also held an add patch for it. The edge's props in
  /// this delta go with it.
  pub fn delete_edge_over(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, in_base: bool) {
    if !self.edge_visible(src, etype, dst, in_base) {
      return;
    }
    self.edge_props.remove(&(src, etype, dst));
    self.remove_edge_patch(src, etype, dst, true);
    if in_base {
      self
        .out_del
        .entry(src)
        .or_default()
        .insert(EdgePatch { etype, other: dst });
      self
        .in_del
        .entry(dst)
        .or_default()
        .insert(EdgePatch { etype, other: src });
    }
  }

  /// Remove an add patch (`added`) or a tombstone (`!added`) in both directions.
  fn remove_edge_patch(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, added: bool) {
    let (out_patch, in_patch) = (
      EdgePatch { etype, other: dst },
      EdgePatch { etype, other: src },
    );
    if added {
      self.out_add.remove_patch(src, &out_patch);
      self.in_add.remove_patch(dst, &in_patch);
    } else {
      remove_tombstone(&mut self.out_del, src, out_patch);
      remove_tombstone(&mut self.in_del, dst, in_patch);
    }
  }

  /// Whether `node_id` exists through this delta over `snapshot`.
  pub fn node_exists_over(&self, snapshot: Option<&SnapshotData>, node_id: NodeId) -> bool {
    self.is_node_created(node_id) || self.snapshot_node_over(snapshot, node_id)
  }

  /// Whether `snapshot` holds the node and this delta did not delete it: the
  /// node exists through its snapshot copy, unless the delta created it again.
  pub fn snapshot_node_over(&self, snapshot: Option<&SnapshotData>, node_id: NodeId) -> bool {
    !self.is_node_deleted(node_id) && snapshot.is_some_and(|snap| snap.has_node(node_id))
  }

  /// Whether `snapshot` holds the edge and this delta keeps the snapshot
  /// copies of both endpoints: a deleted or recreated endpoint masks the
  /// snapshot's edges. Edge tombstones are not applied here.
  pub fn snapshot_edge_over(
    &self,
    snapshot: Option<&SnapshotData>,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> bool {
    !self.is_node_deleted(src)
      && !self.is_node_deleted(dst)
      && snapshot_has_edge(snapshot, src, etype, dst)
  }

  /// Whether the edge is visible through this delta over `snapshot`.
  pub fn edge_exists_over(
    &self,
    snapshot: Option<&SnapshotData>,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> bool {
    if self.is_node_removed(src) || self.is_node_removed(dst) {
      return false;
    }
    if self.is_edge_added(src, etype, dst) {
      return true;
    }
    !self.is_edge_deleted(src, etype, dst) && self.snapshot_edge_over(snapshot, src, etype, dst)
  }

  /// Add edge with cancellation logic
  pub fn add_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    let patch = EdgePatch { etype, other: dst };

    // Check if cancels a pending delete
    if let Some(del_set) = self.out_del.get_mut(&src) {
      if del_set.remove(&patch) {
        if del_set.is_empty() {
          self.out_del.remove(&src);
        }
      } else {
        self.out_add.insert_patch(src, patch);
      }
    } else {
      self.out_add.insert_patch(src, patch);
    }

    // Same for in-edges
    let in_patch = EdgePatch { etype, other: src };
    if let Some(del_set) = self.in_del.get_mut(&dst) {
      if del_set.remove(&in_patch) {
        if del_set.is_empty() {
          self.in_del.remove(&dst);
        }
      } else {
        self.in_add.insert_patch(dst, in_patch);
      }
    } else {
      self.in_add.insert_patch(dst, in_patch);
    }
  }

  /// Delete edge with cancellation logic. The edge's props in this delta go
  /// with it: a later add of the same triple is a new edge.
  pub fn delete_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    self.edge_props.remove(&(src, etype, dst));
    let patch = EdgePatch { etype, other: dst };

    // Check if cancels a pending add (and its in-edge copy)
    if self.out_add.remove_patch(src, &patch) {
      self
        .in_add
        .remove_patch(dst, &EdgePatch { etype, other: src });
      return;
    }

    // Add to delete sets
    self.out_del.entry(src).or_default().insert(patch);
    let in_patch = EdgePatch { etype, other: src };
    self.in_del.entry(dst).or_default().insert(in_patch);
  }

  /// Check if edge is deleted in delta
  pub fn is_edge_deleted(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    self
      .out_del
      .get(&src)
      .map(|s| s.contains(&EdgePatch { etype, other: dst }))
      .unwrap_or(false)
  }

  /// Check if edge is added in delta
  pub fn is_edge_added(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    self
      .out_add
      .get(&src)
      .map(|s| s.contains(&EdgePatch { etype, other: dst }))
      .unwrap_or(false)
  }

  /// Clear all delta state
  pub fn clear(&mut self) {
    self.created_nodes.clear();
    self.deleted_nodes.clear();
    self.modified_nodes.clear();
    self.out_add.clear();
    self.out_del.clear();
    self.in_add.clear();
    self.in_del.clear();
    self.edge_props.clear();
    self.new_labels.clear();
    self.new_etypes.clear();
    self.new_propkeys.clear();
    self.key_index.clear();
    self.pending_vectors.clear();
    self.labeled_nodes.clear();
  }

  /// Empty the delta for another transaction to use, and return whether it
  /// is worth keeping: no table holds room for more than `max_capacity`
  /// entries (a large transaction's tables are better dropped than kept).
  pub(crate) fn clear_for_reuse(&mut self, max_capacity: usize) -> bool {
    self.clear();
    [
      Table::capacity(&self.created_nodes),
      Table::capacity(&self.deleted_nodes),
      Table::capacity(&self.modified_nodes),
      Table::capacity(&self.out_add),
      Table::capacity(&self.out_del),
      Table::capacity(&self.in_add),
      Table::capacity(&self.in_del),
      Table::capacity(&self.edge_props),
      Table::capacity(&self.new_labels),
      Table::capacity(&self.new_etypes),
      Table::capacity(&self.new_propkeys),
      Table::capacity(&self.key_index),
      self.pending_vectors.capacity(),
      self.labeled_nodes.capacity(),
    ]
    .into_iter()
    .all(|capacity| capacity <= max_capacity)
  }

  /// Get count of edges added for a source node
  pub fn edges_added_count(&self, src: NodeId) -> usize {
    self.out_add.get(&src).map(|s| s.len()).unwrap_or(0)
  }

  /// Get count of edges deleted for a source node
  pub fn edges_deleted_count(&self, src: NodeId) -> usize {
    self.out_del.get(&src).map(|s| s.len()).unwrap_or(0)
  }

  /// Total edges added across all nodes
  pub fn total_edges_added(&self) -> usize {
    self.out_add.patch_count()
  }

  /// Total edges deleted across all nodes
  pub fn total_edges_deleted(&self) -> usize {
    self.out_del.values().map(|s| s.len()).sum()
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a new node.
  ///
  /// Over a deleted id this recreates the node: the delete stays and keeps
  /// masking the base copy (its props, labels, key and edges), and the node
  /// starts fresh, without the edge patches or edge props this delta held
  /// for the old copy. Props of the old copy's base edges have no patch to
  /// find them by, so this scans `edge_props`; recreating an id is rare.
  pub fn create_node(&mut self, node_id: NodeId, key: Option<&str>) {
    let node_delta = NodeDelta {
      key: key.map(|s| s.to_string()),
      labels: None,
      labels_deleted: None,
      props: None,
    };
    self.install_created_node(node_id, node_delta);

    // Add to key index if key provided
    if let Some(k) = key {
      self.key_index.insert(k.to_string(), node_id);
    }
  }

  /// Make `node_delta` node `node_id`'s own copy here, as `create_node` does,
  /// without touching the key index.
  fn install_created_node(&mut self, node_id: NodeId, node_delta: NodeDelta) {
    if self.is_node_deleted(node_id) {
      self.modified_nodes.remove(&node_id);
      self.drop_edge_patches(node_id);
      self
        .edge_props
        .retain(|&(src, _, dst), _| src != node_id && dst != node_id);
    }
    self.created_nodes.insert(node_id, node_delta);
  }

  /// Delete a node
  pub fn delete_node(&mut self, node_id: NodeId) {
    // If it was created in this delta, remove it instead. A recreated node
    // keeps the delete that masks its base copy.
    if let Some(removed) = self.created_nodes.remove(&node_id) {
      // Remove from key index
      if let Some(key) = &removed.key {
        self.key_index.remove(key);
      }

      // State written to the node after it was created here (older versions
      // let writes to a deleted node through), and its edges, found through
      // its own patch sets: O(its degree).
      self.modified_nodes.remove(&node_id);
      self.drop_edge_patches(node_id);

      return;
    }

    // Mark as deleted
    self.deleted_nodes.insert(node_id);

    // Remove any modified state, and this delta's patches of its edges: the
    // delete masks the base copies, and an add patch would outlive the node.
    self.modified_nodes.remove(&node_id);
    self.drop_edge_patches(node_id);
  }

  /// Whether this delta holds its own copy of the node: created here, or
  /// recreated over a deleted base copy. Its state is the delta's alone.
  pub fn is_node_created(&self, node_id: NodeId) -> bool {
    self.created_nodes.contains_key(&node_id)
  }

  /// Whether this delta deleted the node's base copy, masking its props,
  /// labels, key and edges. True for a recreated node as well: check
  /// [`Self::is_node_removed`] for whether the node is gone.
  pub fn is_node_deleted(&self, node_id: NodeId) -> bool {
    self.deleted_nodes.contains(&node_id)
  }

  /// Whether the node is gone through this delta: deleted, and not created
  /// again.
  pub fn is_node_removed(&self, node_id: NodeId) -> bool {
    self.is_node_deleted(node_id) && !self.is_node_created(node_id)
  }

  /// Drop every edge patch (add or tombstone, both directions) incident to
  /// `node_id`, with the props of those edges. Patches are kept in both
  /// directions, so the node's own `out_*` and `in_*` sets name every one of
  /// them.
  fn drop_edge_patches(&mut self, node_id: NodeId) {
    for added in [true, false] {
      let (out_patches, in_patches) = if added {
        (self.out_add.get(&node_id), self.in_add.get(&node_id))
      } else {
        (self.out_del.get(&node_id), self.in_del.get(&node_id))
      };
      let out_edges = out_patches
        .into_iter()
        .flatten()
        .map(|patch| (node_id, patch.etype, patch.other));
      let in_edges = in_patches
        .into_iter()
        .flatten()
        .map(|patch| (patch.other, patch.etype, node_id));
      let edges: Vec<_> = out_edges.chain(in_edges).collect();
      for (src, etype, dst) in edges {
        self.remove_edge_patch(src, etype, dst, added);
        self.edge_props.remove(&(src, etype, dst));
      }
    }
  }

  /// Get node delta (for created or modified nodes)
  pub fn node_delta(&self, node_id: NodeId) -> Option<&NodeDelta> {
    self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
  }

  // ========================================================================
  // Node Property Operations
  // ========================================================================

  /// Set a node property
  pub fn set_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId, value: PropValue) {
    self.set_node_prop_ref(node_id, key_id, std::sync::Arc::new(value));
  }

  /// Set a node property using a shared value
  pub fn set_node_prop_ref(&mut self, node_id: NodeId, key_id: PropKeyId, value: PropValueRef) {
    // Get or create the node delta
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self.modified_nodes.entry(node_id).or_default()
    };

    let props = node_delta
      .props
      .get_or_insert_with(std::collections::HashMap::new);
    props.insert(key_id, Some(value));
  }

  /// Delete a node property
  pub fn delete_node_prop(&mut self, node_id: NodeId, key_id: PropKeyId) {
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self.modified_nodes.entry(node_id).or_default()
    };

    let props = node_delta
      .props
      .get_or_insert_with(std::collections::HashMap::new);
    // None value means deleted
    props.insert(key_id, None);
  }

  /// Get a node property from delta
  pub fn node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Option<Option<&PropValue>> {
    let node_delta = self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))?;

    let props = node_delta.props.as_ref()?;
    props.get(&key_id).map(|v| v.as_deref())
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Add a label to a node
  pub fn add_node_label(&mut self, node_id: NodeId, label_id: LabelId) {
    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self.modified_nodes.entry(node_id).or_default()
    };

    // Remove from deleted set if present
    if let Some(ref mut deleted) = node_delta.labels_deleted {
      deleted.remove(&label_id);
    }

    // Add to labels set
    let labels = node_delta
      .labels
      .get_or_insert_with(std::collections::HashSet::new);
    if labels.insert(label_id) {
      self
        .labeled_nodes
        .entry(label_id)
        .or_default()
        .push(node_id);
    }
  }

  /// Remove a label from a node
  pub fn remove_node_label(&mut self, node_id: NodeId, label_id: LabelId) {
    let is_created = self.created_nodes.contains_key(&node_id);

    let node_delta = if let Some(node_delta) = self.created_nodes.get_mut(&node_id) {
      node_delta
    } else {
      self.modified_nodes.entry(node_id).or_default()
    };

    // Remove from added labels if present
    if let Some(ref mut labels) = node_delta.labels {
      labels.remove(&label_id);
    }

    // If not a new node, mark as deleted
    if !is_created {
      let deleted = node_delta
        .labels_deleted
        .get_or_insert_with(std::collections::HashSet::new);
      deleted.insert(label_id);
    }
  }

  /// Check if a label was added to a node in delta
  pub fn is_label_added(&self, node_id: NodeId, label_id: LabelId) -> bool {
    if let Some(node_delta) = self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
    {
      if let Some(ref labels) = node_delta.labels {
        return labels.contains(&label_id);
      }
    }
    false
  }

  /// Check if a label was removed from a node in delta
  pub fn is_label_removed(&self, node_id: NodeId, label_id: LabelId) -> bool {
    if let Some(node_delta) = self.modified_nodes.get(&node_id) {
      if let Some(ref deleted) = node_delta.labels_deleted {
        return deleted.contains(&label_id);
      }
    }
    false
  }

  /// Get labels added in delta for a node
  pub fn added_labels(&self, node_id: NodeId) -> Option<&std::collections::HashSet<LabelId>> {
    self
      .created_nodes
      .get(&node_id)
      .or_else(|| self.modified_nodes.get(&node_id))
      .and_then(|d| d.labels.as_ref())
  }

  /// Get labels removed in delta for a node
  pub fn removed_labels(&self, node_id: NodeId) -> Option<&std::collections::HashSet<LabelId>> {
    self
      .modified_nodes
      .get(&node_id)
      .and_then(|d| d.labels_deleted.as_ref())
  }

  // ========================================================================
  // Definition Operations
  // ========================================================================

  /// Define a new label
  pub fn define_label(&mut self, label_id: LabelId, name: &str) {
    self.new_labels.insert(label_id, name.to_string());
  }

  /// Define a new edge type
  pub fn define_etype(&mut self, etype_id: ETypeId, name: &str) {
    self.new_etypes.insert(etype_id, name.to_string());
  }

  /// Define a new property key
  pub fn define_propkey(&mut self, propkey_id: PropKeyId, name: &str) {
    self.new_propkeys.insert(propkey_id, name.to_string());
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  pub fn set_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
    value: PropValue,
  ) {
    self.set_edge_prop_ref(src, etype, dst, key_id, std::sync::Arc::new(value));
  }

  /// Set an edge property using a shared value
  pub fn set_edge_prop_ref(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
    value: PropValueRef,
  ) {
    let edge_key = (src, etype, dst);
    let props = self.edge_props.entry(edge_key).or_default();
    props.insert(key_id, Some(value));
  }

  /// Delete an edge property
  pub fn delete_edge_prop(&mut self, src: NodeId, etype: ETypeId, dst: NodeId, key_id: PropKeyId) {
    let edge_key = (src, etype, dst);
    let props = self.edge_props.entry(edge_key).or_default();
    props.insert(key_id, None);
  }

  /// Get an edge property from delta
  /// Returns Some(Some(value)) if set, Some(None) if deleted, None if not in delta
  pub fn edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Option<Option<&PropValue>> {
    let edge_key = (src, etype, dst);
    self
      .edge_props
      .get(&edge_key)
      .and_then(|props| props.get(&key_id))
      .map(|v| v.as_deref())
  }

  /// Get all edge property modifications in delta
  pub fn edge_props_delta(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Option<&HashMap<PropKeyId, Option<PropValueRef>>> {
    self.edge_props.get(&(src, etype, dst))
  }

  // ========================================================================
  // Key Index Operations
  // ========================================================================

  /// Lookup node by key in delta
  pub fn node_by_key(&self, key: &str) -> Option<NodeId> {
    self.key_index.get(key).copied()
  }

  /// Live node holding `key` through this delta over `snapshot`.
  pub fn key_owner_over(&self, snapshot: Option<&SnapshotData>, key: &str) -> Option<NodeId> {
    if let Some(&node_id) = self.key_index.get(key) {
      if !self.is_node_removed(node_id) {
        return Some(node_id);
      }
    }
    // A deleted or recreated node's snapshot key is masked with its copy.
    snapshot
      .and_then(|snap| snap.lookup_by_key(key))
      .filter(|&node_id| !self.is_node_deleted(node_id))
  }

  // ========================================================================
  // Commit Merge
  // ========================================================================

  /// Merge `pending`, a committed transaction's delta over this one, into
  /// this delta, and leave `pending` drained (its allocations are the
  /// caller's to drop, outside its locks).
  ///
  /// Commits merge under `delta.write()`, where every reader and writer
  /// waits. So wherever this delta holds nothing yet for an entry of
  /// `pending` (a new node's state, a new node's edge patches, a new edge's
  /// props: all of a typical commit), the entry moves in whole, with one map
  /// insert and no allocation. Elsewhere its changes apply one at a time, the
  /// way the transaction made them; both give the same result.
  pub(crate) fn merge_from(&mut self, pending: &mut DeltaState) {
    self.grow_tables_for(pending);
    // Small transactions touch few of the tables: the rest are skipped.
    if !(pending.new_labels.is_empty()
      && pending.new_etypes.is_empty()
      && pending.new_propkeys.is_empty())
    {
      self.new_labels.extend(pending.new_labels.drain());
      self.new_etypes.extend(pending.new_etypes.drain());
      self.new_propkeys.extend(pending.new_propkeys.drain());
    }

    // A node the transaction deleted (and did not recreate) takes the props
    // of its edges with it, also those it wrote to its committed edges.
    let mut removed = DeltaSet::new();
    if !pending.deleted_nodes.is_empty() {
      removed.extend(
        pending
          .deleted_nodes
          .iter()
          .copied()
          .filter(|&node_id| pending.is_node_removed(node_id)),
      );
      // Deletes first: a node the transaction deleted and created again is a
      // recreate, whose new copy replaces the committed one.
      for node_id in pending.deleted_nodes.drain() {
        self.delete_node(node_id);
      }
    }

    // A transaction's key index names exactly the keys of the nodes it
    // created (`create_node` adds one, `delete_node` takes it back), and is
    // copied last.
    if !pending.created_nodes.is_empty() {
      for (node_id, node_delta) in pending.created_nodes.drain() {
        debug_assert!(node_delta
          .key
          .as_deref()
          .is_none_or(|key| pending.key_index.get(key) == Some(&node_id)));
        self.merge_created_node(node_id, node_delta);
      }
    }
    if !pending.modified_nodes.is_empty() {
      for (node_id, node_delta) in pending.modified_nodes.drain() {
        self.merge_modified_node(node_id, node_delta);
      }
    }
    // The transaction added labels with `add_node_label`, which indexed them:
    // the merged nodes' labels are the committed ones' and those.
    if !pending.labeled_nodes.is_empty() {
      for (label_id, nodes) in pending.labeled_nodes.drain() {
        self
          .labeled_nodes
          .entry(label_id)
          .or_default()
          .extend(nodes);
      }
    }

    // Each direction merges from its own patches (a delta keeps them in
    // both): an add cancels a tombstone in its direction, as in `add_edge`.
    if !pending.out_add.is_empty() {
      for (src, patches) in pending.out_add.drain() {
        merge_added_patches(&mut self.out_add, &mut self.out_del, src, patches);
      }
    }
    if !pending.in_add.is_empty() {
      for (dst, patches) in pending.in_add.drain() {
        merge_added_patches(&mut self.in_add, &mut self.in_del, dst, patches);
      }
    }
    // `delete_edge` keeps both directions of a tombstone itself.
    if !pending.out_del.is_empty() {
      pending.in_del.clear();
      for (src, patches) in pending.out_del.drain() {
        for patch in patches {
          self.delete_edge(src, patch.etype, patch.other);
        }
      }
    }

    if !pending.edge_props.is_empty() {
      for (edge, props) in pending.edge_props.drain() {
        let (src, _, dst) = edge;
        if props.is_empty() || removed.contains(&src) || removed.contains(&dst) {
          continue;
        }
        match self.edge_props.entry(edge) {
          Entry::Vacant(entry) => {
            entry.insert(props);
          }
          Entry::Occupied(mut entry) => entry.get_mut().extend(props),
        }
      }
    }

    if !pending.key_index.is_empty() {
      self.key_index.extend(pending.key_index.drain());
    }
  }

  /// Make room for merging `pending` (see `merge_from`): grow each table the merge would
  /// overflow, and if none, the first table past `GROW_AHEAD_LOAD` of its capacity, ahead of
  /// need. A merge runs under `delta.write()`, and a table that grows moves all its entries
  /// meanwhile. A growing delta's tables hold about as many entries each (keys and edge props
  /// beside the created nodes and edge patches, whose dense parts never move), so they would
  /// fill up together and all grow in one merge: grown ahead, they grow one merge at a time.
  fn grow_tables_for(&mut self, pending: &DeltaState) {
    // Each table by name, so the checks compile to straight-line code: small
    // merges pay a few compares.
    macro_rules! each_table {
      ($check:ident) => {
        $check!(created_nodes);
        $check!(deleted_nodes);
        $check!(modified_nodes);
        $check!(out_add);
        $check!(out_del);
        $check!(in_add);
        $check!(in_del);
        $check!(edge_props);
        $check!(key_index);
      };
    }
    let mut grown = false;
    macro_rules! grow_if_overflowing {
      ($table:ident) => {
        let incoming = pending.$table.len();
        if Table::len(&self.$table) + incoming > Table::capacity(&self.$table) {
          Table::reserve(&mut self.$table, incoming);
          grown = true;
        }
      };
    }
    each_table!(grow_if_overflowing);
    if grown {
      return;
    }
    let (numerator, denominator) = GROW_AHEAD_LOAD;
    macro_rules! grow_if_filling {
      ($table:ident) => {
        let (len, capacity) = (Table::len(&self.$table), Table::capacity(&self.$table));
        if capacity >= GROW_AHEAD_MIN_CAPACITY
          && (len + pending.$table.len()).saturating_mul(denominator)
            >= capacity.saturating_mul(numerator)
        {
          // One past its capacity: the next size up.
          Table::reserve(&mut self.$table, capacity + 1 - len);
          return;
        }
      };
    }
    each_table!(grow_if_filling);
  }

  /// Merge node `node_id`, created by the merged transaction with the state
  /// `node_delta`: what `create_node` and then its label and prop writes
  /// give, except the key index entry (the transaction's key index has it).
  fn merge_created_node(&mut self, node_id: NodeId, mut node_delta: NodeDelta) {
    let NodeDelta {
      labels,
      labels_deleted,
      props,
      ..
    } = &mut node_delta;
    // Removing a label from a node created here only drops it from its
    // labels (see `remove_node_label`).
    if labels.as_ref().is_some_and(|labels| labels.is_empty()) {
      *labels = None;
    }
    if let Some(added) = labels.as_mut() {
      for label_id in labels_deleted.iter().flatten() {
        added.remove(label_id);
      }
    }
    *labels_deleted = None;
    if props.as_ref().is_some_and(|props| props.is_empty()) {
      *props = None;
    }
    self.install_created_node(node_id, node_delta);
  }

  /// Merge the label and prop changes `node_delta` that the merged
  /// transaction made to node `node_id`, which it did not create: what its
  /// `add_node_label`, `remove_node_label` and prop writes give, in that
  /// order.
  fn merge_modified_node(&mut self, node_id: NodeId, mut node_delta: NodeDelta) {
    let NodeDelta {
      key,
      labels,
      labels_deleted,
      props,
    } = &mut node_delta;
    *key = None;
    for set in [&mut *labels, &mut *labels_deleted] {
      if set.as_ref().is_some_and(|set| set.is_empty()) {
        *set = None;
      }
    }
    if props.as_ref().is_some_and(|props| props.is_empty()) {
      *props = None;
    }
    if labels.is_none() && labels_deleted.is_none() && props.is_none() {
      return;
    }

    if !self.created_nodes.contains_key(&node_id) {
      if let Entry::Vacant(entry) = self.modified_nodes.entry(node_id) {
        if let Some(added) = labels.as_mut() {
          for label_id in labels_deleted.iter().flatten() {
            added.remove(label_id);
          }
        }
        entry.insert(node_delta);
        return;
      }
    }

    let NodeDelta {
      labels,
      labels_deleted,
      props,
      ..
    } = node_delta;
    for label_id in labels.into_iter().flatten() {
      self.add_node_label(node_id, label_id);
    }
    for label_id in labels_deleted.into_iter().flatten() {
      self.remove_node_label(node_id, label_id);
    }
    for (key_id, value) in props.into_iter().flatten() {
      match value {
        Some(value) => self.set_node_prop_ref(node_id, key_id, value),
        None => self.delete_node_prop(node_id, key_id),
      }
    }
  }
}

/// Remove `patch` from `node`'s tombstones in `sets`, and the set once empty.
fn remove_tombstone(
  sets: &mut DeltaMap<NodeId, BTreeSet<EdgePatch>>,
  node: NodeId,
  patch: EdgePatch,
) {
  if let Some(set) = sets.get_mut(&node) {
    if set.remove(&patch) && set.is_empty() {
      sets.remove(&node);
    }
  }
}

/// Merge a transaction's add patches of `node` in one direction (`adds`, with
/// that direction's tombstones `tombstones`): each cancels a tombstone, or is
/// added, as in `DeltaState::add_edge`.
fn merge_added_patches(
  adds: &mut EdgePatches,
  tombstones: &mut DeltaMap<NodeId, BTreeSet<EdgePatch>>,
  node: NodeId,
  patches: BTreeSet<EdgePatch>,
) {
  if patches.is_empty() {
    return;
  }
  let Some(node_tombstones) = tombstones.get_mut(&node) else {
    adds.extend_patches(node, patches);
    return;
  };
  for patch in patches {
    if !node_tombstones.remove(&patch) {
      adds.insert_patch(node, patch);
    }
  }
  if node_tombstones.is_empty() {
    tombstones.remove(&node);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_add_edge() {
    let mut delta = DeltaState::new();
    delta.add_edge(1, 10, 2);

    assert!(delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2));
  }

  #[test]
  fn test_delete_edge() {
    let mut delta = DeltaState::new();
    delta.delete_edge(1, 10, 2);

    assert!(delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.is_edge_added(1, 10, 2));
  }

  #[test]
  fn test_add_cancels_delete() {
    let mut delta = DeltaState::new();
    delta.delete_edge(1, 10, 2);
    assert!(delta.is_edge_deleted(1, 10, 2));

    delta.add_edge(1, 10, 2);
    assert!(!delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.is_edge_added(1, 10, 2)); // Cancellation
  }

  #[test]
  fn test_delete_cancels_add() {
    let mut delta = DeltaState::new();
    delta.add_edge(1, 10, 2);
    assert!(delta.is_edge_added(1, 10, 2));

    delta.delete_edge(1, 10, 2);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2)); // Cancellation
  }

  #[test]
  fn test_add_over_base_edge_is_noop() {
    let mut delta = DeltaState::new();
    delta.add_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert_eq!(delta.total_edges_added(), 0);
    assert!(delta.edge_visible(1, 10, 2, true));
  }

  #[test]
  fn test_delete_over_base_edge_always_tombstones() {
    let mut delta = DeltaState::new();
    // A stray add patch for a base edge (written by older versions).
    delta.add_edge(1, 10, 2);
    delta.delete_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert!(delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.edge_visible(1, 10, 2, true));

    // Re-adding lifts the tombstone without an add patch.
    delta.add_edge_over(1, 10, 2, true);
    assert!(!delta.is_edge_deleted(1, 10, 2));
    assert!(!delta.is_edge_added(1, 10, 2));
  }

  #[test]
  fn test_recreate_keeps_base_masked_and_starts_fresh() {
    let (n, a, b, c) = (1, 2, 3, 4);
    let mut delta = DeltaState::new();
    // State the delta held for the base node n before its delete.
    delta.add_edge(n, 10, a);
    delta.add_edge(b, 10, n);
    delta.delete_edge(n, 10, c); // tombstone of a base edge
    delta.add_edge(a, 10, b); // unrelated
    delta.add_node_label(n, 7);
    delta.delete_node(n);
    assert!(delta.is_node_removed(n));

    delta.create_node(n, Some("new"));
    assert!(delta.is_node_created(n));
    assert!(delta.is_node_deleted(n), "the base copy stays masked");
    assert!(!delta.is_node_removed(n));
    assert!(delta.node_exists_over(None, n));
    assert_eq!(delta.key_owner_over(None, "new"), Some(n));
    assert!(delta.modified_nodes.is_empty());
    let all_patches = delta
      .out_add
      .iter()
      .chain(&delta.in_add)
      .chain(&delta.out_del)
      .chain(&delta.in_del);
    for (&node, patches) in all_patches {
      assert!(
        node != n && patches.iter().all(|p| p.other != n),
        "an old edge patch of n survived the recreate: {node} {patches:?}"
      );
    }
    assert!(delta.is_edge_added(a, 10, b), "unrelated patches stay");

    // Deleting the recreated node removes it again, base copy still masked.
    delta.delete_node(n);
    assert!(delta.is_node_removed(n));
    assert_eq!(delta.key_owner_over(None, "new"), None);
  }

  #[test]
  fn test_delete_created_node_drops_only_its_edges() {
    let mut delta = DeltaState::new();
    for node in 1..=4 {
      delta.create_node(node, None);
    }
    delta.add_edge(1, 10, 2);
    delta.add_edge(3, 10, 1);
    delta.add_edge(1, 10, 1);
    delta.add_edge(3, 10, 4);
    delta.add_edge(2, 11, 3);
    delta.delete_node(1);
    for map in [&delta.out_add, &delta.in_add] {
      assert!(
        map
          .iter()
          .all(|(&node, patches)| node != 1 && patches.iter().all(|p| p.other != 1)),
        "an edge patch of the deleted node survived: {map:?}"
      );
    }
    assert!(delta.is_edge_added(3, 10, 4) && delta.is_edge_added(2, 11, 3));
    assert_eq!(delta.total_edges_added(), 2);
    assert_eq!(delta.in_add.values().map(|s| s.len()).sum::<usize>(), 2);
  }

  #[test]
  fn test_edge_props_go_with_the_edge() {
    let mut delta = DeltaState::new();
    // A delta edge: deleting cancels the add patch and drops its props.
    delta.add_edge(1, 10, 2);
    delta.set_edge_prop(1, 10, 2, 7, PropValue::I64(5));
    delta.delete_edge(1, 10, 2);
    assert!(delta.edge_props_delta(1, 10, 2).is_none());

    // A base edge: the tombstone drops the props the delta held for it.
    delta.set_edge_prop(3, 10, 4, 7, PropValue::I64(6));
    delta.delete_edge_over(3, 10, 4, true);
    assert!(delta.is_edge_deleted(3, 10, 4));
    assert!(delta.edge_props_delta(3, 10, 4).is_none());

    // A deleted node's delta edges lose their props.
    delta.create_node(5, None);
    delta.create_node(6, None);
    delta.add_edge(5, 10, 6);
    delta.add_edge(6, 10, 5);
    delta.set_edge_prop(5, 10, 6, 7, PropValue::I64(7));
    delta.set_edge_prop(6, 10, 5, 7, PropValue::I64(8));
    delta.delete_node(6);
    assert!(delta.edge_props.is_empty(), "{:?}", delta.edge_props);
  }

  #[test]
  fn test_recreate_drops_props_of_the_old_copys_base_edges() {
    let mut delta = DeltaState::new();
    // Props the delta held for base edges of node 1, then its delete.
    delta.set_edge_prop(1, 10, 2, 7, PropValue::I64(5));
    delta.set_edge_prop(3, 10, 1, 7, PropValue::I64(6));
    delta.set_edge_prop(2, 10, 3, 7, PropValue::I64(7));
    delta.delete_node(1);
    delta.create_node(1, None);
    assert_eq!(
      delta.edge_props.keys().copied().collect::<Vec<_>>(),
      vec![(2, 10, 3)],
      "only the unrelated edge keeps its props"
    );
  }

  #[test]
  fn test_layered_ops_on_edge_missing_from_base() {
    let mut delta = DeltaState::new();
    delta.delete_edge_over(1, 10, 2, false);
    assert!(
      !delta.is_edge_deleted(1, 10, 2),
      "no tombstone for a missing edge"
    );

    // A stray tombstone must not swallow the add.
    delta.delete_edge(1, 10, 2);
    delta.add_edge_over(1, 10, 2, false);
    assert!(delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2));

    delta.delete_edge_over(1, 10, 2, false);
    assert!(!delta.is_edge_added(1, 10, 2));
    assert!(!delta.is_edge_deleted(1, 10, 2));
    assert!(delta.in_add.is_empty() && delta.in_del.is_empty());
  }

  /// A transaction's delta that creates `count` keyed nodes from `first` on, each with a
  /// prop and a label, and an edge from each to the next with three props.
  fn created_batch(first: NodeId, count: u64) -> DeltaState {
    let mut pending = DeltaState::new();
    for id in first..first + count {
      pending.create_node(id, Some(&format!("n{id}")));
      pending.set_node_prop(id, 1, PropValue::I64(id as i64));
      pending.add_node_label(id, 1);
    }
    for id in first..first + count {
      let dst = if id + 1 < first + count {
        id + 1
      } else {
        first
      };
      pending.add_edge(id, 1, dst);
      for key in 1..=3 {
        pending.set_edge_prop(id, 1, dst, key, PropValue::I64(key as i64));
      }
    }
    pending
  }

  /// Each table of `delta` as (capacity, bytes per entry).
  fn tables(delta: &DeltaState) -> Vec<(usize, usize)> {
    fn map<K, V>(map: &DeltaMap<K, V>) -> (usize, usize) {
      (map.capacity(), std::mem::size_of::<(K, V)>())
    }
    fn set<T>(set: &DeltaSet<T>) -> (usize, usize) {
      (set.capacity(), std::mem::size_of::<T>())
    }
    fn node_map<V>(map: &NodeMap<V>) -> (usize, usize) {
      (map.map_capacity(), std::mem::size_of::<(NodeId, V)>())
    }
    vec![
      node_map(&delta.created_nodes),
      set(&delta.deleted_nodes),
      map(&delta.modified_nodes),
      node_map(&delta.out_add),
      map(&delta.out_del),
      node_map(&delta.in_add),
      map(&delta.in_del),
      map(&delta.edge_props),
      map(&delta.key_index),
    ]
  }

  /// Merges run under the delta write lock, where every reader and writer waits, and a table
  /// that grows there moves all its entries. One merge must grow at most one table past 16K
  /// entries, and created nodes, once a dense range, must never move. Regression: every table
  /// of a growing delta grew in the same merge (they hold about as many entries each), a
  /// created node's entry being 176 bytes, so a merge stalled for 10 ms at 230K nodes.
  #[test]
  fn a_merge_grows_at_most_one_table() {
    let mut delta = DeltaState::new();
    let mut most_grown = (0, 0, 0);
    for batch in 0..40 {
      let mut pending = created_batch(1 + batch * 1000, 1000);
      let before = tables(&delta);
      delta.merge_from(&mut pending);
      let grown: Vec<usize> = before
        .iter()
        .zip(tables(&delta))
        .filter(|((before, _), (after, _))| before != after && *before >= 16 * 1024)
        .map(|((capacity, bytes), _)| capacity * bytes)
        .collect();
      if grown.len() > most_grown.0 {
        most_grown = (grown.len(), grown.iter().sum(), batch);
      }
    }
    assert!(
      most_grown.0 <= 1,
      "merge {} grew {} tables, moving {} bytes",
      most_grown.2,
      most_grown.0,
      most_grown.1
    );
    assert_eq!(delta.created_nodes.len(), 40_000);
    assert!(
      delta.created_nodes.map_capacity() < 4096,
      "created nodes stayed in a map of {} entries",
      delta.created_nodes.map_capacity()
    );
  }

  /// The bytes of table entries a delta holds.
  fn entry_bytes(delta: &DeltaState) -> usize {
    let lens = [
      delta.created_nodes.len(),
      delta.deleted_nodes.len(),
      delta.modified_nodes.len(),
      delta.out_add.len(),
      delta.out_del.len(),
      delta.in_add.len(),
      delta.in_del.len(),
      delta.edge_props.len(),
      delta.key_index.len(),
    ];
    tables(delta)
      .iter()
      .zip(lens)
      .map(|((_, bytes), len)| bytes * len)
      .sum()
  }

  /// Growing a delta's tables moves their entries (under the delta write lock): a delta that
  /// grows 1000 nodes a merge must have moved, in all, at most 1.75 times the bytes of the
  /// entries it holds, at every size past 20K nodes. Its created nodes and edge patches take
  /// dense parts, which never move what they hold. Regression: every table doubled, which
  /// moved up to 2.6 times over (a third of what merging a 200-node transaction cost).
  #[test]
  fn merges_move_table_entries_less_than_twice_over() {
    let mut delta = DeltaState::new();
    let (mut moved, mut worst) = (0, (0.0, 0));
    for batch in 0..200 {
      let mut pending = created_batch(1 + batch * 1000, 1000);
      let before = tables(&delta);
      delta.merge_from(&mut pending);
      moved += before
        .iter()
        .zip(tables(&delta))
        .filter(|((before, _), (after, _))| before != after)
        .map(|((capacity, bytes), _)| capacity * bytes)
        .sum::<usize>();
      let ratio = moved as f64 / entry_bytes(&delta) as f64;
      if batch >= 20 && ratio > worst.0 {
        worst = (ratio, batch);
      }
    }
    assert!(
      worst.0 <= 1.75,
      "growing the tables had moved {:.2} times the bytes they held after merge {}",
      worst.0,
      worst.1
    );
  }

  fn node(key: &str) -> NodeDelta {
    NodeDelta {
      key: Some(key.to_string()),
      ..NodeDelta::default()
    }
  }

  /// `CreatedNodes` answers as a map would through its switch to a dense part,
  /// for ids in it, below it, past it, recreated and removed.
  #[test]
  fn created_nodes_answer_as_a_map() {
    let mut nodes = CreatedNodes::default();
    let mut model: std::collections::BTreeMap<NodeId, String> = Default::default();
    let check = |nodes: &CreatedNodes, model: &std::collections::BTreeMap<NodeId, String>| {
      assert_eq!(nodes.len(), model.len());
      let mut listed: Vec<(NodeId, String)> = nodes
        .iter()
        .map(|(&id, node)| (id, node.key.clone().unwrap_or_default()))
        .collect();
      listed.sort();
      let expected: Vec<(NodeId, String)> =
        model.iter().map(|(&id, key)| (id, key.clone())).collect();
      assert_eq!(listed, expected);
      for (&id, key) in model {
        assert_eq!(
          nodes.get(&id).and_then(|node| node.key.as_deref()),
          Some(key.as_str())
        );
        assert!(nodes.contains_key(&id));
        assert_eq!(nodes.get_key_value(&id).map(|(&id, _)| id), Some(id));
      }
      for id in [0, 7, 999, 5_000, 1 << 40] {
        assert_eq!(nodes.contains_key(&id), model.contains_key(&id), "id {id}");
      }
    };
    // An old id first, then a run of new ones: the dense part takes the run.
    for id in std::iter::once(7).chain(10_000..12_100) {
      assert!(nodes.insert(id, node(&format!("k{id}"))).is_none());
      model.insert(id, format!("k{id}"));
    }
    assert!(nodes.dense.is_some(), "the run took a dense part");
    check(&nodes, &model);
    // Replace, remove (freeing a chunk), recreate, and ids far past the dense part.
    assert_eq!(
      nodes.insert(10_001, node("again")).and_then(|old| old.key),
      Some("k10001".to_string())
    );
    model.insert(10_001, "again".to_string());
    for id in 10_048..10_112 {
      assert!(nodes.remove(&id).is_some());
      model.remove(&id);
    }
    assert!(nodes.remove(&10_050).is_none());
    nodes.insert(10_050, node("back"));
    model.insert(10_050, "back".to_string());
    for id in [1 << 40, 3] {
      nodes.insert(id, node("far"));
      model.insert(id, "far".to_string());
    }
    if let Some(node) = nodes.get_mut(&12_000) {
      node.key = Some("changed".to_string());
    }
    model.insert(12_000, "changed".to_string());
    check(&nodes, &model);
    let clone = nodes.clone();
    check(&clone, &model);

    let mut drained: Vec<NodeId> = nodes.drain().map(|(id, _)| id).collect();
    drained.sort_unstable();
    assert_eq!(drained, model.keys().copied().collect::<Vec<_>>());
    assert!(nodes.is_empty());
    assert_eq!(nodes.iter().count(), 0);
    nodes.insert(10_000, node("after"));
    assert_eq!(
      nodes.get(&10_000).and_then(|node| node.key.as_deref()),
      Some("after")
    );
    nodes.clear();
    assert!(nodes.is_empty() && !nodes.contains_key(&10_000));
  }

  /// `keys_from` lists a `NodeMap`'s ids in order from any id, as a sorted map would:
  /// sparse ids below, inside and past the dense part's window, kept in order or not,
  /// through inserts, removals, freed chunks, the switch to a dense part, drains and
  /// clears; and `EdgePatches` counts its patches through every change.
  #[test]
  fn node_maps_list_their_ids_in_order() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    let check = |nodes: &NodeMap<u32>, model: &std::collections::BTreeMap<NodeId, u32>| {
      let mut froms = vec![0, 1, NodeId::MAX];
      froms.extend(
        model
          .keys()
          .flat_map(|&id| [id.saturating_sub(1), id, id + 1])
          .take(64),
      );
      for from in froms {
        let listed: Vec<NodeId> = nodes.keys_from(from).collect();
        let expected: Vec<NodeId> = model.range(from..).map(|(&id, _)| id).collect();
        assert_eq!(listed, expected, "from {from}");
      }
      assert_eq!(nodes.len(), model.len());
    };
    for seed in 0..4u64 {
      let mut rng = StdRng::seed_from_u64(seed);
      let mut nodes = NodeMap::<u32>::default();
      let mut model = std::collections::BTreeMap::new();
      // Spread ids (sparse, past `ORDER_MIN_SPARSE` so kept in order), then a run that
      // takes a dense part, ids far past its window, and churn.
      let spread: Vec<NodeId> = (0..100).map(|i| 1000 + i * 997).collect();
      let run: Vec<NodeId> = (200_000..203_000).collect();
      let far: Vec<NodeId> = (0..80).map(|i| 200_000 + DENSE_MAX_IDS + i * 31).collect();
      for (step, &id) in spread.iter().chain(&run).chain(&far).enumerate() {
        nodes.insert(id, step as u32);
        model.insert(id, step as u32);
        if step % 397 == 0 {
          check(&nodes, &model);
        }
      }
      assert!(
        nodes.dense.is_some(),
        "seed {seed}: the run took a dense part"
      );
      check(&nodes, &model);
      assert!(
        nodes.order.get().is_some(),
        "seed {seed}: the sparse ids are kept in order"
      );
      // Free whole chunks, remove at random, reinsert some.
      for id in 200_064..200_256 {
        nodes.remove(&id);
        model.remove(&id);
      }
      for _ in 0..2000 {
        let pool = [&spread[..], &run[..], &far[..]][rng.gen_range(0..3)];
        let id = pool[rng.gen_range(0..pool.len())];
        if rng.gen_bool(0.5) {
          assert_eq!(nodes.remove(&id), model.remove(&id));
        } else {
          assert_eq!(nodes.insert(id, 7), model.insert(id, 7));
        }
      }
      check(&nodes, &model);
      let mut drained: Vec<NodeId> = nodes.drain().map(|(id, _)| id).collect();
      drained.sort_unstable();
      assert_eq!(drained, model.keys().copied().collect::<Vec<_>>());
      model.clear();
      check(&nodes, &model);
      // Few sparse ids: sorted as read.
      for id in [9, 3, 7, 1_000_000, 5] {
        nodes.insert(id, 0);
        model.insert(id, 0);
      }
      check(&nodes, &model);
      assert!(
        nodes.order.get().is_none(),
        "few sparse ids are sorted as read"
      );
      nodes.clear();
      model.clear();
      check(&nodes, &model);
    }

    // A map not read in order keeps no order; once read so, changes keep it.
    let mut nodes = NodeMap::<u32>::default();
    for i in 0..100 {
      nodes.insert(1 + i * 1000, 0);
    }
    assert!(nodes.order.get().is_none());
    assert_eq!(nodes.keys_from(0).count(), 100);
    nodes.insert(500, 0);
    nodes.remove(&1001);
    assert!(nodes
      .order
      .get()
      .is_some_and(|order| order.contains(&500) && !order.contains(&1001)));
    assert_eq!(nodes.keys_from(400).next(), Some(500));

    // `EdgePatches` counts its patches through adds, deletes, cancels and merges.
    let mut rng = StdRng::seed_from_u64(9);
    let mut delta = DeltaState::new();
    let count = |delta: &DeltaState| {
      let out: usize = delta.out_add.values().map(BTreeSet::len).sum();
      let incoming: usize = delta.in_add.values().map(BTreeSet::len).sum();
      assert_eq!(delta.out_add.patch_count(), out);
      assert_eq!(delta.in_add.patch_count(), incoming);
      assert_eq!(out, incoming, "both directions hold every patch");
    };
    for round in 0..40 {
      let mut pending = DeltaState::new();
      for _ in 0..50 {
        let (src, dst) = (rng.gen_range(1..60), rng.gen_range(1..60));
        let etype = rng.gen_range(1..3);
        match rng.gen_range(0..10) {
          0..=4 => pending.add_edge(src, etype, dst),
          5..=6 => pending.delete_edge(src, etype, dst),
          7 => pending.delete_edge_over(src, etype, dst, rng.gen_bool(0.5)),
          8 => pending.create_node(src, None),
          _ => pending.delete_node(src),
        }
        count(&pending);
      }
      delta.merge_from(&mut pending);
      count(&delta);
      count(&pending);
      if round % 10 == 9 {
        delta.clear();
        count(&delta);
      }
    }
  }

  /// Ids too spread out for a dense part stay in the map.
  #[test]
  fn spread_out_created_nodes_stay_in_the_map() {
    let mut nodes = CreatedNodes::default();
    for i in 0..4096u64 {
      nodes.insert(i * 1000, NodeDelta::default());
    }
    assert!(nodes.dense.is_none());
    assert_eq!(nodes.len(), 4096);
    assert!(nodes.contains_key(&4_095_000));
  }
}
