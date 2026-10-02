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
use crate::mvcc::TxManager;
use crate::types::*;
use parking_lot::Mutex;
use std::collections::HashSet;
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
  /// Resolve an edge endpoint with `node_exists` precedence.
  fn endpoint(&self, node_id: NodeId) -> Result<Endpoint> {
    let missing = Err(KiteError::NodeNotFound(node_id));
    if self.pending.is_node_deleted(node_id) {
      return missing;
    }
    if self.pending.is_node_created(node_id) {
      return Ok(Endpoint::Pending);
    }
    if self.delta.is_node_deleted(node_id) {
      return missing;
    }
    if self.delta.is_node_created(node_id) {
      return Ok(Endpoint::Delta);
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

  /// Whether the committed state holds the edge (the pending delta's base),
  /// and whether the transaction sees it.
  fn edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> (bool, bool) {
    let in_base = self.delta.edge_exists_over(self.snapshot, src, etype, dst);
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

/// An edge prop write needs the edge and its endpoints: it conflicts with a concurrent
/// delete_edge (which writes `Edge`) or delete_node of an endpoint (which writes `Node`), but
/// not with writes to the edge's other props.
fn record_edge_prop_dependencies(
  tx_mgr: &mut TxManager,
  txid: TxId,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) {
  tx_mgr.record_read(txid, TxKey::Edge { src, etype, dst });
  tx_mgr.record_read(txid, TxKey::Node(src));
  tx_mgr.record_read(txid, TxKey::Node(dst));
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
  /// the state it checked.
  fn record_read(&self, txid: TxId, key: TxKey) {
    if let Some(mvcc) = self.mvcc.as_ref() {
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_read(txid, key);
    }
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
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.create_node(node_id, key);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(node_id);
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::Node(node_id));
      if let Some(key) = key {
        tx_mgr.record_write(txid, TxKey::Key(key.into()));
      }
    }

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
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.create_node(node_id, key);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(node_id);
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::Node(node_id));
      if let Some(key) = key {
        tx_mgr.record_write(txid, TxKey::Key(key.into()));
      }
    }

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

    let bulk_load = {
      let mut tx = tx_handle.lock();
      for (node_id, key) in entries.iter() {
        tx.pending.create_node(*node_id, *key);
      }
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if !bulk_load {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for (node_id, key) in entries.iter() {
          tx_mgr.record_write(txid, TxKey::Node(*node_id));
          if let Some(key) = key {
            tx_mgr.record_write(txid, TxKey::Key((*key).into()));
          }
        }
      }
    }

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

    // A deleted node keeps no vectors, now or after the next checkpoint.
    for prop_key_id in self.node_vector_keys(&tx_handle, node_id)? {
      self.delete_node_vector(node_id, prop_key_id)?;
    }

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DeleteNode,
      txid,
      build_delete_node_payload(node_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    {
      let mut tx = tx_handle.lock();
      tx.pending.delete_node(node_id);
    }

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::Node(node_id));
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsOut {
          node_id,
          etype: None,
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsIn {
          node_id,
          etype: None,
        },
      );
      tx_mgr.record_write(txid, TxKey::NodeLabels(node_id));
      if let Some(key) = key_to_record.as_ref() {
        tx_mgr.record_write(txid, TxKey::Key(key.as_str().into()));
      }
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_node(node_id);
    }

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
    let (in_base, exists) =
      self.with_tx_view(&tx_handle, |view| view.edge_to_add(src, etype, dst))?;
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
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.add_edge_over(src, etype, dst, in_base);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(true);
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::Edge { src, etype, dst });
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsOut {
          node_id: src,
          etype: None,
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsIn {
          node_id: dst,
          etype: None,
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsOut {
          node_id: src,
          etype: Some(etype),
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsIn {
          node_id: dst,
          etype: Some(etype),
        },
      );
    }

    // Invalidate cache (traversal cache for both src and dst)
    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

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
    self.with_tx_view(&tx_handle, |view| {
      for &(src, etype, dst) in edges {
        let (in_base, exists) = view.edge_to_add(src, etype, dst)?;
        if exists {
          existing.push((src, etype, dst));
        } else {
          new_edges.push((src, etype, dst));
          new_in_base.push(in_base);
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

    let bulk_load = {
      let mut tx = tx_handle.lock();
      for (&(src, etype, dst), &in_base) in edges.iter().zip(&new_in_base) {
        tx.pending.add_edge_over(src, etype, dst, in_base);
      }
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if !bulk_load {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for (src, etype, dst) in edges.iter() {
          tx_mgr.record_write(
            txid,
            TxKey::Edge {
              src: *src,
              etype: *etype,
              dst: *dst,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsOut {
              node_id: *src,
              etype: None,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsIn {
              node_id: *dst,
              etype: None,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsOut {
              node_id: *src,
              etype: Some(*etype),
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsIn {
              node_id: *dst,
              etype: Some(*etype),
            },
          );
        }
      }
    }

    if !bulk_load {
      for (src, etype, dst) in edges.iter() {
        self.cache_invalidate_edge(*src, *etype, *dst);
      }
    }

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
    let (in_base, _) = self.with_tx_view(&tx_handle, |view| view.edge_to_add(src, etype, dst))?;

    let record = WalRecord::new(
      WalRecordType::AddEdgeProps,
      txid,
      build_add_edge_props_payload(src, etype, dst, &props),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let bulk_load = {
      let tx = tx_handle.lock();
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if !bulk_load {
        let mut tx_mgr = mvcc.tx_manager.lock();
        tx_mgr.record_write(txid, TxKey::Edge { src, etype, dst });
        tx_mgr.record_write(
          txid,
          TxKey::NeighborsOut {
            node_id: src,
            etype: None,
          },
        );
        tx_mgr.record_write(
          txid,
          TxKey::NeighborsIn {
            node_id: dst,
            etype: None,
          },
        );
        tx_mgr.record_write(
          txid,
          TxKey::NeighborsOut {
            node_id: src,
            etype: Some(etype),
          },
        );
        tx_mgr.record_write(
          txid,
          TxKey::NeighborsIn {
            node_id: dst,
            etype: Some(etype),
          },
        );
        for (key_id, _) in props.iter() {
          tx_mgr.record_write(
            txid,
            TxKey::EdgeProp {
              src,
              etype,
              dst,
              key_id: *key_id,
            },
          );
        }
      }
    }

    {
      let mut tx = tx_handle.lock();
      tx.pending.add_edge_over(src, etype, dst, in_base);
      for (key_id, value) in props.into_iter() {
        tx.pending.set_edge_prop(src, etype, dst, key_id, value);
      }
    }

    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

    Ok(())
  }

  /// Add multiple edges with properties in a single WAL record
  pub fn add_edges_with_props_batch(&self, edges: Vec<EdgeWithProps>) -> Result<()> {
    if edges.is_empty() {
      return Ok(());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let in_base = self.with_tx_view(&tx_handle, |view| {
      edges
        .iter()
        .map(|&(src, etype, dst, _)| Ok(view.edge_to_add(src, etype, dst)?.0))
        .collect::<Result<Vec<bool>>>()
    })?;
    let mut edge_meta: Vec<(NodeId, ETypeId, NodeId, Vec<PropKeyId>)> =
      Vec::with_capacity(edges.len());
    for (src, etype, dst, props) in edges.iter() {
      let key_ids = props.iter().map(|(key_id, _)| *key_id).collect();
      edge_meta.push((*src, *etype, *dst, key_ids));
    }
    let record = WalRecord::new(
      WalRecordType::AddEdgesPropsBatch,
      txid,
      build_add_edges_props_batch_payload(&edges),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let bulk_load = {
      let mut tx = tx_handle.lock();
      for ((src, etype, dst, props), in_base) in edges.into_iter().zip(in_base) {
        tx.pending.add_edge_over(src, etype, dst, in_base);
        for (key_id, value) in props {
          tx.pending.set_edge_prop(src, etype, dst, key_id, value);
        }
      }
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if !bulk_load {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for (src, etype, dst, key_ids) in edge_meta.iter() {
          tx_mgr.record_write(
            txid,
            TxKey::Edge {
              src: *src,
              etype: *etype,
              dst: *dst,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsOut {
              node_id: *src,
              etype: None,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsIn {
              node_id: *dst,
              etype: None,
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsOut {
              node_id: *src,
              etype: Some(*etype),
            },
          );
          tx_mgr.record_write(
            txid,
            TxKey::NeighborsIn {
              node_id: *dst,
              etype: Some(*etype),
            },
          );
          for key_id in key_ids.iter() {
            tx_mgr.record_write(
              txid,
              TxKey::EdgeProp {
                src: *src,
                etype: *etype,
                dst: *dst,
                key_id: *key_id,
              },
            );
          }
        }
      }
    }

    if !bulk_load {
      for (src, etype, dst, _) in edge_meta.iter() {
        self.cache_invalidate_edge(*src, *etype, *dst);
      }
    }

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
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.delete_edge_over(src, etype, dst, in_base);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::Edge { src, etype, dst });
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsOut {
          node_id: src,
          etype: None,
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsIn {
          node_id: dst,
          etype: None,
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsOut {
          node_id: src,
          etype: Some(etype),
        },
      );
      tx_mgr.record_write(
        txid,
        TxKey::NeighborsIn {
          node_id: dst,
          etype: Some(etype),
        },
      );
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

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

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetNodeProp,
      txid,
      build_set_node_prop_payload(node_id, key_id, &value),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.set_node_prop(node_id, key_id, value);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::NodeProp { node_id, key_id });
      // The write needs the node: it conflicts with a concurrent delete_node, not with
      // writes to the node's other props.
      tx_mgr.record_read(txid, TxKey::Node(node_id));
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_node(node_id);
    }

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

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelNodeProp,
      txid,
      build_del_node_prop_payload(node_id, key_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.delete_node_prop(node_id, key_id);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(txid, TxKey::NodeProp { node_id, key_id });
      // The write needs the node: it conflicts with a concurrent delete_node, not with
      // writes to the node's other props.
      tx_mgr.record_read(txid, TxKey::Node(node_id));
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_node(node_id);
    }

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

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetEdgeProp,
      txid,
      build_set_edge_prop_payload(src, etype, dst, key_id, &value),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.set_edge_prop(src, etype, dst, key_id, value);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(
        txid,
        TxKey::EdgeProp {
          src,
          etype,
          dst,
          key_id,
        },
      );
      record_edge_prop_dependencies(&mut tx_mgr, txid, src, etype, dst);
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

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

    let key_ids: Vec<PropKeyId> = props.iter().map(|(key_id, _)| *key_id).collect();

    let record = WalRecord::new(
      WalRecordType::SetEdgeProps,
      txid,
      build_set_edge_props_payload(src, etype, dst, &props),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let bulk_load = {
      let mut tx = tx_handle.lock();
      for (key_id, value) in props.into_iter() {
        tx.pending.set_edge_prop(src, etype, dst, key_id, value);
      }
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if !bulk_load {
        let mut tx_mgr = mvcc.tx_manager.lock();
        for key_id in key_ids {
          tx_mgr.record_write(
            txid,
            TxKey::EdgeProp {
              src,
              etype,
              dst,
              key_id,
            },
          );
        }
        record_edge_prop_dependencies(&mut tx_mgr, txid, src, etype, dst);
      }
    }

    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

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

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelEdgeProp,
      txid,
      build_del_edge_prop_payload(src, etype, dst, key_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.delete_edge_prop(src, etype, dst, key_id);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      tx_mgr.record_write(
        txid,
        TxKey::EdgeProp {
          src,
          etype,
          dst,
          key_id,
        },
      );
      record_edge_prop_dependencies(&mut tx_mgr, txid, src, etype, dst);
    }

    // Invalidate cache
    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

    Ok(())
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Add a label to a node
  pub fn add_node_label(&self, node_id: NodeId, label_id: LabelId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::AddNodeLabel,
      txid,
      build_add_node_label_payload(node_id, label_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.add_node_label(node_id, label_id);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      // The write needs the node (a concurrent delete_node conflicts), but does not change
      // whether it exists.
      tx_mgr.record_read(txid, TxKey::Node(node_id));
      tx_mgr.record_write(txid, TxKey::NodeLabels(node_id));
      tx_mgr.record_write(txid, TxKey::NodeLabel { node_id, label_id });
    }

    // Invalidate cache (label changes affect node)
    if !bulk_load {
      self.cache_invalidate_node(node_id);
    }

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

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::RemoveNodeLabel,
      txid,
      build_remove_node_label_payload(node_id, label_id),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update pending delta
    let bulk_load = {
      let mut tx = tx_handle.lock();
      tx.pending.remove_node_label(node_id, label_id);
      tx.bulk_load
    };

    if let Some(mvcc) = self.mvcc.as_ref() {
      if bulk_load {
        return Ok(());
      }
      let mut tx_mgr = mvcc.tx_manager.lock();
      // The write needs the node (a concurrent delete_node conflicts), but does not change
      // whether it exists.
      tx_mgr.record_read(txid, TxKey::Node(node_id));
      tx_mgr.record_write(txid, TxKey::NodeLabels(node_id));
      tx_mgr.record_write(txid, TxKey::NodeLabel { node_id, label_id });
    }

    // Invalidate cache (label changes affect node)
    if !bulk_load {
      self.cache_invalidate_node(node_id);
    }

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

    let (txid, tx_handle) = self.require_write_tx_handle()?;

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

    let (txid, tx_handle) = self.require_write_tx_handle()?;

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

    let (txid, tx_handle) = self.require_write_tx_handle()?;

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
