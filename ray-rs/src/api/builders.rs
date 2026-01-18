//! Query Builders
//!
//! Fluent builders for insert, update, delete, link, and unlink operations.
//!
//! Ported from src/api/builders.ts

use crate::types::{ETypeId, NodeId, PropKeyId, PropValue};
use std::collections::HashMap;

// ============================================================================
// Node Reference
// ============================================================================

/// A reference to a node with its ID and key
#[derive(Debug, Clone)]
pub struct NodeRef {
  /// Node ID
  pub id: NodeId,
  /// Node key (may be empty)
  pub key: String,
  /// Node properties (cached)
  pub props: HashMap<String, PropValue>,
}

impl NodeRef {
  /// Create a new node reference
  pub fn new(id: NodeId, key: String) -> Self {
    Self {
      id,
      key,
      props: HashMap::new(),
    }
  }

  /// Create a node reference with properties
  pub fn with_props(id: NodeId, key: String, props: HashMap<String, PropValue>) -> Self {
    Self { id, key, props }
  }

  /// Get a property value
  pub fn get_prop(&self, name: &str) -> Option<&PropValue> {
    self.props.get(name)
  }
}

// ============================================================================
// Insert Builder
// ============================================================================

/// Builder for insert operations
pub struct InsertBuilder<'a, F, R>
where
  F: FnMut(InsertData) -> R,
{
  /// Function to execute the insert
  executor: &'a mut F,
  /// Node type name
  _node_type: String,
}

/// Data for insert operation
#[derive(Debug, Clone)]
pub struct InsertData {
  /// Node key (optional)
  pub key: Option<String>,
  /// Properties to set
  pub props: HashMap<PropKeyId, PropValue>,
}

impl InsertData {
  pub fn new() -> Self {
    Self {
      key: None,
      props: HashMap::new(),
    }
  }

  pub fn with_key(mut self, key: impl Into<String>) -> Self {
    self.key = Some(key.into());
    self
  }

  pub fn with_prop(mut self, key_id: PropKeyId, value: PropValue) -> Self {
    self.props.insert(key_id, value);
    self
  }
}

impl Default for InsertData {
  fn default() -> Self {
    Self::new()
  }
}

impl<'a, F, R> InsertBuilder<'a, F, R>
where
  F: FnMut(InsertData) -> R,
{
  /// Create a new insert builder
  pub fn new(node_type: impl Into<String>, executor: &'a mut F) -> Self {
    Self {
      executor,
      _node_type: node_type.into(),
    }
  }

  /// Execute the insert with the given data
  pub fn values(self, data: InsertData) -> R {
    (self.executor)(data)
  }
}

// ============================================================================
// Update Builder
// ============================================================================

/// Builder for update operations
pub struct UpdateBuilder<'a, F, R>
where
  F: FnMut(NodeId, HashMap<PropKeyId, Option<PropValue>>) -> R,
{
  /// Function to execute the update
  executor: &'a mut F,
  /// Node to update
  node_id: Option<NodeId>,
  /// Updates to apply (None value = delete property)
  updates: HashMap<PropKeyId, Option<PropValue>>,
}

impl<'a, F, R> UpdateBuilder<'a, F, R>
where
  F: FnMut(NodeId, HashMap<PropKeyId, Option<PropValue>>) -> R,
{
  /// Create a new update builder
  pub fn new(executor: &'a mut F) -> Self {
    Self {
      executor,
      node_id: None,
      updates: HashMap::new(),
    }
  }

  /// Set the target node by ID
  pub fn where_id(mut self, node_id: NodeId) -> Self {
    self.node_id = Some(node_id);
    self
  }

  /// Set a property value
  pub fn set(mut self, prop_key_id: PropKeyId, value: PropValue) -> Self {
    self.updates.insert(prop_key_id, Some(value));
    self
  }

  /// Delete a property
  pub fn unset(mut self, prop_key_id: PropKeyId) -> Self {
    self.updates.insert(prop_key_id, None);
    self
  }

  /// Execute the update
  pub fn execute(self) -> R {
    let node_id = self
      .node_id
      .expect("Update requires a node ID (use where_id())");
    (self.executor)(node_id, self.updates)
  }
}

// ============================================================================
// Delete Builder
// ============================================================================

/// Builder for delete operations
pub struct DeleteBuilder<'a, F, R>
where
  F: FnMut(NodeId) -> R,
{
  /// Function to execute the delete
  executor: &'a mut F,
  /// Node to delete
  node_id: Option<NodeId>,
}

impl<'a, F, R> DeleteBuilder<'a, F, R>
where
  F: FnMut(NodeId) -> R,
{
  /// Create a new delete builder
  pub fn new(executor: &'a mut F) -> Self {
    Self {
      executor,
      node_id: None,
    }
  }

  /// Set the target node by ID
  pub fn where_id(mut self, node_id: NodeId) -> Self {
    self.node_id = Some(node_id);
    self
  }

  /// Execute the delete
  pub fn execute(self) -> R {
    let node_id = self
      .node_id
      .expect("Delete requires a node ID (use where_id())");
    (self.executor)(node_id)
  }
}

// ============================================================================
// Link Builder
// ============================================================================

/// Builder for creating edges (links)
pub struct LinkBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId, HashMap<PropKeyId, PropValue>) -> R,
{
  /// Function to execute the link
  executor: &'a mut F,
  /// Source node
  src: NodeId,
  /// Edge type
  etype: ETypeId,
  /// Destination node
  dst: Option<NodeId>,
  /// Edge properties
  props: HashMap<PropKeyId, PropValue>,
}

impl<'a, F, R> LinkBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId, HashMap<PropKeyId, PropValue>) -> R,
{
  /// Create a new link builder
  pub fn new(src: NodeId, etype: ETypeId, executor: &'a mut F) -> Self {
    Self {
      executor,
      src,
      etype,
      dst: None,
      props: HashMap::new(),
    }
  }

  /// Set the destination node
  pub fn to(mut self, dst: NodeId) -> Self {
    self.dst = Some(dst);
    self
  }

  /// Set an edge property
  pub fn with_prop(mut self, prop_key_id: PropKeyId, value: PropValue) -> Self {
    self.props.insert(prop_key_id, value);
    self
  }

  /// Execute the link
  pub fn execute(self) -> R {
    let dst = self.dst.expect("Link requires a destination (use to())");
    (self.executor)(self.src, self.etype, dst, self.props)
  }
}

// ============================================================================
// Unlink Builder
// ============================================================================

/// Builder for deleting edges (unlinks)
pub struct UnlinkBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId) -> R,
{
  /// Function to execute the unlink
  executor: &'a mut F,
  /// Source node
  src: NodeId,
  /// Edge type
  etype: ETypeId,
  /// Destination node
  dst: Option<NodeId>,
}

impl<'a, F, R> UnlinkBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId) -> R,
{
  /// Create a new unlink builder
  pub fn new(src: NodeId, etype: ETypeId, executor: &'a mut F) -> Self {
    Self {
      executor,
      src,
      etype,
      dst: None,
    }
  }

  /// Set the destination node
  pub fn from_node(mut self, dst: NodeId) -> Self {
    self.dst = Some(dst);
    self
  }

  /// Execute the unlink
  pub fn execute(self) -> R {
    let dst = self
      .dst
      .expect("Unlink requires a destination (use from_node())");
    (self.executor)(self.src, self.etype, dst)
  }
}

// ============================================================================
// Update Edge Builder
// ============================================================================

/// Builder for updating edge properties
pub struct UpdateEdgeBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId, HashMap<PropKeyId, Option<PropValue>>) -> R,
{
  /// Function to execute the update
  executor: &'a mut F,
  /// Source node
  src: NodeId,
  /// Edge type
  etype: ETypeId,
  /// Destination node
  dst: NodeId,
  /// Updates to apply
  updates: HashMap<PropKeyId, Option<PropValue>>,
}

impl<'a, F, R> UpdateEdgeBuilder<'a, F, R>
where
  F: FnMut(NodeId, ETypeId, NodeId, HashMap<PropKeyId, Option<PropValue>>) -> R,
{
  /// Create a new update edge builder
  pub fn new(src: NodeId, etype: ETypeId, dst: NodeId, executor: &'a mut F) -> Self {
    Self {
      executor,
      src,
      etype,
      dst,
      updates: HashMap::new(),
    }
  }

  /// Set an edge property
  pub fn set(mut self, prop_key_id: PropKeyId, value: PropValue) -> Self {
    self.updates.insert(prop_key_id, Some(value));
    self
  }

  /// Delete an edge property
  pub fn unset(mut self, prop_key_id: PropKeyId) -> Self {
    self.updates.insert(prop_key_id, None);
    self
  }

  /// Execute the update
  pub fn execute(self) -> R {
    (self.executor)(self.src, self.etype, self.dst, self.updates)
  }
}

// ============================================================================
// Batch Operations
// ============================================================================

/// A batch operation that can be executed in a transaction
#[derive(Debug, Clone)]
pub enum BatchOp {
  /// Insert a node
  Insert(InsertData),
  /// Update a node
  Update {
    node_id: NodeId,
    updates: HashMap<PropKeyId, Option<PropValue>>,
  },
  /// Delete a node
  Delete { node_id: NodeId },
  /// Create an edge
  Link {
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    props: HashMap<PropKeyId, PropValue>,
  },
  /// Delete an edge
  Unlink {
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  },
  /// Update edge properties
  UpdateEdge {
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    updates: HashMap<PropKeyId, Option<PropValue>>,
  },
}

/// Collect batch operations for execution in a single transaction
#[derive(Debug, Default)]
pub struct BatchBuilder {
  ops: Vec<BatchOp>,
}

impl BatchBuilder {
  pub fn new() -> Self {
    Self { ops: Vec::new() }
  }

  /// Add an insert operation
  pub fn insert(mut self, data: InsertData) -> Self {
    self.ops.push(BatchOp::Insert(data));
    self
  }

  /// Add an update operation
  pub fn update(mut self, node_id: NodeId, updates: HashMap<PropKeyId, Option<PropValue>>) -> Self {
    self.ops.push(BatchOp::Update { node_id, updates });
    self
  }

  /// Add a delete operation
  pub fn delete(mut self, node_id: NodeId) -> Self {
    self.ops.push(BatchOp::Delete { node_id });
    self
  }

  /// Add a link operation
  pub fn link(mut self, src: NodeId, etype: ETypeId, dst: NodeId) -> Self {
    self.ops.push(BatchOp::Link {
      src,
      etype,
      dst,
      props: HashMap::new(),
    });
    self
  }

  /// Add a link operation with properties
  pub fn link_with_props(
    mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    props: HashMap<PropKeyId, PropValue>,
  ) -> Self {
    self.ops.push(BatchOp::Link {
      src,
      etype,
      dst,
      props,
    });
    self
  }

  /// Add an unlink operation
  pub fn unlink(mut self, src: NodeId, etype: ETypeId, dst: NodeId) -> Self {
    self.ops.push(BatchOp::Unlink { src, etype, dst });
    self
  }

  /// Get the collected operations
  pub fn build(self) -> Vec<BatchOp> {
    self.ops
  }

  /// Get the number of operations
  pub fn len(&self) -> usize {
    self.ops.len()
  }

  /// Check if empty
  pub fn is_empty(&self) -> bool {
    self.ops.is_empty()
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_node_ref() {
    let node = NodeRef::new(1, "alice".to_string());
    assert_eq!(node.id, 1);
    assert_eq!(node.key, "alice");
    assert!(node.props.is_empty());
  }

  #[test]
  fn test_node_ref_with_props() {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    props.insert("age".to_string(), PropValue::I64(30));

    let node = NodeRef::with_props(1, "alice".to_string(), props);

    assert_eq!(
      node.get_prop("name"),
      Some(&PropValue::String("Alice".to_string()))
    );
    assert_eq!(node.get_prop("age"), Some(&PropValue::I64(30)));
    assert_eq!(node.get_prop("unknown"), None);
  }

  #[test]
  fn test_insert_data() {
    let data = InsertData::new()
      .with_key("alice")
      .with_prop(1, PropValue::String("Alice".to_string()))
      .with_prop(2, PropValue::I64(30));

    assert_eq!(data.key, Some("alice".to_string()));
    assert_eq!(data.props.len(), 2);
  }

  #[test]
  fn test_insert_builder() {
    let mut executed = false;
    let mut captured_data: Option<InsertData> = None;

    let mut executor = |data: InsertData| {
      executed = true;
      captured_data = Some(data);
      1u64 // Return node ID
    };

    let data = InsertData::new().with_key("test");
    let result = InsertBuilder::new("User", &mut executor).values(data);

    assert!(executed);
    assert_eq!(result, 1);
    assert_eq!(captured_data.unwrap().key, Some("test".to_string()));
  }

  #[test]
  fn test_update_builder() {
    let mut captured: Option<(NodeId, HashMap<PropKeyId, Option<PropValue>>)> = None;

    let mut executor = |node_id: NodeId, updates: HashMap<PropKeyId, Option<PropValue>>| {
      captured = Some((node_id, updates));
    };

    UpdateBuilder::new(&mut executor)
      .where_id(42)
      .set(1, PropValue::String("Updated".to_string()))
      .unset(2)
      .execute();

    let (node_id, updates) = captured.unwrap();
    assert_eq!(node_id, 42);
    assert_eq!(updates.len(), 2);
    assert!(updates.get(&1).unwrap().is_some());
    assert!(updates.get(&2).unwrap().is_none());
  }

  #[test]
  fn test_delete_builder() {
    let mut deleted_id: Option<NodeId> = None;

    let mut executor = |node_id: NodeId| {
      deleted_id = Some(node_id);
      true
    };

    let result = DeleteBuilder::new(&mut executor).where_id(42).execute();

    assert!(result);
    assert_eq!(deleted_id, Some(42));
  }

  #[test]
  fn test_link_builder() {
    let mut captured: Option<(NodeId, ETypeId, NodeId, HashMap<PropKeyId, PropValue>)> = None;

    let mut executor = |src, etype, dst, props| {
      captured = Some((src, etype, dst, props));
    };

    LinkBuilder::new(1, 10, &mut executor)
      .to(2)
      .with_prop(100, PropValue::F64(1.5))
      .execute();

    let (src, etype, dst, props) = captured.unwrap();
    assert_eq!(src, 1);
    assert_eq!(etype, 10);
    assert_eq!(dst, 2);
    assert_eq!(props.get(&100), Some(&PropValue::F64(1.5)));
  }

  #[test]
  fn test_unlink_builder() {
    let mut captured: Option<(NodeId, ETypeId, NodeId)> = None;

    let mut executor = |src, etype, dst| {
      captured = Some((src, etype, dst));
      true
    };

    let result = UnlinkBuilder::new(1, 10, &mut executor)
      .from_node(2)
      .execute();

    assert!(result);
    let (src, etype, dst) = captured.unwrap();
    assert_eq!(src, 1);
    assert_eq!(etype, 10);
    assert_eq!(dst, 2);
  }

  #[test]
  fn test_batch_builder() {
    let batch = BatchBuilder::new()
      .insert(InsertData::new().with_key("alice"))
      .insert(InsertData::new().with_key("bob"))
      .link(1, 10, 2)
      .update(1, HashMap::new())
      .delete(3)
      .unlink(1, 10, 2)
      .build();

    assert_eq!(batch.len(), 6);

    assert!(matches!(batch[0], BatchOp::Insert(_)));
    assert!(matches!(batch[1], BatchOp::Insert(_)));
    assert!(matches!(batch[2], BatchOp::Link { .. }));
    assert!(matches!(batch[3], BatchOp::Update { .. }));
    assert!(matches!(batch[4], BatchOp::Delete { .. }));
    assert!(matches!(batch[5], BatchOp::Unlink { .. }));
  }

  #[test]
  fn test_batch_builder_empty() {
    let batch = BatchBuilder::new();
    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
  }
}
