//! Write operations for SingleFileDB
//!
//! Handles all mutation operations: create/delete nodes, add/delete edges,
//! set/delete properties, and node labels.

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

use super::SingleFileDB;

impl SingleFileDB {
  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a node
  pub fn create_node(&self, key: Option<&str>) -> Result<NodeId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let node_id = self.alloc_node_id();

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

  /// Create a node with a specific ID
  pub fn create_node_with_id(&self, node_id: NodeId, key: Option<&str>) -> Result<NodeId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    if self.node_exists(node_id) {
      return Err(KiteError::Internal(format!(
        "Node ID already exists: {node_id}"
      )));
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
    let mut node_ids = Vec::with_capacity(keys.len());
    for _ in keys.iter() {
      node_ids.push(self.alloc_node_id());
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
  pub fn add_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

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
      tx.pending.add_edge(src, etype, dst);
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

    // Invalidate cache (traversal cache for both src and dst)
    if !bulk_load {
      self.cache_invalidate_edge(src, etype, dst);
    }

    Ok(())
  }

  /// Add multiple edges in a single WAL record
  pub fn add_edges_batch(&self, edges: &[(NodeId, ETypeId, NodeId)]) -> Result<()> {
    if edges.is_empty() {
      return Ok(());
    }

    let (txid, tx_handle) = self.require_write_tx_handle()?;
    let record = WalRecord::new(
      WalRecordType::AddEdgesBatch,
      txid,
      build_add_edges_batch_payload(edges),
    );
    self.write_wal_tx(&tx_handle, record)?;

    let bulk_load = {
      let mut tx = tx_handle.lock();
      for (src, etype, dst) in edges.iter() {
        tx.pending.add_edge(*src, *etype, *dst);
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
      tx.pending.add_edge(src, etype, dst);
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
      for (src, etype, dst, props) in edges.into_iter() {
        tx.pending.add_edge(src, etype, dst);
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
    let etype = self.etype_id_or_create(etype_name);
    self.add_edge(src, etype, dst)
  }

  /// Delete an edge
  pub fn delete_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

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
      tx.pending.delete_edge(src, etype, dst);
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
  /// Returns a flag indicating whether the edge was created.
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
    let created = if self.edge_exists(src, etype, dst) {
      false
    } else {
      self.add_edge(src, etype, dst)?;
      true
    };

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
    let key_id = self.propkey_id_or_create(key_name);
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
    let key_id = self.propkey_id_or_create(key_name);
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
      tx_mgr.record_write(txid, TxKey::Node(node_id));
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
    let label_id = self.label_id_or_create(label_name);
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
      tx_mgr.record_write(txid, TxKey::Node(node_id));
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
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Check if already exists
    if let Some(id) = self.label_id(name) {
      return Ok(id);
    }

    let label_id = self.alloc_label_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineLabel,
      txid,
      build_define_label_payload(label_id, name),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update schema maps
    {
      let mut names = self.label_names.write();
      let mut ids = self.label_ids.write();
      names.insert(name.to_string(), label_id);
      ids.insert(label_id, name.to_string());
    }

    // Update delta
    self.delta.write().define_label(label_id, name);

    Ok(label_id)
  }

  /// Define a new edge type (writes to WAL for durability)
  pub fn define_etype(&self, name: &str) -> Result<ETypeId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Check if already exists
    if let Some(id) = self.etype_id(name) {
      return Ok(id);
    }

    let etype_id = self.alloc_etype_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineEtype,
      txid,
      build_define_etype_payload(etype_id, name),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update schema maps
    {
      let mut names = self.etype_names.write();
      let mut ids = self.etype_ids.write();
      names.insert(name.to_string(), etype_id);
      ids.insert(etype_id, name.to_string());
    }

    // Update delta
    self.delta.write().define_etype(etype_id, name);

    Ok(etype_id)
  }

  /// Define a new property key (writes to WAL for durability)
  pub fn define_propkey(&self, name: &str) -> Result<PropKeyId> {
    let (txid, tx_handle) = self.require_write_tx_handle()?;

    // Check if already exists
    if let Some(id) = self.propkey_id(name) {
      return Ok(id);
    }

    let propkey_id = self.alloc_propkey_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefinePropkey,
      txid,
      build_define_propkey_payload(propkey_id, name),
    );
    self.write_wal_tx(&tx_handle, record)?;

    // Update schema maps
    {
      let mut names = self.propkey_names.write();
      let mut ids = self.propkey_ids.write();
      names.insert(name.to_string(), propkey_id);
      ids.insert(propkey_id, name.to_string());
    }

    // Update delta
    self.delta.write().define_propkey(propkey_id, name);

    Ok(propkey_id)
  }
}
