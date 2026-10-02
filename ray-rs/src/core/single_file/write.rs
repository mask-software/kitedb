//! Write operations for SingleFileDB
//!
//! Handles all mutation operations: create/delete nodes, add/delete edges,
//! set/delete properties, and node labels.

use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::record::{
  build_add_edge_payload, build_add_edge_props_payload, build_add_edges_batch_payload,
  build_add_edges_props_batch_payload, build_add_node_label_payload, build_create_node_payload,
  build_create_nodes_batch_payload, build_define_etype_payload, build_define_label_payload,
  build_define_propkey_payload, build_del_edge_prop_payload, build_del_node_prop_payload,
  build_delete_edge_payload, build_delete_node_payload, build_remove_node_label_payload,
  build_set_edge_prop_payload, build_set_edge_props_payload, build_set_node_prop_payload,
  WalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{SingleFileDB, SingleFileTxState, MAX_NODE_ID};

/// What a write transaction sees: its pending delta over the committed delta
/// over the snapshot. Write paths validate against it before logging to the
/// WAL. It records no MVCC reads.
struct TxView<'a> {
  pending: &'a DeltaState,
  delta: &'a DeltaState,
  snapshot: Option<&'a SnapshotData>,
}

/// Where a live node lives, as seen by a write transaction.
enum Endpoint {
  /// Created by this transaction.
  Pending,
  /// Created in the committed delta.
  Delta,
  /// In the snapshot, at this physical index.
  Snapshot(PhysNode),
}

impl TxView<'_> {
  /// Resolve an edge endpoint with `node_exists` precedence: a layer's own
  /// copy of a node (created or recreated there) wins over its delete, which
  /// masks the copies below.
  fn endpoint(&self, node_id: NodeId) -> Result<Endpoint> {
    let missing = Err(KiteError::NodeNotFound(node_id));
    if self.pending.is_node_created(node_id) {
      return Ok(Endpoint::Pending);
    }
    if self.pending.is_node_deleted(node_id) {
      return missing;
    }
    if self.delta.is_node_created(node_id) {
      return Ok(Endpoint::Delta);
    }
    if self.delta.is_node_deleted(node_id) {
      return missing;
    }
    match self.snapshot.and_then(|snap| snap.phys_node(node_id)) {
      Some(phys) => Ok(Endpoint::Snapshot(phys)),
      None => missing,
    }
  }

  fn node_exists(&self, node_id: NodeId) -> bool {
    self.endpoint(node_id).is_ok()
  }

  /// Like `edge`, but both endpoints must exist (`NodeNotFound` otherwise).
  fn edge_to_add(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<(bool, bool)> {
    let in_base = match (self.endpoint(src)?, self.endpoint(dst)?) {
      // A node created by this transaction has no committed edges.
      (Endpoint::Pending, _) | (_, Endpoint::Pending) => false,
      (Endpoint::Snapshot(src_phys), Endpoint::Snapshot(dst_phys)) => {
        self.delta.is_edge_added(src, etype, dst)
          || (!self.delta.is_edge_deleted(src, etype, dst)
            && self
              .snapshot
              .is_some_and(|snap| snap.has_edge(src_phys, etype, dst_phys)))
      }
      _ => self.delta.is_edge_added(src, etype, dst),
    };
    Ok((in_base, self.pending.edge_visible(src, etype, dst, in_base)))
  }

  /// `edge_to_add`, plus the props an add would bring back
  /// (`revealed_edge_props`), except those in `keep` (which the add sets).
  fn edge_to_add_revealing(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    keep: &[PropKeyId],
  ) -> Result<(bool, bool, Vec<PropKeyId>)> {
    let (in_base, exists) = self.edge_to_add(src, etype, dst)?;
    let revealed = if exists {
      Vec::new()
    } else {
      let mut keys = self.revealed_edge_props(src, etype, dst, in_base);
      keys.retain(|key_id| !keep.contains(key_id));
      keys
    };
    Ok((in_base, exists, revealed))
  }

  /// Props of the deleted copy of an edge that adding it brings back. An add
  /// over a tombstone cancels the tombstone, which exposes the copy below it:
  /// the committed edge this transaction deleted (`in_base`), or the snapshot
  /// edge the committed delta deleted. The re-added edge is a new edge, so the
  /// caller masks these. Sorted, so the WAL is deterministic.
  fn revealed_edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    in_base: bool,
  ) -> Vec<PropKeyId> {
    let snapshot_copy = !self.delta.is_edge_added(src, etype, dst)
      && self
        .delta
        .snapshot_edge_over(self.snapshot, src, etype, dst);
    let revealed = in_base || (snapshot_copy && self.delta.is_edge_deleted(src, etype, dst));
    if !revealed {
      return Vec::new();
    }
    let mut props: HashMap<PropKeyId, bool> = HashMap::new();
    if snapshot_copy {
      if let Some(snapshot_props) = self.snapshot.and_then(|snap| {
        let (src_phys, dst_phys) = (snap.phys_node(src)?, snap.phys_node(dst)?);
        snap.edge_props(snap.find_edge_index(src_phys, etype, dst_phys)?)
      }) {
        props.extend(snapshot_props.into_keys().map(|key_id| (key_id, true)));
      }
    }
    if let Some(delta_props) = self.delta.edge_props_delta(src, etype, dst) {
      for (&key_id, value) in delta_props {
        props.insert(key_id, value.is_some());
      }
    }
    let mut keys: Vec<PropKeyId> = props
      .into_iter()
      .filter_map(|(key_id, set)| set.then_some(key_id))
      .collect();
    keys.sort_unstable();
    keys
  }

  /// Whether the committed state holds the edge (the pending delta's base),
  /// and whether the transaction sees it. A node this transaction deleted or
  /// recreated masks its committed edges.
  fn edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> (bool, bool) {
    let masked = self.pending.is_node_deleted(src) || self.pending.is_node_deleted(dst);
    let in_base = !masked && self.delta.edge_exists_over(self.snapshot, src, etype, dst);
    (in_base, self.pending.edge_visible(src, etype, dst, in_base))
  }

  fn check_key_free(&self, key: &str) -> Result<()> {
    let owner = self.pending.key_owner_over(None, key).or_else(|| {
      self
        .delta
        .key_owner_over(self.snapshot, key)
        .filter(|&node_id| !self.pending.is_node_deleted(node_id))
    });
    match owner {
      Some(_) => Err(KiteError::DuplicateKey(key.to_string())),
      None => Ok(()),
    }
  }
}

/// The MVCC keys an edge add or delete writes: the edge, and its endpoints'
/// neighbor lists, with and without its type.
fn edge_write_keys(src: NodeId, etype: ETypeId, dst: NodeId) -> [TxKey; 5] {
  [
    TxKey::Edge { src, etype, dst },
    TxKey::NeighborsOut {
      node_id: src,
      etype: None,
    },
    TxKey::NeighborsIn {
      node_id: dst,
      etype: None,
    },
    TxKey::NeighborsOut {
      node_id: src,
      etype: Some(etype),
    },
    TxKey::NeighborsIn {
      node_id: dst,
      etype: Some(etype),
    },
  ]
}

/// What an edge prop write reads: the edge and its endpoints. It conflicts
/// with a concurrent delete_edge (which writes `Edge`) or delete_node of an
/// endpoint (which writes `Node`), but not with writes to the edge's other
/// props.
fn edge_prop_dependencies(src: NodeId, etype: ETypeId, dst: NodeId) -> [TxKey; 3] {
  [
    TxKey::Edge { src, etype, dst },
    TxKey::Node(src),
    TxKey::Node(dst),
  ]
}

impl SingleFileDB {
  fn with_tx_view<R>(
    &self,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    f: impl FnOnce(&TxView<'_>) -> R,
  ) -> R {
    let tx = tx_handle.lock();
    let delta = self.delta.read();
    let snapshot = self.snapshot.read();
    f(&TxView {
      pending: &tx.pending,
      delta: &delta,
      snapshot: snapshot.as_ref(),
    })
  }

  /// A write that a check turned into a no-op or rejected still depends on
  /// the state it checked: note the read for the MVCC conflict check of the
  /// calling thread's transaction `txid`.
  pub(super) fn record_read(&self, txid: TxId, key: TxKey) {
    if self.mvcc.is_none() {
      return;
    }
    if let Some(handle) = self.current_tx_handle() {
      let mut tx = handle.lock();
      if tx.txid == txid {
        tx.record_read(key);
      }
    }
  }

  /// Note what the write transaction `tx` wrote (`writes`) and read
  /// (`reads`) for its MVCC conflict check at commit. The keys stay with the
  /// transaction until then (see `SingleFileTxState::mvcc_writes`). A bulk
  /// load, which runs without MVCC, records nothing.
  fn record_tx_keys(
    &self,
    tx: &mut SingleFileTxState,
    writes: impl IntoIterator<Item = TxKey>,
    reads: impl IntoIterator<Item = TxKey>,
  ) {
    if self.mvcc.is_none() || tx.bulk_load {
      return;
    }
    tx.mvcc_writes.extend(writes);
    for key in reads {
      tx.record_read(key);
    }
  }

  /// Fail with `NodeNotFound` unless the transaction sees `node_id`: a prop or
  /// label write to a missing node would otherwise linger in the delta. A
  /// rejected write still depends on the node's absence.
  pub(super) fn require_node(
    &self,
    txid: TxId,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    node_id: NodeId,
  ) -> Result<()> {
    // Bulk loads write to nodes they just created: skip the committed state.
    if tx_handle.lock().pending.is_node_created(node_id) {
      return Ok(());
    }
    if self.with_tx_view(tx_handle, |view| view.node_exists(node_id)) {
      return Ok(());
    }
    self.record_read(txid, TxKey::Node(node_id));
    Err(KiteError::NodeNotFound(node_id))
  }

  /// Fail with `EdgeNotFound` unless the transaction sees the edge.
  fn require_edge(
    &self,
    txid: TxId,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  ) -> Result<()> {
    let (_, visible) = self.with_tx_view(tx_handle, |view| view.edge(src, etype, dst));
    if visible {
      return Ok(());
    }
    self.record_read(txid, TxKey::Edge { src, etype, dst });
    Err(KiteError::EdgeNotFound { src, etype, dst })
  }

  /// Delete the props a re-add brought back (`TxView::revealed_edge_props`),
  /// one `DelEdgeProp` record each, so the re-added edge starts without them
  /// here, after WAL replay, and on replicas. Call after the add is logged
  /// and applied.
  fn mask_revealed_edge_props(
    &self,
    txid: TxId,
    tx_handle: &Arc<Mutex<SingleFileTxState>>,
    revealed: Vec<((NodeId, ETypeId, NodeId), Vec<PropKeyId>)>,
  ) -> Result<()> {
    for ((src, etype, dst), key_ids) in revealed {
      for key_id in key_ids {
        let record = WalRecord::new(
          WalRecordType::DelEdgeProp,
          txid,
          build_del_edge_prop_payload(src, etype, dst, key_id),
        );
        self.write_wal_tx(tx_handle, record)?;
        let mut tx = tx_handle.lock();
        tx.pending.delete_edge_prop(src, etype, dst, key_id);
        let written = TxKey::EdgeProp {
          src,
          etype,
          dst,
          key_id,
        };
        self.record_tx_keys(&mut tx, [written], []);
      }
    }
    Ok(())
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a node
  ///
  /// Fails with `DuplicateKey` if a live node already holds `key`.
  pub fn create_node(&self, key: Option<&str>) -> Result<NodeId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    if let Some(key) = key {
      self.with_tx_view(&tx_handle, |view| view.check_key_free(key))?;
    }
    let node_id = self.alloc_node_id()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      txid,
      build_create_node_payload(node_id, key),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.create_node(node_id, key);
    let key_written = key.map(|key| TxKey::Key(key.into()));
    self.record_tx_keys(
      &mut tx,
      [TxKey::Node(node_id)].into_iter().chain(key_written),
      [],
    );

    Ok(node_id)
  }

  /// Create a node with a specific ID (at most [`MAX_NODE_ID`])
  pub fn create_node_with_id(&self, node_id: NodeId, key: Option<&str>) -> Result<NodeId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    if node_id > MAX_NODE_ID {
      return Err(KiteError::InvalidQuery(
        format!("Node ID {node_id} exceeds the maximum node ID {MAX_NODE_ID}").into(),
      ));
    }

    if self.with_tx_view(&tx_handle, |view| view.node_exists(node_id)) {
      self.record_read(txid, TxKey::Node(node_id));
      return Err(KiteError::Internal(format!(
        "Node ID already exists: {node_id}"
      )));
    }

    if let Some(key) = key {
      self.with_tx_view(&tx_handle, |view| view.check_key_free(key))?;
    }

    self.reserve_node_id(node_id);

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      txid,
      build_create_node_payload(node_id, key),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.create_node(node_id, key);
    let key_written = key.map(|key| TxKey::Key(key.into()));
    self.record_tx_keys(
      &mut tx,
      [TxKey::Node(node_id)].into_iter().chain(key_written),
      [],
    );

    Ok(node_id)
  }

  /// Create multiple nodes in a single WAL record
  pub fn create_nodes_batch(&self, keys: &[Option<&str>]) -> Result<Vec<NodeId>> {
    if keys.is_empty() {
      return Ok(Vec::new());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    if keys.iter().any(Option::is_some) {
      self.with_tx_view(&tx_handle, |view| {
        let mut batch_keys = HashSet::with_capacity(keys.len());
        for &key in keys.iter().flatten() {
          view.check_key_free(key)?;
          if !batch_keys.insert(key) {
            return Err(KiteError::DuplicateKey(key.to_string()));
          }
        }
        Ok(())
      })?;
    }
    let mut node_ids = Vec::with_capacity(keys.len());
    for _ in keys.iter() {
      node_ids.push(self.alloc_node_id()?);
    }

    let entries: Vec<(NodeId, Option<&str>)> =
      node_ids.iter().copied().zip(keys.iter().copied()).collect();

    let record = WalRecord::new(
      WalRecordType::CreateNodesBatch,
      txid,
      build_create_nodes_batch_payload(&entries),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let mut tx = tx_handle.lock();
    for (node_id, key) in entries.iter() {
      tx.pending.create_node(*node_id, *key);
    }
    let written = entries.iter().flat_map(|&(node_id, key)| {
      [TxKey::Node(node_id)]
        .into_iter()
        .chain(key.map(|key| TxKey::Key(key.into())))
    });
    self.record_tx_keys(&mut tx, written, []);

    Ok(node_ids)
  }

  /// Delete a node
  pub fn delete_node(&self, node_id: NodeId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let mut key_to_record = None;
    let bulk_load = {
      let tx = tx_handle.lock();
      if !tx.bulk_load {
        if let Some(node_delta) = tx.pending.created_nodes.get(&node_id) {
          key_to_record = node_delta.key.clone();
        }
      }
      tx.bulk_load
    };
    if !bulk_load && key_to_record.is_none() {
      let delta = self.delta.read();
      if let Some(node_delta) = delta.created_nodes.get(&node_id) {
        key_to_record = node_delta.key.clone();
      } else if let Some(ref snap) = *self.snapshot.read() {
        if let Some(phys) = snap.phys_node(node_id) {
          key_to_record = snap.node_key(phys);
        }
      }
    }

    // A deleted node keeps no vectors, now or after the next checkpoint
    // (also a missing node's leftovers from older versions: unchecked).
    for prop_key_id in self.node_vector_keys(&tx_handle, node_id)? {
      self.log_delete_node_vector(txid, &tx_handle, node_id, prop_key_id)?;
    }

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DeleteNode,
      txid,
      build_delete_node_payload(node_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.delete_node(node_id);
    let written = [
      TxKey::Node(node_id),
      TxKey::NeighborsOut {
        node_id,
        etype: None,
      },
      TxKey::NeighborsIn {
        node_id,
        etype: None,
      },
      TxKey::NodeLabels(node_id),
    ];
    let key_written = key_to_record.map(|key| TxKey::Key(key.as_str().into()));
    self.record_tx_keys(&mut tx, written.into_iter().chain(key_written), []);

    Ok(())
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  ///
  /// Both endpoints must exist (`NodeNotFound` otherwise). Adding an edge
  /// that already exists is a no-op.
  pub fn add_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    self.add_missing_edge(src, etype, dst).map(|_| ())
  }

  /// `add_edge`, returning whether the edge was added (false if it existed).
  fn add_missing_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<bool> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let (in_base, exists, revealed) = self.with_tx_view(&tx_handle, |view| {
      view.edge_to_add_revealing(src, etype, dst, &[])
    })?;
    if exists {
      self.record_read(txid, TxKey::Edge { src, etype, dst });
      return Ok(false);
    }

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::AddEdge,
      txid,
      build_add_edge_payload(src, etype, dst),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    {
      let mut tx = tx_handle.lock();
      tx.pending.add_edge_over(src, etype, dst, in_base);
      self.record_tx_keys(&mut tx, edge_write_keys(src, etype, dst), []);
    }
    self.mask_revealed_edge_props(txid, &tx_handle, vec![((src, etype, dst), revealed)])?;

    Ok(true)
  }

  /// Add multiple edges in a single WAL record
  ///
  /// Fails without adding anything if an endpoint is missing. Edges that
  /// already exist are skipped.
  pub fn add_edges_batch(&self, edges: &[(NodeId, ETypeId, NodeId)]) -> Result<()> {
    if edges.is_empty() {
      return Ok(());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let mut existing = Vec::new();
    let mut new_edges = Vec::with_capacity(edges.len());
    let mut new_in_base = Vec::with_capacity(edges.len());
    let mut revealed = Vec::new();
    self.with_tx_view(&tx_handle, |view| {
      for &(src, etype, dst) in edges {
        let (in_base, exists, keys) = view.edge_to_add_revealing(src, etype, dst, &[])?;
        if exists {
          existing.push((src, etype, dst));
        } else {
          new_edges.push((src, etype, dst));
          new_in_base.push(in_base);
          if !keys.is_empty() {
            revealed.push(((src, etype, dst), keys));
          }
        }
      }
      Ok::<_, KiteError>(())
    })?;
    for &(src, etype, dst) in &existing {
      self.record_read(txid, TxKey::Edge { src, etype, dst });
    }
    if new_edges.is_empty() {
      return Ok(());
    }
    let edges = new_edges.as_slice();

    let record = WalRecord::new(
      WalRecordType::AddEdgesBatch,
      txid,
      build_add_edges_batch_payload(edges),
    );
    self.write_wal_tx(&tx_handle, record)?;

    {
      let mut tx = tx_handle.lock();
      for (&(src, etype, dst), &in_base) in edges.iter().zip(&new_in_base) {
        tx.pending.add_edge_over(src, etype, dst, in_base);
      }
      let written = edges
        .iter()
        .flat_map(|&(src, etype, dst)| edge_write_keys(src, etype, dst));
      self.record_tx_keys(&mut tx, written, []);
    }
    self.mask_revealed_edge_props(txid, &tx_handle, revealed)?;

    Ok(())
  }

  /// Add an edge with properties in a single WAL record
  pub fn add_edge_with_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    props: Vec<(PropKeyId, PropValue)>,
  ) -> Result<()> {
    if props.is_empty() {
      return self.add_edge(src, etype, dst);
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let set_keys: Vec<PropKeyId> = props.iter().map(|(key_id, _)| *key_id).collect();
    let (in_base, _, revealed) = self.with_tx_view(&tx_handle, |view| {
      view.edge_to_add_revealing(src, etype, dst, &set_keys)
    })?;

    let record = WalRecord::new(
      WalRecordType::AddEdgeProps,
      txid,
      build_add_edge_props_payload(src, etype, dst, &props),
    );
    self.write_wal_tx(&tx_handle, record)?;

    {
      let mut tx = tx_handle.lock();
      tx.pending.add_edge_over(src, etype, dst, in_base);
      for (key_id, value) in props.into_iter() {
        tx.pending.set_edge_prop(src, etype, dst, key_id, value);
      }
      let props_written = set_keys.iter().map(|&key_id| TxKey::EdgeProp {
        src,
        etype,
        dst,
        key_id,
      });
      self.record_tx_keys(
        &mut tx,
        edge_write_keys(src, etype, dst)
          .into_iter()
          .chain(props_written),
        [],
      );
    }
    self.mask_revealed_edge_props(txid, &tx_handle, vec![((src, etype, dst), revealed)])?;

    Ok(())
  }

  /// Add multiple edges with properties in a single WAL record
  pub fn add_edges_with_props_batch(&self, edges: Vec<EdgeWithProps>) -> Result<()> {
    if edges.is_empty() {
      return Ok(());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let (in_base, revealed): (Vec<bool>, Vec<_>) = self
      .with_tx_view(&tx_handle, |view| {
        edges
          .iter()
          .map(|(src, etype, dst, props)| {
            let set_keys: Vec<PropKeyId> = props.iter().map(|(key_id, _)| *key_id).collect();
            let (in_base, _, keys) = view.edge_to_add_revealing(*src, *etype, *dst, &set_keys)?;
            Ok((in_base, ((*src, *etype, *dst), keys)))
          })
          .collect::<Result<Vec<_>>>()
      })?
      .into_iter()
      .unzip();
    let record = WalRecord::new(
      WalRecordType::AddEdgesPropsBatch,
      txid,
      build_add_edges_props_batch_payload(&edges),
    );
    self.write_wal_tx(&tx_handle, record)?;

    {
      let mut tx = tx_handle.lock();
      let mut written = Vec::new();
      for ((src, etype, dst, props), in_base) in edges.into_iter().zip(in_base) {
        tx.pending.add_edge_over(src, etype, dst, in_base);
        written.extend(edge_write_keys(src, etype, dst));
        for (key_id, value) in props {
          tx.pending.set_edge_prop(src, etype, dst, key_id, value);
          written.push(TxKey::EdgeProp {
            src,
            etype,
            dst,
            key_id,
          });
        }
      }
      self.record_tx_keys(&mut tx, written, []);
    }
    self.mask_revealed_edge_props(txid, &tx_handle, revealed)?;

    Ok(())
  }

  /// Add an edge by type name
  pub fn add_edge_by_name(&self, src: NodeId, etype_name: &str, dst: NodeId) -> Result<()> {
    let etype = self.define_etype(etype_name)?;
    self.add_edge(src, etype, dst)
  }

  /// Delete an edge (a no-op if it does not exist)
  pub fn delete_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let (in_base, exists) = self.with_tx_view(&tx_handle, |view| view.edge(src, etype, dst));
    if !exists {
      self.record_read(txid, TxKey::Edge { src, etype, dst });
      return Ok(());
    }

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DeleteEdge,
      txid,
      build_delete_edge_payload(src, etype, dst),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.delete_edge_over(src, etype, dst, in_base);
    self.record_tx_keys(&mut tx, edge_write_keys(src, etype, dst), []);

    Ok(())
  }

  /// Upsert an edge (create if missing, otherwise update props)
  ///
  /// Both endpoints must exist (`NodeNotFound` otherwise). Returns a flag
  /// indicating whether the edge was created.
  pub fn upsert_edge_with_props<I>(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    props: I,
  ) -> Result<bool>
  where
    I: IntoIterator<Item = (PropKeyId, Option<PropValue>)>,
  {
    let created = self.add_missing_edge(src, etype, dst)?;

    for (key_id, value_opt) in props {
      match value_opt {
        Some(value) => self.set_edge_prop(src, etype, dst, key_id, value)?,
        None => self.delete_edge_prop(src, etype, dst, key_id)?,
      }
    }

    Ok(created)
  }

  // ========================================================================
  // Node Property Operations
  // ========================================================================

  /// Set a node property
  pub fn set_node_prop(&self, node_id: NodeId, key_id: PropKeyId, value: PropValue) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetNodeProp,
      txid,
      build_set_node_prop_payload(node_id, key_id, &value),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.set_node_prop(node_id, key_id, value);
    // The write needs the node: it conflicts with a concurrent delete_node,
    // not with writes to the node's other props.
    self.record_tx_keys(
      &mut tx,
      [TxKey::NodeProp { node_id, key_id }],
      [TxKey::Node(node_id)],
    );

    Ok(())
  }

  /// Set a node property by key name
  pub fn set_node_prop_by_name(
    &self,
    node_id: NodeId,
    key_name: &str,
    value: PropValue,
  ) -> Result<()> {
    let key_id = self.define_propkey(key_name)?;
    self.set_node_prop(node_id, key_id, value)
  }

  /// Delete a node property
  pub fn delete_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelNodeProp,
      txid,
      build_del_node_prop_payload(node_id, key_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.delete_node_prop(node_id, key_id);
    // The write needs the node: it conflicts with a concurrent delete_node,
    // not with writes to the node's other props.
    self.record_tx_keys(
      &mut tx,
      [TxKey::NodeProp { node_id, key_id }],
      [TxKey::Node(node_id)],
    );

    Ok(())
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  pub fn set_edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
    value: PropValue,
  ) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_edge(txid, &tx_handle, src, etype, dst)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetEdgeProp,
      txid,
      build_set_edge_prop_payload(src, etype, dst, key_id, &value),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.set_edge_prop(src, etype, dst, key_id, value);
    let written = TxKey::EdgeProp {
      src,
      etype,
      dst,
      key_id,
    };
    self.record_tx_keys(&mut tx, [written], edge_prop_dependencies(src, etype, dst));

    Ok(())
  }

  /// Set multiple edge properties in a single WAL record
  pub fn set_edge_props(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    props: Vec<(PropKeyId, PropValue)>,
  ) -> Result<()> {
    if props.is_empty() {
      return Ok(());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_edge(txid, &tx_handle, src, etype, dst)?;

    let key_ids: Vec<PropKeyId> = props.iter().map(|(key_id, _)| *key_id).collect();

    let record = WalRecord::new(
      WalRecordType::SetEdgeProps,
      txid,
      build_set_edge_props_payload(src, etype, dst, &props),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let mut tx = tx_handle.lock();
    for (key_id, value) in props.into_iter() {
      tx.pending.set_edge_prop(src, etype, dst, key_id, value);
    }
    let written = key_ids.into_iter().map(|key_id| TxKey::EdgeProp {
      src,
      etype,
      dst,
      key_id,
    });
    self.record_tx_keys(&mut tx, written, edge_prop_dependencies(src, etype, dst));

    Ok(())
  }

  /// Set an edge property by key name
  pub fn set_edge_prop_by_name(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_name: &str,
    value: PropValue,
  ) -> Result<()> {
    let key_id = self.define_propkey(key_name)?;
    self.set_edge_prop(src, etype, dst, key_id, value)
  }

  /// Delete an edge property
  pub fn delete_edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    key_id: PropKeyId,
  ) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_edge(txid, &tx_handle, src, etype, dst)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelEdgeProp,
      txid,
      build_del_edge_prop_payload(src, etype, dst, key_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.delete_edge_prop(src, etype, dst, key_id);
    let written = TxKey::EdgeProp {
      src,
      etype,
      dst,
      key_id,
    };
    self.record_tx_keys(&mut tx, [written], edge_prop_dependencies(src, etype, dst));

    Ok(())
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Add a label to a node
  pub fn add_node_label(&self, node_id: NodeId, label_id: LabelId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::AddNodeLabel,
      txid,
      build_add_node_label_payload(node_id, label_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.add_node_label(node_id, label_id);
    // The write needs the node (a concurrent delete_node conflicts), but does
    // not change whether it exists.
    self.record_tx_keys(
      &mut tx,
      [
        TxKey::NodeLabels(node_id),
        TxKey::NodeLabel { node_id, label_id },
      ],
      [TxKey::Node(node_id)],
    );

    Ok(())
  }

  /// Add a label to a node by name
  pub fn add_node_label_by_name(&self, node_id: NodeId, label_name: &str) -> Result<()> {
    let label_id = self.define_label(label_name)?;
    self.add_node_label(node_id, label_id)
  }

  /// Remove a label from a node
  pub fn remove_node_label(&self, node_id: NodeId, label_id: LabelId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    self.require_node(txid, &tx_handle, node_id)?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::RemoveNodeLabel,
      txid,
      build_remove_node_label_payload(node_id, label_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let mut tx = tx_handle.lock();
    tx.pending.remove_node_label(node_id, label_id);
    // The write needs the node (a concurrent delete_node conflicts), but does
    // not change whether it exists.
    self.record_tx_keys(
      &mut tx,
      [
        TxKey::NodeLabels(node_id),
        TxKey::NodeLabel { node_id, label_id },
      ],
      [TxKey::Node(node_id)],
    );

    Ok(())
  }

  /// Remove a label from a node by name
  pub fn remove_node_label_by_name(&self, node_id: NodeId, label_name: &str) -> Result<()> {
    if let Some(label_id) = self.label_id(label_name) {
      self.remove_node_label(node_id, label_id)
    } else {
      Ok(()) // Label doesn't exist, nothing to remove
    }
  }

  // ========================================================================
  // Schema Definition Operations
  // ========================================================================

  /// Define a new label (writes to WAL for durability)
  pub fn define_label(&self, name: &str) -> Result<LabelId> {
    // Check if already exists
    if let Some(id) = self.label_id(name) {
      return Ok(id);
    }

    let (txid, tx_handle) = self.require_schema_tx_handle()?;

    // A concurrent writer may have defined it while this transaction was starting.
    if let Some(id) = self.label_id(name) {
      return Ok(id);
    }

    let label_id = self.claim_label_reservation(name, txid);

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineLabel,
      txid,
      build_define_label_payload(label_id, name),
    );
    if let Err(error) = self.write_wal_tx(&tx_handle, record) {
      self.release_label_reservation(name, txid);
      return Err(error);
    }

    // Stage both the transaction-local lookup and the transaction delta. The
    // global maps are published by commit after the durable WAL boundary.
    {
      let mut tx = tx_handle.lock();
      tx.schema.define_label(label_id, name);
      tx.pending.define_label(label_id, name);
    }

    Ok(label_id)
  }

  /// Define a new edge type (writes to WAL for durability)
  pub fn define_etype(&self, name: &str) -> Result<ETypeId> {
    // Check if already exists
    if let Some(id) = self.etype_id(name) {
      return Ok(id);
    }

    let (txid, tx_handle) = self.require_schema_tx_handle()?;

    // A concurrent writer may have defined it while this transaction was starting.
    if let Some(id) = self.etype_id(name) {
      return Ok(id);
    }

    let etype_id = self.claim_etype_reservation(name, txid);

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineEtype,
      txid,
      build_define_etype_payload(etype_id, name),
    );
    if let Err(error) = self.write_wal_tx(&tx_handle, record) {
      self.release_etype_reservation(name, txid);
      return Err(error);
    }

    // Stage both the transaction-local lookup and the transaction delta. The
    // global maps are published by commit after the durable WAL boundary.
    {
      let mut tx = tx_handle.lock();
      tx.schema.define_etype(etype_id, name);
      tx.pending.define_etype(etype_id, name);
    }

    Ok(etype_id)
  }

  /// Define a new property key (writes to WAL for durability)
  pub fn define_propkey(&self, name: &str) -> Result<PropKeyId> {
    // Check if already exists
    if let Some(id) = self.propkey_id(name) {
      return Ok(id);
    }

    let (txid, tx_handle) = self.require_schema_tx_handle()?;

    // A concurrent writer may have defined it while this transaction was starting.
    if let Some(id) = self.propkey_id(name) {
      return Ok(id);
    }

    let propkey_id = self.claim_propkey_reservation(name, txid);

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefinePropkey,
      txid,
      build_define_propkey_payload(propkey_id, name),
    );
    if let Err(error) = self.write_wal_tx(&tx_handle, record) {
      self.release_propkey_reservation(name, txid);
      return Err(error);
    }

    // Stage both the transaction-local lookup and the transaction delta. The
    // global maps are published by commit after the durable WAL boundary.
    {
      let mut tx = tx_handle.lock();
      tx.schema.define_propkey(propkey_id, name);
      tx.pending.define_propkey(propkey_id, name);
    }

    Ok(propkey_id)
  }
}

#[cfg(test)]
mod tests {
  // No checkpoints here: checkpoint unit tests arm process-wide phase
  // barriers that a concurrent checkpoint could consume.
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::error::KiteError;
  use tempfile::tempdir;

  #[test]
  fn add_after_deleting_missing_edge_keeps_edge() {
    let dir = tempdir().expect("tempdir");
    let db =
      open_single_file(dir.path().join("db.kitedb"), SingleFileOpenOptions::new()).expect("open");
    db.begin(false).expect("begin");
    let a = db.create_node(None).expect("a");
    let b = db.create_node(None).expect("b");
    let t = db.define_etype("T").expect("etype");
    db.commit().expect("commit");

    db.begin(false).expect("begin");
    db.delete_edge(a, t, b).expect("delete missing edge");
    db.add_edge(a, t, b).expect("add");
    assert!(db.edge_exists(a, t, b));
    db.commit().expect("commit");
    assert!(db.edge_exists(a, t, b));
    assert_eq!(db.count_edges(), 1);
    close_single_file(db).expect("close");
  }

  #[test]
  fn bulk_load_batches_validate_and_skip_existing() {
    let dir = tempdir().expect("tempdir");
    let db =
      open_single_file(dir.path().join("db.kitedb"), SingleFileOpenOptions::new()).expect("open");
    db.begin_bulk().expect("begin bulk");
    let ids = db
      .create_nodes_batch(&[Some("a"), Some("b")])
      .expect("nodes");
    let (a, b) = (ids[0], ids[1]);
    let t = db.define_etype("T").expect("etype");
    assert!(matches!(
      db.create_nodes_batch(&[Some("c"), Some("a")]),
      Err(KiteError::DuplicateKey(_))
    ));
    assert!(matches!(
      db.add_edges_batch(&[(a, t, b), (a, t, 999_999)]),
      Err(KiteError::NodeNotFound(999_999))
    ));
    db.add_edges_batch(&[(a, t, b), (a, t, b), (b, t, a)])
      .expect("edges");
    db.add_edges_batch(&[(a, t, b)]).expect("re-add");
    db.commit().expect("commit");
    assert_eq!(db.count_nodes(), 2);
    assert_eq!(db.count_edges(), 2);
    close_single_file(db).expect("close");
  }

  #[test]
  fn delete_node_drops_vector_set_in_same_tx() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("db.kitedb");
    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("open");
    db.begin(false).expect("begin");
    let keep = db.create_node(Some("keep")).expect("keep");
    let pk = db.define_propkey("embedding").expect("propkey");
    db.set_node_vector(keep, pk, &[1.0, 0.0]).expect("vector");
    let gone = db.create_node(Some("gone")).expect("gone");
    db.set_node_vector(gone, pk, &[0.0, 1.0]).expect("vector");
    db.delete_node(gone).expect("delete");
    assert!(db.node_vector(gone, pk).is_none());
    db.commit().expect("commit");

    assert!(db.node_vector(gone, pk).is_none());
    assert!(db.has_node_vector(keep, pk));
    close_single_file(db).expect("close");
    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen");
    assert!(db.node_vector(gone, pk).is_none());
    assert!(db.has_node_vector(keep, pk));
    close_single_file(db).expect("close");
  }
}
