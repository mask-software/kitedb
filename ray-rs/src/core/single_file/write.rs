//! Write operations for SingleFileDB
//!
//! Handles all mutation operations: create/delete nodes, add/delete edges,
//! set/delete properties, and node labels.

use crate::core::wal::record::{
  build_add_edge_payload, build_add_node_label_payload, build_create_node_payload,
  build_define_etype_payload, build_define_label_payload, build_define_propkey_payload,
  build_del_edge_prop_payload, build_del_node_prop_payload, build_delete_edge_payload,
  build_delete_node_payload, build_remove_node_label_payload, build_set_edge_prop_payload,
  build_set_node_prop_payload, WalRecord,
};
use crate::error::Result;
use crate::types::*;

use super::SingleFileDB;

impl SingleFileDB {
  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a node
  pub fn create_node(&self, key: Option<&str>) -> Result<NodeId> {
    let txid = self.require_write_tx()?;
    let node_id = self.alloc_node_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      txid,
      build_create_node_payload(node_id, key),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().create_node(node_id, key);

    Ok(node_id)
  }

  /// Delete a node
  pub fn delete_node(&self, node_id: NodeId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DeleteNode,
      txid,
      build_delete_node_payload(node_id),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().delete_node(node_id);

    // Invalidate cache
    self.cache_invalidate_node(node_id);

    Ok(())
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  pub fn add_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::AddEdge,
      txid,
      build_add_edge_payload(src, etype, dst),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().add_edge(src, etype, dst);

    // Invalidate cache (traversal cache for both src and dst)
    self.cache_invalidate_edge(src, etype, dst);

    Ok(())
  }

  /// Add an edge by type name
  pub fn add_edge_by_name(&self, src: NodeId, etype_name: &str, dst: NodeId) -> Result<()> {
    let etype = self.get_or_create_etype(etype_name);
    self.add_edge(src, etype, dst)
  }

  /// Delete an edge
  pub fn delete_edge(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DeleteEdge,
      txid,
      build_delete_edge_payload(src, etype, dst),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().delete_edge(src, etype, dst);

    // Invalidate cache
    self.cache_invalidate_edge(src, etype, dst);

    Ok(())
  }

  // ========================================================================
  // Node Property Operations
  // ========================================================================

  /// Set a node property
  pub fn set_node_prop(&self, node_id: NodeId, key_id: PropKeyId, value: PropValue) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetNodeProp,
      txid,
      build_set_node_prop_payload(node_id, key_id, &value),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().set_node_prop(node_id, key_id, value);

    // Invalidate cache
    self.cache_invalidate_node(node_id);

    Ok(())
  }

  /// Set a node property by key name
  pub fn set_node_prop_by_name(
    &self,
    node_id: NodeId,
    key_name: &str,
    value: PropValue,
  ) -> Result<()> {
    let key_id = self.get_or_create_propkey(key_name);
    self.set_node_prop(node_id, key_id, value)
  }

  /// Delete a node property
  pub fn delete_node_prop(&self, node_id: NodeId, key_id: PropKeyId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelNodeProp,
      txid,
      build_del_node_prop_payload(node_id, key_id),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().delete_node_prop(node_id, key_id);

    // Invalidate cache
    self.cache_invalidate_node(node_id);

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
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::SetEdgeProp,
      txid,
      build_set_edge_prop_payload(src, etype, dst, key_id, &value),
    );
    self.write_wal(record)?;

    // Update delta
    self
      .delta
      .write()
      .set_edge_prop(src, etype, dst, key_id, value);

    // Invalidate cache
    self.cache_invalidate_edge(src, etype, dst);

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
    let key_id = self.get_or_create_propkey(key_name);
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
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DelEdgeProp,
      txid,
      build_del_edge_prop_payload(src, etype, dst, key_id),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().delete_edge_prop(src, etype, dst, key_id);

    // Invalidate cache
    self.cache_invalidate_edge(src, etype, dst);

    Ok(())
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Add a label to a node
  pub fn add_node_label(&self, node_id: NodeId, label_id: LabelId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::AddNodeLabel,
      txid,
      build_add_node_label_payload(node_id, label_id),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().add_node_label(node_id, label_id);

    // Invalidate cache (label changes affect node)
    self.cache_invalidate_node(node_id);

    Ok(())
  }

  /// Add a label to a node by name
  pub fn add_node_label_by_name(&self, node_id: NodeId, label_name: &str) -> Result<()> {
    let label_id = self.get_or_create_label(label_name);
    self.add_node_label(node_id, label_id)
  }

  /// Remove a label from a node
  pub fn remove_node_label(&self, node_id: NodeId, label_id: LabelId) -> Result<()> {
    let txid = self.require_write_tx()?;

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::RemoveNodeLabel,
      txid,
      build_remove_node_label_payload(node_id, label_id),
    );
    self.write_wal(record)?;

    // Update delta
    self.delta.write().remove_node_label(node_id, label_id);

    // Invalidate cache (label changes affect node)
    self.cache_invalidate_node(node_id);

    Ok(())
  }

  /// Remove a label from a node by name
  pub fn remove_node_label_by_name(&self, node_id: NodeId, label_name: &str) -> Result<()> {
    if let Some(label_id) = self.get_label_id(label_name) {
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
    let txid = self.require_write_tx()?;

    // Check if already exists
    if let Some(id) = self.get_label_id(name) {
      return Ok(id);
    }

    let label_id = self.alloc_label_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineLabel,
      txid,
      build_define_label_payload(label_id, name),
    );
    self.write_wal(record)?;

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
    let txid = self.require_write_tx()?;

    // Check if already exists
    if let Some(id) = self.get_etype_id(name) {
      return Ok(id);
    }

    let etype_id = self.alloc_etype_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefineEtype,
      txid,
      build_define_etype_payload(etype_id, name),
    );
    self.write_wal(record)?;

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
    let txid = self.require_write_tx()?;

    // Check if already exists
    if let Some(id) = self.get_propkey_id(name) {
      return Ok(id);
    }

    let propkey_id = self.alloc_propkey_id();

    // Write WAL record
    let record = WalRecord::new(
      WalRecordType::DefinePropkey,
      txid,
      build_define_propkey_payload(propkey_id, name),
    );
    self.write_wal(record)?;

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
