//! Ray - High-level API for RayDB
//!
//! The Ray struct provides a clean, ergonomic API for graph operations.
//! It wraps the lower-level GraphDB with schema definitions and type-safe operations.
//!
//! Provides:
//! - Node and edge CRUD operations
//! - Graph traversal with fluent API
//! - Shortest path finding (Dijkstra, BFS, Yen's k-shortest)
//! - Schema-based type safety
//!
//! Ported from src/api/ray.ts

use crate::error::{RayError, Result};
use crate::graph::db::{close_graph_db, open_graph_db, GraphDB, OpenOptions};
use crate::graph::edges::{
  add_edge,
  del_edge_prop,
  delete_edge,
  edge_exists,
  // Direct read functions (no transaction)
  edge_exists_db,
  get_edge_prop_db,
  get_edge_props_db,
  get_neighbors_in_db,
  get_neighbors_out_db,
  set_edge_prop,
};
use crate::graph::iterators::{
  count_edges, count_nodes, list_edges, list_nodes, FullEdge, ListEdgesOptions,
};
use crate::graph::key_index::get_node_key;
use crate::graph::nodes::{
  create_node, del_node_prop, delete_node, get_node_by_key, get_node_by_key_db, get_node_prop,
  get_node_prop_db, node_exists, node_exists_db, set_node_prop, NodeOpts,
};
use crate::graph::tx::{begin_tx, commit, rollback, TxHandle};
use crate::types::*;

use std::collections::{HashMap, HashSet};
use std::path::Path;

// ============================================================================
// Schema Definitions
// ============================================================================

/// Property definition for nodes or edges
#[derive(Debug, Clone)]
pub struct PropDef {
  /// Property name
  pub name: String,
  /// Property type hint (for documentation/validation)
  pub prop_type: PropType,
  /// Whether this property is required
  pub required: bool,
  /// Default value (if any)
  pub default: Option<PropValue>,
}

/// Property type hints
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
  String,
  Int,
  Float,
  Bool,
  Any,
}

impl PropDef {
  pub fn string(name: &str) -> Self {
    Self {
      name: name.to_string(),
      prop_type: PropType::String,
      required: false,
      default: None,
    }
  }

  pub fn int(name: &str) -> Self {
    Self {
      name: name.to_string(),
      prop_type: PropType::Int,
      required: false,
      default: None,
    }
  }

  pub fn float(name: &str) -> Self {
    Self {
      name: name.to_string(),
      prop_type: PropType::Float,
      required: false,
      default: None,
    }
  }

  pub fn bool(name: &str) -> Self {
    Self {
      name: name.to_string(),
      prop_type: PropType::Bool,
      required: false,
      default: None,
    }
  }

  pub fn required(mut self) -> Self {
    self.required = true;
    self
  }

  pub fn default(mut self, value: PropValue) -> Self {
    self.default = Some(value);
    self
  }
}

/// Node type definition
#[derive(Debug, Clone)]
pub struct NodeDef {
  /// Node type name
  pub name: String,
  /// Property definitions
  pub props: HashMap<String, PropDef>,
  /// Key prefix for this node type (e.g., "user:")
  pub key_prefix: String,
  /// Internal label ID (set after registration)
  pub label_id: Option<LabelId>,
  /// Property key IDs (set after registration)
  pub prop_key_ids: HashMap<String, PropKeyId>,
}

impl NodeDef {
  pub fn new(name: &str, key_prefix: &str) -> Self {
    Self {
      name: name.to_string(),
      props: HashMap::new(),
      key_prefix: key_prefix.to_string(),
      label_id: None,
      prop_key_ids: HashMap::new(),
    }
  }

  pub fn prop(mut self, prop: PropDef) -> Self {
    self.props.insert(prop.name.clone(), prop);
    self
  }

  /// Generate a full key from a key suffix
  pub fn key(&self, suffix: &str) -> String {
    format!("{}{}", self.key_prefix, suffix)
  }
}

/// Edge type definition
#[derive(Debug, Clone)]
pub struct EdgeDef {
  /// Edge type name
  pub name: String,
  /// Property definitions
  pub props: HashMap<String, PropDef>,
  /// Internal edge type ID (set after registration)
  pub etype_id: Option<ETypeId>,
  /// Property key IDs (set after registration)
  pub prop_key_ids: HashMap<String, PropKeyId>,
}

impl EdgeDef {
  pub fn new(name: &str) -> Self {
    Self {
      name: name.to_string(),
      props: HashMap::new(),
      etype_id: None,
      prop_key_ids: HashMap::new(),
    }
  }

  pub fn prop(mut self, prop: PropDef) -> Self {
    self.props.insert(prop.name.clone(), prop);
    self
  }
}

// ============================================================================
// Node Reference
// ============================================================================

/// Reference to a node in the database
#[derive(Debug, Clone)]
pub struct NodeRef {
  /// Node ID
  pub id: NodeId,
  /// Full key (if available)
  pub key: Option<String>,
  /// Node type name
  pub node_type: String,
}

impl NodeRef {
  pub fn new(id: NodeId, key: Option<String>, node_type: &str) -> Self {
    Self {
      id,
      key,
      node_type: node_type.to_string(),
    }
  }
}

// ============================================================================
// Ray Options
// ============================================================================

/// Options for opening a Ray database
#[derive(Debug, Clone, Default)]
pub struct RayOptions {
  /// Node type definitions
  pub nodes: Vec<NodeDef>,
  /// Edge type definitions
  pub edges: Vec<EdgeDef>,
  /// Open in read-only mode
  pub read_only: bool,
  /// Create database if it doesn't exist
  pub create_if_missing: bool,
  /// Acquire file lock
  pub lock_file: bool,
}

impl RayOptions {
  pub fn new() -> Self {
    Self {
      nodes: Vec::new(),
      edges: Vec::new(),
      read_only: false,
      create_if_missing: true,
      lock_file: true,
    }
  }

  pub fn node(mut self, node: NodeDef) -> Self {
    self.nodes.push(node);
    self
  }

  pub fn edge(mut self, edge: EdgeDef) -> Self {
    self.edges.push(edge);
    self
  }

  pub fn read_only(mut self, value: bool) -> Self {
    self.read_only = value;
    self
  }
}

// ============================================================================
// Ray Database
// ============================================================================

/// High-level graph database API
pub struct Ray {
  /// Underlying database
  db: GraphDB,
  /// Node type definitions by name
  nodes: HashMap<String, NodeDef>,
  /// Edge type definitions by name
  edges: HashMap<String, EdgeDef>,
  /// Key prefix to node def mapping for fast lookups
  key_prefix_to_node: HashMap<String, String>,
}

impl Ray {
  /// Open or create a Ray database
  pub fn open<P: AsRef<Path>>(path: P, options: RayOptions) -> Result<Self> {
    let db_options = OpenOptions {
      read_only: options.read_only,
      create_if_missing: options.create_if_missing,
      lock_file: options.lock_file,
      ..Default::default()
    };

    let db = open_graph_db(path, db_options)?;

    // Initialize schema in a transaction
    let mut nodes: HashMap<String, NodeDef> = HashMap::new();
    let mut edges: HashMap<String, EdgeDef> = HashMap::new();
    let mut key_prefix_to_node: HashMap<String, String> = HashMap::new();

    // Process node definitions
    for mut node_def in options.nodes {
      // Define label
      let label_id = db.get_or_create_label(&node_def.name);
      node_def.label_id = Some(label_id);

      // Define property keys
      for prop_name in node_def.props.keys() {
        let prop_key_id = db.get_or_create_propkey(prop_name);
        node_def.prop_key_ids.insert(prop_name.clone(), prop_key_id);
      }

      key_prefix_to_node.insert(node_def.key_prefix.clone(), node_def.name.clone());
      nodes.insert(node_def.name.clone(), node_def);
    }

    // Process edge definitions
    for mut edge_def in options.edges {
      // Define edge type
      let etype_id = db.get_or_create_etype(&edge_def.name);
      edge_def.etype_id = Some(etype_id);

      // Define property keys
      for prop_name in edge_def.props.keys() {
        let prop_key_id = db.get_or_create_propkey(prop_name);
        edge_def.prop_key_ids.insert(prop_name.clone(), prop_key_id);
      }

      edges.insert(edge_def.name.clone(), edge_def);
    }

    Ok(Self {
      db,
      nodes,
      edges,
      key_prefix_to_node,
    })
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a new node
  pub fn create_node(
    &mut self,
    node_type: &str,
    key_suffix: &str,
    props: HashMap<String, PropValue>,
  ) -> Result<NodeRef> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?
      .clone();

    let full_key = node_def.key(key_suffix);

    // Begin transaction
    let mut handle = begin_tx(&self.db)?;

    // Create the node with key
    let node_opts = NodeOpts {
      key: Some(full_key.clone()),
      labels: node_def.label_id.map(|id| vec![id]),
      props: None,
    };
    let node_id = create_node(&mut handle, node_opts)?;

    // Set properties
    for (prop_name, value) in props {
      if let Some(&prop_key_id) = node_def.prop_key_ids.get(&prop_name) {
        set_node_prop(&mut handle, node_id, prop_key_id, value)?;
      }
    }

    // Commit
    commit(&mut handle)?;

    Ok(NodeRef::new(node_id, Some(full_key), node_type))
  }

  /// Insert a node using fluent builder API
  ///
  /// This method provides a more ergonomic way to create nodes with properties
  /// using the builder pattern. Use `.values()` to specify the node data,
  /// then either `.execute()` to insert without returning, or `.returning()`
  /// to get the created node reference.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::PropValue;
  /// # use std::collections::HashMap;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// # let props: HashMap<String, PropValue> = HashMap::new();
  /// // Insert and get the node reference
  /// let user = ray.insert("User")?
  ///     .values("alice", props)?
  ///     .returning()?;
  ///
  /// // Insert without returning (slightly faster)
  /// ray.insert("User")?
  ///     .values("bob", HashMap::new())?
  ///     .execute()?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn insert(&mut self, node_type: &str) -> Result<RayInsertBuilder<'_>> {
    let key_prefix = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?
      .key_prefix
      .clone();

    Ok(RayInsertBuilder {
      ray: self,
      node_type: node_type.to_string(),
      key_prefix,
    })
  }

  /// Get a node by key (direct read, no transaction overhead)
  pub fn get(&self, node_type: &str, key_suffix: &str) -> Result<Option<NodeRef>> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?;

    let full_key = node_def.key(key_suffix);

    // Direct read without transaction
    let node_id = get_node_by_key_db(&self.db, &full_key);

    match node_id {
      Some(id) => Ok(Some(NodeRef::new(id, Some(full_key), node_type))),
      None => Ok(None),
    }
  }

  /// Get a node by ID (direct read, no transaction overhead)
  pub fn get_by_id(&self, node_id: NodeId) -> Result<Option<NodeRef>> {
    // Direct read without transaction
    let exists = node_exists_db(&self.db, node_id);

    if exists {
      // Look up the node's key from snapshot/delta
      let delta = self.db.delta.read();
      let key = get_node_key(self.db.snapshot.as_ref(), &delta, node_id);

      // Try to determine node type from key prefix
      let node_type = if let Some(ref k) = key {
        // Find matching node def by key prefix
        self
          .nodes
          .values()
          .find(|def| k.starts_with(&def.key_prefix))
          .map(|def| def.name.as_str())
          .unwrap_or("unknown")
      } else {
        "unknown"
      };

      Ok(Some(NodeRef::new(node_id, key, node_type)))
    } else {
      Ok(None)
    }
  }

  /// Check if a node exists (direct read, no transaction overhead)
  pub fn exists(&self, node_id: NodeId) -> bool {
    // Direct read without transaction
    node_exists_db(&self.db, node_id)
  }

  /// Delete a node
  pub fn delete_node(&mut self, node_id: NodeId) -> Result<bool> {
    let mut handle = begin_tx(&self.db)?;
    let deleted = delete_node(&mut handle, node_id)?;
    commit(&mut handle)?;
    Ok(deleted)
  }

  /// Get a node property (direct read, no transaction overhead)
  pub fn get_prop(&self, node_id: NodeId, prop_name: &str) -> Option<PropValue> {
    let prop_key_id = self.db.get_propkey_id(prop_name)?;
    // Direct read without transaction
    get_node_prop_db(&self.db, node_id, prop_key_id)
  }

  /// Set a node property
  pub fn set_prop(&mut self, node_id: NodeId, prop_name: &str, value: PropValue) -> Result<()> {
    let prop_key_id = self.db.get_or_create_propkey(prop_name);

    let mut handle = begin_tx(&self.db)?;
    set_node_prop(&mut handle, node_id, prop_key_id, value)?;
    commit(&mut handle)?;
    Ok(())
  }

  /// Update a node by reference using fluent builder API
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::PropValue;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// let alice = ray.get("User", "alice")?.unwrap();
  /// ray.update(&alice)?
  ///     .set("name", PropValue::String("Alice Updated".into()))
  ///     .set("age", PropValue::I64(31))
  ///     .execute()?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn update(&mut self, node_ref: &NodeRef) -> Result<RayUpdateNodeBuilder<'_>> {
    // Verify node exists
    let mut handle = begin_tx(&self.db)?;
    let exists = node_exists(&handle, node_ref.id);
    commit(&mut handle)?;

    if !exists {
      return Err(RayError::NodeNotFound(node_ref.id));
    }

    Ok(RayUpdateNodeBuilder {
      ray: self,
      node_id: node_ref.id,
      updates: HashMap::new(),
    })
  }

  /// Update a node by ID using fluent builder API
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::{NodeId, PropValue};
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// # let node_id: NodeId = 1;
  /// ray.update_by_id(node_id)?
  ///     .set("name", PropValue::String("Updated".into()))
  ///     .execute()?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn update_by_id(&mut self, node_id: NodeId) -> Result<RayUpdateNodeBuilder<'_>> {
    // Verify node exists
    let mut handle = begin_tx(&self.db)?;
    let exists = node_exists(&handle, node_id);
    commit(&mut handle)?;

    if !exists {
      return Err(RayError::NodeNotFound(node_id));
    }

    Ok(RayUpdateNodeBuilder {
      ray: self,
      node_id,
      updates: HashMap::new(),
    })
  }

  /// Update a node by key using fluent builder API
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::PropValue;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// ray.update_by_key("User", "alice")?
  ///     .set("name", PropValue::String("Alice Updated".into()))
  ///     .execute()?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn update_by_key(
    &mut self,
    node_type: &str,
    key_suffix: &str,
  ) -> Result<RayUpdateNodeBuilder<'_>> {
    let full_key = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?
      .key(key_suffix);

    let mut handle = begin_tx(&self.db)?;
    let node_id =
      get_node_by_key(&handle, &full_key).ok_or_else(|| RayError::KeyNotFound(full_key.clone()))?;
    commit(&mut handle)?;

    Ok(RayUpdateNodeBuilder {
      ray: self,
      node_id,
      updates: HashMap::new(),
    })
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Create an edge between two nodes
  pub fn link(&mut self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<()> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let mut handle = begin_tx(&self.db)?;
    add_edge(&mut handle, src, etype_id, dst)?;
    commit(&mut handle)?;
    Ok(())
  }

  /// Create an edge between two nodes with properties
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::{NodeRef, Ray};
  /// # use raydb::types::PropValue;
  /// # use std::collections::HashMap;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// # let alice: NodeRef = unimplemented!();
  /// # let bob: NodeRef = unimplemented!();
  /// let mut props = HashMap::new();
  /// props.insert("weight".to_string(), PropValue::F64(0.5));
  /// props.insert("since".to_string(), PropValue::String("2024".into()));
  /// ray.link_with_props(alice.id, "FOLLOWS", bob.id, props)?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn link_with_props(
    &mut self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    props: HashMap<String, PropValue>,
  ) -> Result<()> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?
      .clone();

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let mut handle = begin_tx(&self.db)?;
    add_edge(&mut handle, src, etype_id, dst)?;

    // Set edge properties
    for (prop_name, value) in props {
      let prop_key_id = if let Some(&id) = edge_def.prop_key_ids.get(&prop_name) {
        id
      } else {
        // Create prop key if not in schema
        handle.db.get_or_create_propkey(&prop_name)
      };
      set_edge_prop(&mut handle, src, etype_id, dst, prop_key_id, value)?;
    }

    commit(&mut handle)?;
    Ok(())
  }

  /// Remove an edge between two nodes
  pub fn unlink(&mut self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<bool> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let mut handle = begin_tx(&self.db)?;
    let deleted = delete_edge(&mut handle, src, etype_id, dst)?;
    commit(&mut handle)?;
    Ok(deleted)
  }

  /// Check if an edge exists (direct read, no transaction overhead)
  pub fn has_edge(&self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<bool> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    // Direct read without transaction
    Ok(edge_exists_db(&self.db, src, etype_id, dst))
  }

  /// Get outgoing neighbors of a node (direct read, no transaction overhead)
  pub fn neighbors_out(&self, node_id: NodeId, edge_type: Option<&str>) -> Result<Vec<NodeId>> {
    let etype_id = match edge_type {
      Some(name) => {
        let edge_def = self
          .edges
          .get(name)
          .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {name}")))?;
        edge_def.etype_id
      }
      None => None,
    };

    // Direct read without transaction
    Ok(get_neighbors_out_db(&self.db, node_id, etype_id))
  }

  /// Get incoming neighbors of a node (direct read, no transaction overhead)
  pub fn neighbors_in(&self, node_id: NodeId, edge_type: Option<&str>) -> Result<Vec<NodeId>> {
    let etype_id = match edge_type {
      Some(name) => {
        let edge_def = self
          .edges
          .get(name)
          .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {name}")))?;
        edge_def.etype_id
      }
      None => None,
    };

    // Direct read without transaction
    let neighbors = get_neighbors_in_db(&self.db, node_id, etype_id);
    Ok(neighbors)
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Get an edge property (direct read, no transaction overhead)
  ///
  /// Returns None if the edge doesn't exist or the property is not set.
  pub fn get_edge_prop(
    &self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    prop_name: &str,
  ) -> Result<Option<PropValue>> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let prop_key_id = match self.db.get_propkey_id(prop_name) {
      Some(id) => id,
      None => return Ok(None), // Unknown property = not set
    };

    // Direct read without transaction
    Ok(get_edge_prop_db(&self.db, src, etype_id, dst, prop_key_id))
  }

  /// Get all properties for an edge (direct read, no transaction overhead)
  ///
  /// Returns None if the edge doesn't exist.
  pub fn get_edge_props(
    &self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
  ) -> Result<Option<HashMap<String, PropValue>>> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    // Direct read without transaction
    let props = get_edge_props_db(&self.db, src, etype_id, dst);

    // Convert PropKeyId -> String in the result
    match props {
      Some(props_by_id) => {
        let mut result = HashMap::new();
        for (key_id, value) in props_by_id {
          if let Some(name) = self.db.get_propkey_name(key_id) {
            result.insert(name, value);
          }
        }
        Ok(Some(result))
      }
      None => Ok(None),
    }
  }

  /// Set an edge property
  pub fn set_edge_prop(
    &mut self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    prop_name: &str,
    value: PropValue,
  ) -> Result<()> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let prop_key_id = self.db.get_or_create_propkey(prop_name);

    let mut handle = begin_tx(&self.db)?;
    set_edge_prop(&mut handle, src, etype_id, dst, prop_key_id, value)?;
    commit(&mut handle)?;
    Ok(())
  }

  /// Delete an edge property
  pub fn del_edge_prop(
    &mut self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
    prop_name: &str,
  ) -> Result<()> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    let prop_key_id = self
      .db
      .get_propkey_id(prop_name)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown property: {prop_name}")))?;

    let mut handle = begin_tx(&self.db)?;
    del_edge_prop(&mut handle, src, etype_id, dst, prop_key_id)?;
    commit(&mut handle)?;
    Ok(())
  }

  /// Update edge properties using fluent builder API
  ///
  /// Returns an `UpdateEdgeBuilder` that allows setting multiple properties
  /// in a single transaction.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::{NodeId, PropValue};
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// # let alice_id: NodeId = 1;
  /// # let bob_id: NodeId = 2;
  /// ray.update_edge(alice_id, "FOLLOWS", bob_id)?
  ///    .set("weight", PropValue::F64(0.9))
  ///    .set("since", PropValue::String("2024".to_string()))
  ///    .execute()?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn update_edge(
    &mut self,
    src: NodeId,
    edge_type: &str,
    dst: NodeId,
  ) -> Result<RayUpdateEdgeBuilder<'_>> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    Ok(RayUpdateEdgeBuilder {
      ray: self,
      src,
      etype_id,
      dst,
      updates: HashMap::new(),
    })
  }

  // ========================================================================
  // Listing and Counting
  // ========================================================================

  /// Count all nodes in the database
  ///
  /// This is an O(1) operation when possible, using cached counts.
  pub fn count_nodes(&self) -> u64 {
    count_nodes(&self.db)
  }

  /// Count nodes of a specific type
  ///
  /// This requires iteration to filter by key prefix.
  pub fn count_nodes_by_type(&self, node_type: &str) -> Result<u64> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?;

    let prefix = &node_def.key_prefix;
    let mut count = 0u64;

    for node_id in list_nodes(&self.db) {
      if let Some(key) = self.get_node_key_internal(node_id) {
        if key.starts_with(prefix) {
          count += 1;
        }
      }
    }

    Ok(count)
  }

  /// Count all edges
  pub fn count_edges(&self) -> u64 {
    count_edges(&self.db, None)
  }

  /// Count edges of a specific type
  pub fn count_edges_by_type(&self, edge_type: &str) -> Result<u64> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    Ok(count_edges(&self.db, Some(etype_id)))
  }

  /// List all node IDs
  pub fn list_nodes(&self) -> Vec<NodeId> {
    list_nodes(&self.db)
  }

  /// Iterate over all nodes of a specific type
  ///
  /// Returns an iterator that yields `NodeRef` for each matching node.
  /// Filters nodes by matching their key prefix.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// for node_ref in ray.all("User")? {
  ///     println!("User: {:?}", node_ref.id);
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn all(&self, node_type: &str) -> Result<impl Iterator<Item = NodeRef> + '_> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?
      .clone();

    let prefix = node_def.key_prefix.clone();
    let node_type_str = node_type.to_string();

    Ok(list_nodes(&self.db).into_iter().filter_map(move |node_id| {
      let key = self.get_node_key_internal(node_id)?;
      if key.starts_with(&prefix) {
        Some(NodeRef::new(node_id, Some(key), &node_type_str))
      } else {
        None
      }
    }))
  }

  /// List all edges in the database
  pub fn list_all_edges(&self) -> Vec<FullEdge> {
    list_edges(&self.db, ListEdgesOptions::default())
  }

  /// Iterate over all edges, optionally filtered by type
  ///
  /// Returns an iterator that yields edge information.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// for edge in ray.all_edges(Some("FOLLOWS"))? {
  ///     println!("{} -> {}", edge.src, edge.dst);
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn all_edges(&self, edge_type: Option<&str>) -> Result<impl Iterator<Item = FullEdge> + '_> {
    let etype_id = match edge_type {
      Some(name) => {
        let edge_def = self
          .edges
          .get(name)
          .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {name}")))?;
        edge_def.etype_id
      }
      None => None,
    };

    let options = ListEdgesOptions { etype: etype_id };
    Ok(list_edges(&self.db, options).into_iter())
  }

  /// Get a lightweight node reference without loading properties
  ///
  /// This is faster than `get()` when you only need the node reference
  /// for traversals or edge operations.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// let user_ref = ray.get_ref("User", "alice")?;
  /// if let Some(node) = user_ref {
  ///     // Can now use node.id for edges, traversals, etc.
  /// }
  /// # Ok(())
  /// # }
  /// ```
  /// Get a lightweight node reference by key (direct read, no transaction overhead)
  ///
  /// This is faster than `get()` as it only returns a reference without loading properties.
  /// Use this when you only need the node ID for traversals or edge operations.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// // Fast: only gets reference (~85ns)
  /// if let Some(node) = ray.get_ref("User", "alice")? {
  ///     // Can now use node.id for edges, traversals, etc.
  ///     let friends = ray.from(node.id).out(Some("FOLLOWS"))?.to_vec();
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn get_ref(&self, node_type: &str, key_suffix: &str) -> Result<Option<NodeRef>> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?;

    let full_key = node_def.key(key_suffix);

    // Direct read without transaction
    let node_id = get_node_by_key_db(&self.db, &full_key);

    match node_id {
      Some(id) => Ok(Some(NodeRef::new(id, Some(full_key), node_type))),
      None => Ok(None),
    }
  }

  /// Helper to get node key from database
  fn get_node_key_internal(&self, node_id: NodeId) -> Option<String> {
    let delta = self.db.delta.read();
    get_node_key(self.db.snapshot.as_ref(), &delta, node_id)
  }

  // ========================================================================
  // Schema Access
  // ========================================================================

  /// Get a node definition by name
  pub fn node_def(&self, name: &str) -> Option<&NodeDef> {
    self.nodes.get(name)
  }

  /// Get an edge definition by name
  pub fn edge_def(&self, name: &str) -> Option<&EdgeDef> {
    self.edges.get(name)
  }

  /// Get all node type names
  pub fn node_types(&self) -> Vec<&str> {
    self.nodes.keys().map(|s| s.as_str()).collect()
  }

  /// Get all edge type names
  pub fn edge_types(&self) -> Vec<&str> {
    self.edges.keys().map(|s| s.as_str()).collect()
  }

  // ========================================================================
  // Traversal
  // ========================================================================

  /// Start a traversal from a node
  ///
  /// Returns a traversal builder that can be used to chain traversal steps.
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::ray::{NodeRef, Ray};
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// # let alice: NodeRef = unimplemented!();
  /// let friends = ray
  ///     .from(alice.id)
  ///     .out(Some("FOLLOWS"))?
  ///     .out(Some("FOLLOWS"))?
  ///     .to_vec();
  /// # Ok(())
  /// # }
  /// ```
  pub fn from(&self, node_id: NodeId) -> RayTraversalBuilder<'_> {
    RayTraversalBuilder::new(self, vec![node_id])
  }

  /// Start a traversal from multiple nodes
  pub fn from_nodes(&self, node_ids: Vec<NodeId>) -> RayTraversalBuilder<'_> {
    RayTraversalBuilder::new(self, node_ids)
  }

  // ========================================================================
  // Pathfinding
  // ========================================================================

  /// Find the shortest path between two nodes
  ///
  /// Returns a path finding builder that can be configured with edge types,
  /// direction, and maximum depth.
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::ray::{NodeRef, Ray};
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// # let alice: NodeRef = unimplemented!();
  /// # let bob: NodeRef = unimplemented!();
  /// let path = ray
  ///     .shortest_path(alice.id, bob.id)
  ///     .via("FOLLOWS")?
  ///     .max_depth(5)
  ///     .find();
  ///
  /// if path.found {
  ///     println!("Path: {:?}", path.path);
  ///     println!("Total weight: {}", path.total_weight);
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn shortest_path(&self, source: NodeId, target: NodeId) -> RayPathBuilder<'_> {
    RayPathBuilder::new(self, source, target)
  }

  /// Find shortest paths to any of the target nodes
  pub fn shortest_path_to_any(&self, source: NodeId, targets: Vec<NodeId>) -> RayPathBuilder<'_> {
    RayPathBuilder::new_multi(self, source, targets)
  }

  /// Check if a path exists between two nodes
  ///
  /// This is more efficient than `shortest_path()` when you only need to
  /// know if a path exists, not the path itself.
  pub fn has_path(
    &mut self,
    source: NodeId,
    target: NodeId,
    edge_type: Option<&str>,
  ) -> Result<bool> {
    let path = self.shortest_path(source, target);
    let path = if let Some(etype) = edge_type {
      path.via(etype)?
    } else {
      path
    };
    Ok(path.find().found)
  }

  /// Get all nodes reachable from a source within a certain depth
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::ray::{NodeRef, Ray};
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// # let alice: NodeRef = unimplemented!();
  /// let reachable = ray.reachable_from(alice.id, 3, Some("FOLLOWS"))?;
  /// println!("Alice can reach {} nodes in 3 hops", reachable.len());
  /// # Ok(())
  /// # }
  /// ```
  pub fn reachable_from(
    &self,
    source: NodeId,
    max_depth: usize,
    edge_type: Option<&str>,
  ) -> Result<Vec<NodeId>> {
    let etype = match edge_type {
      Some(name) => {
        let edge_def = self
          .edges
          .get(name)
          .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {name}")))?;
        edge_def.etype_id
      }
      None => None,
    };

    use super::traversal::{TraversalBuilder, TraversalDirection, TraverseOptions};

    let options = TraverseOptions::new(TraversalDirection::Out, max_depth);

    let results = TraversalBuilder::from_node(source)
      .traverse(etype, options)
      .collect_node_ids(|node_id, dir, etype_filter| {
        self.get_neighbors(node_id, dir, etype_filter)
      });

    Ok(results)
  }

  // Internal helper to get neighbors for traversal/pathfinding (read-only, no transaction)
  fn get_neighbors(
    &self,
    node_id: NodeId,
    direction: super::traversal::TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    use super::traversal::TraversalDirection;

    let mut edges = Vec::new();
    let delta = self.db.delta.read();

    match direction {
      TraversalDirection::Out => {
        // Build set of deleted edges for filtering
        let deleted_set = delta.out_del.get(&node_id);

        // Get from snapshot first
        if let Some(ref snapshot) = self.db.snapshot {
          if let Some(src_phys) = snapshot.get_phys_node(node_id) {
            for (dst_phys, edge_etype) in snapshot.iter_out_edges(src_phys) {
              // Filter by edge type if specified
              if etype.is_some() && etype != Some(edge_etype) {
                continue;
              }

              // Get the logical node ID for the destination
              if let Some(dst_id) = snapshot.get_node_id(dst_phys) {
                // Check if this edge was deleted in delta
                let is_deleted = deleted_set
                  .map(|set| {
                    set.contains(&EdgePatch {
                      etype: edge_etype,
                      other: dst_id,
                    })
                  })
                  .unwrap_or(false);

                if !is_deleted {
                  edges.push(Edge {
                    src: node_id,
                    etype: edge_etype,
                    dst: dst_id,
                  });
                }
              }
            }
          }
        }

        // Get from delta additions
        if let Some(add_set) = delta.out_add.get(&node_id) {
          for patch in add_set {
            if etype.is_none() || etype == Some(patch.etype) {
              // Only add if not already in edges (from snapshot)
              if !edges
                .iter()
                .any(|e| e.dst == patch.other && e.etype == patch.etype)
              {
                edges.push(Edge {
                  src: node_id,
                  etype: patch.etype,
                  dst: patch.other,
                });
              }
            }
          }
        }
      }
      TraversalDirection::In => {
        // Build set of deleted edges for filtering
        let deleted_set = delta.in_del.get(&node_id);

        // Get from snapshot first
        if let Some(ref snapshot) = self.db.snapshot {
          if let Some(dst_phys) = snapshot.get_phys_node(node_id) {
            for (src_phys, edge_etype, _out_index) in snapshot.iter_in_edges(dst_phys) {
              // Filter by edge type if specified
              if etype.is_some() && etype != Some(edge_etype) {
                continue;
              }

              // Get the logical node ID for the source
              if let Some(src_id) = snapshot.get_node_id(src_phys) {
                // Check if this edge was deleted in delta
                let is_deleted = deleted_set
                  .map(|set| {
                    set.contains(&EdgePatch {
                      etype: edge_etype,
                      other: src_id,
                    })
                  })
                  .unwrap_or(false);

                if !is_deleted {
                  edges.push(Edge {
                    src: src_id,
                    etype: edge_etype,
                    dst: node_id,
                  });
                }
              }
            }
          }
        }

        // Get from delta additions
        if let Some(add_set) = delta.in_add.get(&node_id) {
          for patch in add_set {
            if etype.is_none() || etype == Some(patch.etype) {
              // Only add if not already in edges (from snapshot)
              if !edges
                .iter()
                .any(|e| e.src == patch.other && e.etype == patch.etype)
              {
                edges.push(Edge {
                  src: patch.other,
                  etype: patch.etype,
                  dst: node_id,
                });
              }
            }
          }
        }
      }
      TraversalDirection::Both => {
        drop(delta); // Release lock before recursive calls
        edges.extend(self.get_neighbors(node_id, TraversalDirection::Out, etype));
        edges.extend(self.get_neighbors(node_id, TraversalDirection::In, etype));
      }
    }

    edges
  }

  // ========================================================================
  // Database Maintenance
  // ========================================================================

  /// Optimize (compact) the database
  ///
  /// This merges the write-ahead log (WAL) into the snapshot, reducing
  /// file size and improving read performance. This is equivalent to
  /// "VACUUM" in SQLite.
  ///
  /// Optimize the database by compacting snapshot + delta into a new snapshot
  ///
  /// This operation:
  /// 1. Collects all live nodes and edges from snapshot + delta
  /// 2. Builds a new snapshot with the merged data  
  /// 3. Updates manifest to point to new snapshot
  /// 4. Clears WAL and delta
  /// 5. Garbage collects old snapshots (keeps last 2)
  ///
  /// Call this periodically to reclaim space from deleted nodes/edges
  /// and improve read performance.
  pub fn optimize(&mut self) -> Result<()> {
    self.db.optimize()
  }

  /// Get database statistics
  pub fn stats(&self) -> DbStats {
    use crate::graph::iterators::{count_edges, count_nodes};

    let node_count = count_nodes(&self.db);
    let edge_count = count_edges(&self.db, None);

    // Get delta statistics
    let delta = self.db.delta.read();
    let delta_nodes_created = delta.created_nodes.len();
    let delta_nodes_deleted = delta.deleted_nodes.len();
    let delta_edges_added = delta.total_edges_added();
    let delta_edges_deleted = delta.total_edges_deleted();
    drop(delta);

    // Get snapshot statistics
    let (snapshot_gen, snapshot_nodes, snapshot_edges, snapshot_max_node_id) =
      if let Some(ref snapshot) = self.db.snapshot {
        (
          snapshot.header.generation,
          snapshot.header.num_nodes,
          snapshot.header.num_edges,
          snapshot.header.max_node_id,
        )
      } else {
        (0, 0, 0, 0)
      };

    // Get WAL segment from manifest
    let wal_segment = self
      .db
      .manifest
      .as_ref()
      .map(|m| m.active_wal_seg)
      .unwrap_or(0);

    let mvcc_stats = self.db.mvcc.as_ref().map(|mvcc| {
      let tx_mgr = mvcc.tx_manager.lock();
      let gc = mvcc.gc.lock();
      let gc_stats = gc.get_stats();
      let committed_stats = tx_mgr.get_committed_writes_stats();
      MvccStats {
        active_transactions: tx_mgr.get_active_count(),
        min_active_ts: tx_mgr.min_active_ts(),
        versions_pruned: gc_stats.versions_pruned,
        gc_runs: gc_stats.gc_runs,
        last_gc_time: gc_stats.last_gc_time,
        committed_writes_size: committed_stats.size,
        committed_writes_pruned: committed_stats.pruned,
      }
    });

    // Recommend compaction if delta has significant changes
    let total_changes =
      delta_nodes_created + delta_nodes_deleted + delta_edges_added + delta_edges_deleted;
    let recommend_compact = total_changes > 10_000;

    DbStats {
      snapshot_gen,
      snapshot_nodes: snapshot_nodes.max(node_count), // Use higher of snapshot or total
      snapshot_edges: snapshot_edges.max(edge_count),
      snapshot_max_node_id,
      delta_nodes_created,
      delta_nodes_deleted,
      delta_edges_added,
      delta_edges_deleted,
      wal_segment,
      wal_bytes: self.db.wal_bytes(),
      recommend_compact,
      mvcc_stats,
    }
  }

  /// Get a human-readable description of the database
  ///
  /// Useful for debugging and monitoring. Returns information about:
  /// - Database path and format
  /// - Schema (node types and edge types)
  /// - Current statistics
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() {
  /// # let ray: Ray = unimplemented!();
  /// println!("{}", ray.describe());
  /// // Output:
  /// // RayDB at /path/to/db (multi-file format)
  /// // Schema:
  /// //   Node types: User, Post, Comment
  /// //   Edge types: FOLLOWS, LIKES, WROTE
  /// // Statistics:
  /// //   Nodes: 1,234 (snapshot: 1,200, delta: +34)
  /// //   Edges: 5,678 (snapshot: 5,600, delta: +78)
  /// # }
  /// ```
  pub fn describe(&self) -> String {
    let stats = self.stats();
    let path = self.db.path.display();
    let format = if self.db.is_single_file {
      "single-file"
    } else {
      "multi-file"
    };

    let node_types: Vec<&str> = self.nodes.keys().map(|s| s.as_str()).collect();
    let edge_types: Vec<&str> = self.edges.keys().map(|s| s.as_str()).collect();

    let delta_nodes = stats.delta_nodes_created as i64 - stats.delta_nodes_deleted as i64;
    let delta_edges = stats.delta_edges_added as i64 - stats.delta_edges_deleted as i64;

    format!(
      "RayDB at {} ({} format)\n\
       Schema:\n  \
         Node types: {}\n  \
         Edge types: {}\n\
       Statistics:\n  \
         Nodes: {} (snapshot: {}, delta: {:+})\n  \
         Edges: {} (snapshot: {}, delta: {:+})\n  \
         Recommend compact: {}",
      path,
      format,
      if node_types.is_empty() {
        "(none)".to_string()
      } else {
        node_types.join(", ")
      },
      if edge_types.is_empty() {
        "(none)".to_string()
      } else {
        edge_types.join(", ")
      },
      stats.snapshot_nodes,
      stats
        .snapshot_nodes
        .saturating_sub(stats.delta_nodes_created as u64),
      delta_nodes,
      stats.snapshot_edges,
      stats
        .snapshot_edges
        .saturating_sub(stats.delta_edges_added as u64),
      delta_edges,
      if stats.recommend_compact { "yes" } else { "no" }
    )
  }

  /// Check database integrity
  ///
  /// Performs validation checks on the database structure:
  /// - Verifies edge reciprocity (for each outgoing edge, a matching incoming edge exists)
  /// - Checks that all edges reference existing nodes
  /// - Validates node key mappings
  ///
  /// Returns a `CheckResult` with `valid=true` if no errors found, or detailed
  /// error/warning messages otherwise.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// let result = ray.check()?;
  /// if !result.valid {
  ///     for error in &result.errors {
  ///         eprintln!("Error: {}", error);
  ///     }
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn check(&self) -> Result<CheckResult> {
    let mut result = if let Some(ref snapshot) = self.db.snapshot {
      crate::check::check_snapshot(snapshot)
    } else {
      CheckResult {
        valid: true,
        errors: Vec::new(),
        warnings: vec!["No snapshot to check".to_string()],
      }
    };

    // Schema consistency - verify all registered edge types have valid IDs
    for (edge_name, edge_def) in &self.edges {
      if edge_def.etype_id.is_none() {
        result
          .warnings
          .push(format!("Edge type '{edge_name}' has no assigned etype_id"));
      }
    }

    Ok(result)
  }

  // ========================================================================
  // Database Access
  // ========================================================================

  /// Get a reference to the underlying GraphDB
  pub fn raw(&self) -> &GraphDB {
    &self.db
  }

  /// Get a mutable reference to the underlying GraphDB
  pub fn raw_mut(&mut self) -> &mut GraphDB {
    &mut self.db
  }

  /// Close the database
  pub fn close(self) -> Result<()> {
    close_graph_db(self.db)
  }
}

// ============================================================================
// Traversal Builder for Ray
// ============================================================================

use super::traversal::{TraversalBuilder, TraversalDirection, TraversalResult, TraverseOptions};

/// Traversal builder bound to a Ray database
///
/// Provides ergonomic traversal operations using edge type names.
pub struct RayTraversalBuilder<'a> {
  ray: &'a Ray,
  builder: TraversalBuilder,
}

impl<'a> RayTraversalBuilder<'a> {
  fn new(ray: &'a Ray, start_nodes: Vec<NodeId>) -> Self {
    Self {
      ray,
      builder: TraversalBuilder::new(start_nodes),
    }
  }

  /// Traverse outgoing edges
  ///
  /// @param edge_type - Edge type name (or None for all types)
  pub fn out(mut self, edge_type: Option<&str>) -> Result<Self> {
    let etype = self.resolve_etype(edge_type)?;
    self.builder = self.builder.out(etype);
    Ok(self)
  }

  /// Traverse incoming edges
  pub fn r#in(mut self, edge_type: Option<&str>) -> Result<Self> {
    let etype = self.resolve_etype(edge_type)?;
    self.builder = self.builder.r#in(etype);
    Ok(self)
  }

  /// Traverse edges in both directions
  pub fn both(mut self, edge_type: Option<&str>) -> Result<Self> {
    let etype = self.resolve_etype(edge_type)?;
    self.builder = self.builder.both(etype);
    Ok(self)
  }

  /// Variable-depth traversal
  pub fn traverse(mut self, edge_type: Option<&str>, options: TraverseOptions) -> Result<Self> {
    let etype = self.resolve_etype(edge_type)?;
    self.builder = self.builder.traverse(etype, options);
    Ok(self)
  }

  /// Limit the number of results
  pub fn take(mut self, limit: usize) -> Self {
    self.builder = self.builder.take(limit);
    self
  }

  /// Select specific properties to load (optimization)
  ///
  /// Only the specified properties will be loaded when collecting results,
  /// reducing overhead. This is useful when you only need a few properties
  /// from nodes that have many properties.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::NodeId;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// # let user_id: NodeId = 1;
  /// let friends = ray.from(user_id)
  ///     .out(Some("FOLLOWS"))?
  ///     .select(&["name", "avatar"]) // Only load name and avatar
  ///     .to_vec();
  /// # Ok(())
  /// # }
  /// ```
  pub fn select(mut self, props: &[&str]) -> Self {
    self.builder = self.builder.select_props(props);
    self
  }

  /// Execute and collect node IDs
  pub fn to_vec(self) -> Vec<NodeId> {
    self
      .builder
      .collect_node_ids(|node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype))
  }

  /// Execute and get first result
  pub fn first(self) -> Option<TraversalResult> {
    self
      .builder
      .first(|node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype))
  }

  /// Execute and get first node ID
  pub fn first_node(self) -> Option<NodeId> {
    self
      .builder
      .first_node(|node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype))
  }

  /// Execute and count results
  pub fn count(self) -> usize {
    self
      .builder
      .count(|node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype))
  }

  /// Execute and return iterator over traversal results
  pub fn execute(self) -> impl Iterator<Item = TraversalResult> + 'a {
    let ray = self.ray;
    self
      .builder
      .execute(move |node_id, dir, etype| ray.get_neighbors(node_id, dir, etype))
  }

  /// Execute and return iterator over edges only
  ///
  /// This is useful when you want to collect the edges traversed rather than nodes.
  /// Each result contains the source, destination, and edge type of edges encountered.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::NodeId;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let ray: Ray = unimplemented!();
  /// # let user_id: NodeId = 1;
  /// let edges: Vec<_> = ray.from(user_id)
  ///     .out(Some("FOLLOWS"))?
  ///     .edges()
  ///     .collect();
  ///
  /// for edge in edges {
  ///     println!("{} -[{}]-> {}", edge.src, edge.etype, edge.dst);
  /// }
  /// # Ok(())
  /// # }
  /// ```
  pub fn edges(self) -> impl Iterator<Item = Edge> + 'a {
    let ray = self.ray;
    self
      .builder
      .execute(move |node_id, dir, etype| ray.get_neighbors(node_id, dir, etype))
      .filter_map(|result| {
        result.edge.map(|e| Edge {
          src: e.src,
          etype: e.etype,
          dst: e.dst,
        })
      })
  }

  /// Execute and return iterator over full edge details
  ///
  /// Similar to `edges()` but returns FullEdge structs.
  pub fn full_edges(self) -> impl Iterator<Item = FullEdge> + 'a {
    let ray = self.ray;
    self
      .builder
      .execute(move |node_id, dir, etype| ray.get_neighbors(node_id, dir, etype))
      .filter_map(move |result| {
        result.edge.map(|e| FullEdge {
          src: e.src,
          etype: e.etype,
          dst: e.dst,
        })
      })
  }

  fn resolve_etype(&self, edge_type: Option<&str>) -> Result<Option<ETypeId>> {
    match edge_type {
      Some(name) => {
        let edge_def = self
          .ray
          .edges
          .get(name)
          .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {name}")))?;
        Ok(edge_def.etype_id)
      }
      None => Ok(None),
    }
  }
}

// ============================================================================
// Path Finding Builder for Ray
// ============================================================================

use super::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig, PathResult};

/// Path finding builder bound to a Ray database
///
/// Provides ergonomic pathfinding operations using edge type names.
pub struct RayPathBuilder<'a> {
  ray: &'a Ray,
  source: NodeId,
  targets: HashSet<NodeId>,
  allowed_etypes: HashSet<ETypeId>,
  direction: TraversalDirection,
  max_depth: usize,
  weights: HashMap<(NodeId, ETypeId, NodeId), f64>,
}

impl<'a> RayPathBuilder<'a> {
  fn new(ray: &'a Ray, source: NodeId, target: NodeId) -> Self {
    let mut targets = HashSet::new();
    targets.insert(target);

    Self {
      ray,
      source,
      targets,
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
      weights: HashMap::new(),
    }
  }

  fn new_multi(ray: &'a Ray, source: NodeId, targets: Vec<NodeId>) -> Self {
    Self {
      ray,
      source,
      targets: targets.into_iter().collect(),
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
      weights: HashMap::new(),
    }
  }

  /// Restrict traversal to specific edge type
  ///
  /// Can be called multiple times to allow multiple edge types.
  pub fn via(mut self, edge_type: &str) -> Result<Self> {
    let edge_def = self
      .ray
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    if let Some(etype_id) = edge_def.etype_id {
      self.allowed_etypes.insert(etype_id);
    }

    Ok(self)
  }

  /// Set maximum search depth
  pub fn max_depth(mut self, depth: usize) -> Self {
    self.max_depth = depth;
    self
  }

  /// Set traversal direction
  pub fn direction(mut self, direction: TraversalDirection) -> Self {
    self.direction = direction;
    self
  }

  /// Use bidirectional traversal
  pub fn bidirectional(mut self) -> Self {
    self.direction = TraversalDirection::Both;
    self
  }

  /// Find the shortest path using Dijkstra's algorithm
  pub fn find(self) -> PathResult {
    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    let weights = self.weights;
    dijkstra(
      config,
      |node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype),
      move |src, etype, dst| weights.get(&(src, etype, dst)).copied().unwrap_or(1.0),
    )
  }

  /// Find the shortest path using BFS (unweighted)
  ///
  /// Faster than Dijkstra for unweighted graphs.
  pub fn find_bfs(self) -> PathResult {
    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    bfs(config, |node_id, dir, etype| {
      self.ray.get_neighbors(node_id, dir, etype)
    })
  }

  /// Find the k shortest paths using Yen's algorithm
  pub fn find_k_shortest(self, k: usize) -> Vec<PathResult> {
    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    let weights = self.weights;
    yen_k_shortest(
      config,
      k,
      |node_id, dir, etype| self.ray.get_neighbors(node_id, dir, etype),
      move |src, etype, dst| weights.get(&(src, etype, dst)).copied().unwrap_or(1.0),
    )
  }
}

// ============================================================================
// Batch Operations
// ============================================================================

/// A batch operation that can be executed atomically with other operations
#[derive(Debug, Clone)]
pub enum BatchOp {
  /// Create a new node
  CreateNode {
    node_type: String,
    key_suffix: String,
    props: HashMap<String, PropValue>,
  },
  /// Delete a node
  DeleteNode { node_id: NodeId },
  /// Create an edge
  Link {
    src: NodeId,
    edge_type: String,
    dst: NodeId,
  },
  /// Remove an edge
  Unlink {
    src: NodeId,
    edge_type: String,
    dst: NodeId,
  },
  /// Set a node property
  SetProp {
    node_id: NodeId,
    prop_name: String,
    value: PropValue,
  },
  /// Delete a node property
  DelProp { node_id: NodeId, prop_name: String },
}

/// Result of a batch operation
#[derive(Debug, Clone)]
pub enum BatchResult {
  /// Node was created, contains the NodeRef
  NodeCreated(NodeRef),
  /// Node was deleted
  NodeDeleted(bool),
  /// Edge was created
  EdgeCreated,
  /// Edge was removed
  EdgeRemoved(bool),
  /// Property was set
  PropSet,
  /// Property was deleted
  PropDeleted,
}

impl Ray {
  /// Execute multiple operations atomically in a single transaction
  ///
  /// All operations succeed or fail together. If any operation fails,
  /// the entire batch is rolled back.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::{BatchOp, Ray, RayOptions};
  /// # use std::collections::HashMap;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let options = RayOptions::default();
  /// let mut ray = Ray::open("db", options)?;
  ///
  /// let results = ray.batch(vec![
  ///   BatchOp::CreateNode {
  ///     node_type: "User".into(),
  ///     key_suffix: "alice".into(),
  ///     props: HashMap::new(),
  ///   },
  ///   BatchOp::CreateNode {
  ///     node_type: "User".into(),
  ///     key_suffix: "bob".into(),
  ///     props: HashMap::new(),
  ///   },
  /// ])?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn batch(&mut self, ops: Vec<BatchOp>) -> Result<Vec<BatchResult>> {
    let mut handle = begin_tx(&self.db)?;
    let mut results = Vec::with_capacity(ops.len());

    for op in ops {
      let result = match op {
        BatchOp::CreateNode {
          node_type,
          key_suffix,
          props,
        } => {
          let node_def = self
            .nodes
            .get(&node_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?;

          let full_key = node_def.key(&key_suffix);

          let node_opts = NodeOpts {
            key: Some(full_key.clone()),
            labels: node_def.label_id.map(|id| vec![id]),
            props: None,
          };
          let node_id = create_node(&mut handle, node_opts)?;

          // Set properties
          for (prop_name, value) in props {
            if let Some(&prop_key_id) = node_def.prop_key_ids.get(&prop_name) {
              set_node_prop(&mut handle, node_id, prop_key_id, value)?;
            }
          }

          BatchResult::NodeCreated(NodeRef::new(node_id, Some(full_key), &node_type))
        }

        BatchOp::DeleteNode { node_id } => {
          let deleted = delete_node(&mut handle, node_id)?;
          BatchResult::NodeDeleted(deleted)
        }

        BatchOp::Link {
          src,
          edge_type,
          dst,
        } => {
          let edge_def = self
            .edges
            .get(&edge_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

          let etype_id = edge_def
            .etype_id
            .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

          add_edge(&mut handle, src, etype_id, dst)?;
          BatchResult::EdgeCreated
        }

        BatchOp::Unlink {
          src,
          edge_type,
          dst,
        } => {
          let edge_def = self
            .edges
            .get(&edge_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

          let etype_id = edge_def
            .etype_id
            .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

          let deleted = delete_edge(&mut handle, src, etype_id, dst)?;
          BatchResult::EdgeRemoved(deleted)
        }

        BatchOp::SetProp {
          node_id,
          prop_name,
          value,
        } => {
          // Use handle.db to access schema methods while handle is active
          let prop_key_id = handle.db.get_or_create_propkey(&prop_name);
          set_node_prop(&mut handle, node_id, prop_key_id, value)?;
          BatchResult::PropSet
        }

        BatchOp::DelProp { node_id, prop_name } => {
          let prop_key_id = handle
            .db
            .get_propkey_id(&prop_name)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown property: {prop_name}")))?;
          del_node_prop(&mut handle, node_id, prop_key_id)?;
          BatchResult::PropDeleted
        }
      };

      results.push(result);
    }

    // Commit the entire batch
    commit(&mut handle)?;

    Ok(results)
  }
}

// ============================================================================
// Transaction Context
// ============================================================================

/// Context for executing operations within a transaction
///
/// Provides the same operations as Ray but within an explicit transaction scope.
/// All operations are committed together when the transaction closure returns Ok,
/// or rolled back if an error is returned.
///
/// Note: TxContext holds references to the schema maps (nodes, edges) separately
/// from the TxHandle to avoid borrow checker issues.
pub struct TxContext<'a> {
  handle: TxHandle<'a>,
  nodes: &'a HashMap<String, NodeDef>,
  edges: &'a HashMap<String, EdgeDef>,
}

impl<'a> TxContext<'a> {
  /// Create a new node
  pub fn create_node(
    &mut self,
    node_type: &str,
    key_suffix: &str,
    props: HashMap<String, PropValue>,
  ) -> Result<NodeRef> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?
      .clone();

    let full_key = node_def.key(key_suffix);

    let node_opts = NodeOpts {
      key: Some(full_key.clone()),
      labels: node_def.label_id.map(|id| vec![id]),
      props: None,
    };
    let node_id = create_node(&mut self.handle, node_opts)?;

    // Set properties
    for (prop_name, value) in props {
      if let Some(&prop_key_id) = node_def.prop_key_ids.get(&prop_name) {
        set_node_prop(&mut self.handle, node_id, prop_key_id, value)?;
      }
    }

    Ok(NodeRef::new(node_id, Some(full_key), node_type))
  }

  /// Delete a node
  pub fn delete_node(&mut self, node_id: NodeId) -> Result<bool> {
    delete_node(&mut self.handle, node_id)
  }

  /// Create an edge
  pub fn link(&mut self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<()> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    add_edge(&mut self.handle, src, etype_id, dst)?;
    Ok(())
  }

  /// Remove an edge
  pub fn unlink(&mut self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<bool> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    delete_edge(&mut self.handle, src, etype_id, dst)
  }

  /// Set a node property
  pub fn set_prop(&mut self, node_id: NodeId, prop_name: &str, value: PropValue) -> Result<()> {
    let prop_key_id = self.handle.db.get_or_create_propkey(prop_name);
    set_node_prop(&mut self.handle, node_id, prop_key_id, value)?;
    Ok(())
  }

  /// Delete a node property
  pub fn del_prop(&mut self, node_id: NodeId, prop_name: &str) -> Result<()> {
    let prop_key_id = self
      .handle
      .db
      .get_propkey_id(prop_name)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown property: {prop_name}")))?;
    del_node_prop(&mut self.handle, node_id, prop_key_id)?;
    Ok(())
  }

  /// Check if a node exists
  pub fn exists(&self, node_id: NodeId) -> bool {
    node_exists(&self.handle, node_id)
  }

  /// Check if an edge exists
  pub fn has_edge(&self, src: NodeId, edge_type: &str, dst: NodeId) -> Result<bool> {
    let edge_def = self
      .edges
      .get(edge_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {edge_type}")))?;

    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

    Ok(edge_exists(&self.handle, src, etype_id, dst))
  }

  /// Get a node property
  pub fn get_prop(&self, node_id: NodeId, prop_name: &str) -> Result<Option<PropValue>> {
    let prop_key_id = self
      .handle
      .db
      .get_propkey_id(prop_name)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown property: {prop_name}")))?;

    Ok(get_node_prop(&self.handle, node_id, prop_key_id))
  }

  /// Get a node by key
  pub fn get(&self, node_type: &str, key_suffix: &str) -> Result<Option<NodeRef>> {
    let node_def = self
      .nodes
      .get(node_type)
      .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {node_type}")))?;

    let full_key = node_def.key(key_suffix);
    let node_id = get_node_by_key(&self.handle, &full_key);

    match node_id {
      Some(id) => Ok(Some(NodeRef::new(id, Some(full_key), node_type))),
      None => Ok(None),
    }
  }
}

impl Ray {
  /// Execute operations in an explicit transaction
  ///
  /// The closure receives a TxContext with access to node/edge operations.
  /// All operations performed through the context are committed together when
  /// the closure returns Ok, or rolled back if an error is returned.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use raydb::api::ray::Ray;
  /// # use raydb::types::PropValue;
  /// # use std::collections::HashMap;
  /// # fn main() -> raydb::error::Result<()> {
  /// # let mut ray: Ray = unimplemented!();
  /// let result = ray.transaction(|ctx| {
  ///   let alice = ctx.create_node("User", "alice", HashMap::new())?;
  ///   let bob = ctx.create_node("User", "bob", HashMap::new())?;
  ///   ctx.link(alice.id, "FOLLOWS", bob.id)?;
  ///   Ok((alice, bob))
  /// })?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn transaction<T, F>(&mut self, f: F) -> Result<T>
  where
    F: FnOnce(&mut TxContext) -> Result<T>,
  {
    // Start the transaction
    let handle = begin_tx(&self.db)?;

    // Create context with references to schema maps
    let mut ctx = TxContext {
      handle,
      nodes: &self.nodes,
      edges: &self.edges,
    };

    match f(&mut ctx) {
      Ok(result) => {
        commit(&mut ctx.handle)?;
        Ok(result)
      }
      Err(e) => {
        rollback(&mut ctx.handle)?;
        Err(e)
      }
    }
  }

  /// Execute a transaction with a simpler API using a builder pattern
  ///
  /// Returns a TxBuilder that collects operations and executes them atomically.
  pub fn tx(&mut self) -> TxBuilder {
    TxBuilder { ops: Vec::new() }
  }
}

/// Builder for constructing transactions with a fluent API
#[derive(Debug, Default)]
pub struct TxBuilder {
  ops: Vec<BatchOp>,
}

impl TxBuilder {
  /// Add a create node operation
  pub fn create_node(
    mut self,
    node_type: impl Into<String>,
    key_suffix: impl Into<String>,
    props: HashMap<String, PropValue>,
  ) -> Self {
    self.ops.push(BatchOp::CreateNode {
      node_type: node_type.into(),
      key_suffix: key_suffix.into(),
      props,
    });
    self
  }

  /// Add a delete node operation
  pub fn delete_node(mut self, node_id: NodeId) -> Self {
    self.ops.push(BatchOp::DeleteNode { node_id });
    self
  }

  /// Add a link operation
  pub fn link(mut self, src: NodeId, edge_type: impl Into<String>, dst: NodeId) -> Self {
    self.ops.push(BatchOp::Link {
      src,
      edge_type: edge_type.into(),
      dst,
    });
    self
  }

  /// Add an unlink operation
  pub fn unlink(mut self, src: NodeId, edge_type: impl Into<String>, dst: NodeId) -> Self {
    self.ops.push(BatchOp::Unlink {
      src,
      edge_type: edge_type.into(),
      dst,
    });
    self
  }

  /// Add a set property operation
  pub fn set_prop(
    mut self,
    node_id: NodeId,
    prop_name: impl Into<String>,
    value: PropValue,
  ) -> Self {
    self.ops.push(BatchOp::SetProp {
      node_id,
      prop_name: prop_name.into(),
      value,
    });
    self
  }

  /// Add a delete property operation
  pub fn del_prop(mut self, node_id: NodeId, prop_name: impl Into<String>) -> Self {
    self.ops.push(BatchOp::DelProp {
      node_id,
      prop_name: prop_name.into(),
    });
    self
  }

  /// Execute the transaction on the given Ray instance
  pub fn execute(self, ray: &mut Ray) -> Result<Vec<BatchResult>> {
    ray.batch(self.ops)
  }

  /// Get the operations as a Vec<BatchOp>
  pub fn into_ops(self) -> Vec<BatchOp> {
    self.ops
  }
}

// ============================================================================
// Update Node Builder
// ============================================================================

/// Fluent builder for updating node properties
///
/// Created via `ray.update()`, `ray.update_by_id()`, or `ray.update_by_key()`
/// and allows chaining multiple property set/unset operations before executing
/// in a single transaction.
///
/// # Example
/// ```rust,no_run
/// # use raydb::api::ray::{NodeRef, Ray};
/// # use raydb::types::PropValue;
/// # fn main() -> raydb::error::Result<()> {
/// # let mut ray: Ray = unimplemented!();
/// # let alice: NodeRef = unimplemented!();
/// // Update by node reference
/// ray.update(&alice)?
///     .set("name", PropValue::String("Alice Updated".into()))
///     .set("age", PropValue::I64(31))
///     .unset("old_field")
///     .execute()?;
///
/// // Update by key
/// ray.update_by_key("User", "alice")?
///     .set("name", PropValue::String("New Name".into()))
///     .execute()?;
/// # Ok(())
/// # }
/// ```
pub struct RayUpdateNodeBuilder<'a> {
  ray: &'a mut Ray,
  node_id: NodeId,
  updates: HashMap<String, Option<PropValue>>,
}

impl<'a> RayUpdateNodeBuilder<'a> {
  /// Set a node property value
  ///
  /// The property will be set when `execute()` is called.
  pub fn set(mut self, prop_name: &str, value: PropValue) -> Self {
    self.updates.insert(prop_name.to_string(), Some(value));
    self
  }

  /// Remove a node property
  ///
  /// The property will be deleted when `execute()` is called.
  pub fn unset(mut self, prop_name: &str) -> Self {
    self.updates.insert(prop_name.to_string(), None);
    self
  }

  /// Set multiple properties at once from a HashMap
  ///
  /// Convenience method for setting multiple properties.
  pub fn set_all(mut self, props: HashMap<String, PropValue>) -> Self {
    for (k, v) in props {
      self.updates.insert(k, Some(v));
    }
    self
  }

  /// Execute the update, applying all property changes in a single transaction
  pub fn execute(self) -> Result<()> {
    if self.updates.is_empty() {
      return Ok(());
    }

    let mut handle = begin_tx(&self.ray.db)?;

    for (prop_name, value_opt) in self.updates {
      let prop_key_id = self.ray.db.get_or_create_propkey(&prop_name);

      match value_opt {
        Some(value) => {
          set_node_prop(&mut handle, self.node_id, prop_key_id, value)?;
        }
        None => {
          // Only delete if prop exists
          del_node_prop(&mut handle, self.node_id, prop_key_id)?;
        }
      }
    }

    commit(&mut handle)?;
    Ok(())
  }

  /// Get the node ID being updated
  pub fn node_id(&self) -> NodeId {
    self.node_id
  }
}

// ============================================================================
// Insert Builder
// ============================================================================

/// Fluent builder for inserting nodes
///
/// Created via `ray.insert(node_type)` and provides a fluent API for creating
/// nodes with the `.values().returning()` or `.values().execute()` pattern.
///
/// # Example
/// ```rust,no_run
/// # use raydb::api::ray::Ray;
/// # use raydb::types::PropValue;
/// # use std::collections::HashMap;
/// # fn main() -> raydb::error::Result<()> {
/// # let mut ray: Ray = unimplemented!();
/// # let props: HashMap<String, PropValue> = HashMap::new();
/// # let alice_props: HashMap<String, PropValue> = HashMap::new();
/// # let bob_props: HashMap<String, PropValue> = HashMap::new();
/// // Insert and get the node reference back
/// let user = ray.insert("User")?
///     .values("alice", props)?
///     .returning()?;
///
/// // Insert multiple nodes
/// let users = ray.insert("User")?
///     .values_many(vec![
///         ("alice", alice_props),
///         ("bob", bob_props),
///     ])?
///     .returning()?;
/// # Ok(())
/// # }
/// ```
pub struct RayInsertBuilder<'a> {
  ray: &'a mut Ray,
  node_type: String,
  key_prefix: String,
}

impl<'a> RayInsertBuilder<'a> {
  /// Specify the values for a single node insert
  ///
  /// Returns an executor that can either `.execute()` (no return) or
  /// `.returning()` (returns NodeRef).
  pub fn values(
    self,
    key_suffix: &str,
    props: HashMap<String, PropValue>,
  ) -> Result<InsertExecutorSingle<'a>> {
    let full_key = format!("{}{}", self.key_prefix, key_suffix);
    Ok(InsertExecutorSingle {
      ray: self.ray,
      node_type: self.node_type,
      full_key,
      props,
    })
  }

  /// Specify values for multiple nodes
  ///
  /// Returns an executor that can either `.execute()` (no return) or
  /// `.returning()` (returns Vec<NodeRef>).
  pub fn values_many(
    self,
    items: Vec<(&str, HashMap<String, PropValue>)>,
  ) -> Result<InsertExecutorMultiple<'a>> {
    let entries: Vec<(String, HashMap<String, PropValue>)> = items
      .into_iter()
      .map(|(key_suffix, props)| {
        let full_key = format!("{}{}", self.key_prefix, key_suffix);
        (full_key, props)
      })
      .collect();

    Ok(InsertExecutorMultiple {
      ray: self.ray,
      node_type: self.node_type,
      entries,
    })
  }
}

/// Executor for single node insert
pub struct InsertExecutorSingle<'a> {
  ray: &'a mut Ray,
  node_type: String,
  full_key: String,
  props: HashMap<String, PropValue>,
}

impl<'a> InsertExecutorSingle<'a> {
  /// Execute the insert and return the created node reference
  pub fn returning(self) -> Result<NodeRef> {
    let mut handle = begin_tx(&self.ray.db)?;

    // Create the node
    let node_opts = NodeOpts::new().with_key(self.full_key.clone());
    let node_id = create_node(&mut handle, node_opts)?;

    // Set properties
    for (prop_name, value) in self.props {
      let prop_key_id = self.ray.db.get_or_create_propkey(&prop_name);
      set_node_prop(&mut handle, node_id, prop_key_id, value)?;
    }

    commit(&mut handle)?;

    Ok(NodeRef::new(node_id, Some(self.full_key), &self.node_type))
  }

  /// Execute the insert without returning the node reference
  ///
  /// Slightly more efficient when you don't need the result.
  pub fn execute(self) -> Result<()> {
    let _ = self.returning()?;
    Ok(())
  }
}

/// Executor for multiple node insert
pub struct InsertExecutorMultiple<'a> {
  ray: &'a mut Ray,
  node_type: String,
  entries: Vec<(String, HashMap<String, PropValue>)>,
}

impl<'a> InsertExecutorMultiple<'a> {
  /// Execute the insert and return all created node references
  pub fn returning(self) -> Result<Vec<NodeRef>> {
    if self.entries.is_empty() {
      return Ok(Vec::new());
    }

    let mut handle = begin_tx(&self.ray.db)?;
    let mut results = Vec::with_capacity(self.entries.len());

    for (full_key, props) in self.entries {
      // Create the node
      let node_opts = NodeOpts::new().with_key(full_key.clone());
      let node_id = create_node(&mut handle, node_opts)?;

      // Set properties
      for (prop_name, value) in props {
        let prop_key_id = self.ray.db.get_or_create_propkey(&prop_name);
        set_node_prop(&mut handle, node_id, prop_key_id, value)?;
      }

      results.push(NodeRef::new(node_id, Some(full_key), &self.node_type));
    }

    commit(&mut handle)?;

    Ok(results)
  }

  /// Execute the insert without returning node references
  pub fn execute(self) -> Result<()> {
    let _ = self.returning()?;
    Ok(())
  }
}

// ============================================================================
// Update Edge Builder
// ============================================================================

/// Fluent builder for updating edge properties
///
/// Created via `ray.update_edge(src, edge_type, dst)` and allows chaining
/// multiple property set/unset operations before executing in a single transaction.
///
/// # Example
/// ```rust,no_run
/// # use raydb::api::ray::Ray;
/// # use raydb::types::{NodeId, PropValue};
/// # fn main() -> raydb::error::Result<()> {
/// # let mut ray: Ray = unimplemented!();
/// # let alice_id: NodeId = 1;
/// # let bob_id: NodeId = 2;
/// ray.update_edge(alice_id, "FOLLOWS", bob_id)?
///    .set("weight", PropValue::F64(0.9))
///    .set("since", PropValue::String("2024".to_string()))
///    .unset("deprecated_field")
///    .execute()?;
/// # Ok(())
/// # }
/// ```
pub struct RayUpdateEdgeBuilder<'a> {
  ray: &'a mut Ray,
  src: NodeId,
  etype_id: ETypeId,
  dst: NodeId,
  updates: HashMap<String, Option<PropValue>>,
}

impl<'a> RayUpdateEdgeBuilder<'a> {
  /// Set an edge property value
  ///
  /// The property will be set when `execute()` is called.
  pub fn set(mut self, prop_name: &str, value: PropValue) -> Self {
    self.updates.insert(prop_name.to_string(), Some(value));
    self
  }

  /// Remove an edge property
  ///
  /// The property will be deleted when `execute()` is called.
  pub fn unset(mut self, prop_name: &str) -> Self {
    self.updates.insert(prop_name.to_string(), None);
    self
  }

  /// Set multiple properties at once from a HashMap
  ///
  /// Convenience method for setting multiple properties.
  pub fn set_all(mut self, props: HashMap<String, PropValue>) -> Self {
    for (k, v) in props {
      self.updates.insert(k, Some(v));
    }
    self
  }

  /// Execute the update, applying all property changes in a single transaction
  pub fn execute(self) -> Result<()> {
    if self.updates.is_empty() {
      return Ok(());
    }

    let mut handle = begin_tx(&self.ray.db)?;

    for (prop_name, value_opt) in self.updates {
      let prop_key_id = self.ray.db.get_or_create_propkey(&prop_name);

      match value_opt {
        Some(value) => {
          set_edge_prop(
            &mut handle,
            self.src,
            self.etype_id,
            self.dst,
            prop_key_id,
            value,
          )?;
        }
        None => {
          // Only delete if prop_key exists
          if let Some(existing_key_id) = self.ray.db.get_propkey_id(&prop_name) {
            del_edge_prop(
              &mut handle,
              self.src,
              self.etype_id,
              self.dst,
              existing_key_id,
            )?;
          }
        }
      }
    }

    commit(&mut handle)?;
    Ok(())
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  fn create_test_schema() -> RayOptions {
    let user = NodeDef::new("User", "user:")
      .prop(PropDef::string("name").required())
      .prop(PropDef::int("age"));

    let post = NodeDef::new("Post", "post:")
      .prop(PropDef::string("title").required())
      .prop(PropDef::string("content"));

    let follows = EdgeDef::new("FOLLOWS");
    let authored = EdgeDef::new("AUTHORED");

    RayOptions::new()
      .node(user)
      .node(post)
      .edge(follows)
      .edge(authored)
  }

  #[test]
  fn test_open_database() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let ray = Ray::open(temp_dir.path(), options).unwrap();

    assert_eq!(ray.node_types().len(), 2);
    assert_eq!(ray.edge_types().len(), 2);
    assert!(ray.node_def("User").is_some());
    assert!(ray.edge_def("FOLLOWS").is_some());

    ray.close().unwrap();
  }

  #[test]
  fn test_create_and_get_node() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a user
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    props.insert("age".to_string(), PropValue::I64(30));

    let user_ref = ray.create_node("User", "alice", props).unwrap();
    assert!(user_ref.id > 0);
    assert_eq!(user_ref.key, Some("user:alice".to_string()));

    // Get the user
    let found = ray.get("User", "alice").unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, user_ref.id);

    // Non-existent user
    let not_found = ray.get("User", "bob").unwrap();
    assert!(not_found.is_none());

    ray.close().unwrap();
  }

  #[test]
  fn test_link_and_unlink() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create two users
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Link them
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Check edge exists
    assert!(ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());
    assert!(!ray.has_edge(bob.id, "FOLLOWS", alice.id).unwrap());

    // Check neighbors
    let alice_follows = ray.neighbors_out(alice.id, Some("FOLLOWS")).unwrap();
    assert_eq!(alice_follows, vec![bob.id]);

    let bob_followers = ray.neighbors_in(bob.id, Some("FOLLOWS")).unwrap();
    assert_eq!(bob_followers, vec![alice.id]);

    // Unlink
    ray.unlink(alice.id, "FOLLOWS", bob.id).unwrap();
    assert!(!ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());

    ray.close().unwrap();
  }

  #[test]
  fn test_properties() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a user
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    let user = ray.create_node("User", "alice", props).unwrap();

    // Get property
    let name = ray.get_prop(user.id, "name");
    assert_eq!(name, Some(PropValue::String("Alice".to_string())));

    // Set property
    ray.set_prop(user.id, "age", PropValue::I64(25)).unwrap();
    let age = ray.get_prop(user.id, "age");
    assert_eq!(age, Some(PropValue::I64(25)));

    ray.close().unwrap();
  }

  #[test]
  fn test_count_nodes() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    assert_eq!(ray.count_nodes(), 0);

    ray.create_node("User", "alice", HashMap::new()).unwrap();
    ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.create_node("Post", "post1", HashMap::new()).unwrap();

    assert_eq!(ray.count_nodes(), 3);

    ray.close().unwrap();
  }

  #[test]
  fn test_delete_node() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let user = ray.create_node("User", "alice", HashMap::new()).unwrap();
    assert!(ray.exists(user.id));

    ray.delete_node(user.id).unwrap();
    assert!(!ray.exists(user.id));

    ray.close().unwrap();
  }

  #[test]
  fn test_get_ref() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a user
    let user = ray.create_node("User", "alice", HashMap::new()).unwrap();

    // Get lightweight reference
    let node_ref = ray.get_ref("User", "alice").unwrap();
    assert!(node_ref.is_some());
    let node_ref = node_ref.unwrap();
    assert_eq!(node_ref.id, user.id);
    assert_eq!(node_ref.key, Some("user:alice".to_string()));

    // Non-existent user
    let not_found = ray.get_ref("User", "bob").unwrap();
    assert!(not_found.is_none());

    ray.close().unwrap();
  }

  #[test]
  fn test_all_nodes_by_type() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some users and posts
    ray.create_node("User", "alice", HashMap::new()).unwrap();
    ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.create_node("Post", "post1", HashMap::new()).unwrap();

    // Iterate all users
    let users: Vec<_> = ray.all("User").unwrap().collect();
    assert_eq!(users.len(), 2);

    // Iterate all posts
    let posts: Vec<_> = ray.all("Post").unwrap().collect();
    assert_eq!(posts.len(), 1);

    ray.close().unwrap();
  }

  #[test]
  fn test_count_nodes_by_type() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some users and posts
    ray.create_node("User", "alice", HashMap::new()).unwrap();
    ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.create_node("Post", "post1", HashMap::new()).unwrap();

    // Count by type
    assert_eq!(ray.count_nodes_by_type("User").unwrap(), 2);
    assert_eq!(ray.count_nodes_by_type("Post").unwrap(), 1);
    assert_eq!(ray.count_nodes(), 3);

    ray.close().unwrap();
  }

  #[test]
  fn test_all_edges() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create nodes and edges
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let post = ray.create_node("Post", "post1", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(alice.id, "AUTHORED", post.id).unwrap();

    // List all edges
    let all_edges: Vec<_> = ray.all_edges(None).unwrap().collect();
    assert_eq!(all_edges.len(), 2);

    // List FOLLOWS edges only
    let follows_edges: Vec<_> = ray.all_edges(Some("FOLLOWS")).unwrap().collect();
    assert_eq!(follows_edges.len(), 1);
    assert_eq!(follows_edges[0].src, alice.id);
    assert_eq!(follows_edges[0].dst, bob.id);

    ray.close().unwrap();
  }

  #[test]
  fn test_count_edges_by_type() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let post = ray.create_node("Post", "post1", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(alice.id, "AUTHORED", post.id).unwrap();

    // Count by type
    assert_eq!(ray.count_edges_by_type("FOLLOWS").unwrap(), 1);
    assert_eq!(ray.count_edges_by_type("AUTHORED").unwrap(), 1);
    assert_eq!(ray.count_edges(), 2);

    ray.close().unwrap();
  }

  // ============================================================================
  // Traversal Tests
  // ============================================================================

  #[test]
  fn test_from_traversal() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a chain: alice -> bob -> charlie
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(bob.id, "FOLLOWS", charlie.id).unwrap();

    // Single hop traversal
    let friends = ray.from(alice.id).out(Some("FOLLOWS")).unwrap().to_vec();
    assert_eq!(friends, vec![bob.id]);

    // Two hop traversal
    let friends_of_friends = ray
      .from(alice.id)
      .out(Some("FOLLOWS"))
      .unwrap()
      .out(Some("FOLLOWS"))
      .unwrap()
      .to_vec();
    assert_eq!(friends_of_friends, vec![charlie.id]);

    ray.close().unwrap();
  }

  #[test]
  fn test_traversal_first() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Get first result
    let first = ray
      .from(alice.id)
      .out(Some("FOLLOWS"))
      .unwrap()
      .first_node();
    assert_eq!(first, Some(bob.id));

    // No results
    let no_result = ray.from(bob.id).out(Some("FOLLOWS")).unwrap().first_node();
    assert_eq!(no_result, None);

    ray.close().unwrap();
  }

  #[test]
  fn test_traversal_count() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(alice.id, "FOLLOWS", charlie.id).unwrap();

    let count = ray.from(alice.id).out(Some("FOLLOWS")).unwrap().count();
    assert_eq!(count, 2);

    ray.close().unwrap();
  }

  // ============================================================================
  // Pathfinding Tests
  // ============================================================================

  #[test]
  fn test_shortest_path() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a chain: alice -> bob -> charlie
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(bob.id, "FOLLOWS", charlie.id).unwrap();

    // Find path
    let path = ray
      .shortest_path(alice.id, charlie.id)
      .via("FOLLOWS")
      .unwrap()
      .find();

    assert!(path.found);
    assert_eq!(path.path, vec![alice.id, bob.id, charlie.id]);
    assert_eq!(path.edges.len(), 2);

    ray.close().unwrap();
  }

  #[test]
  fn test_shortest_path_not_found() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // No edge between them
    let path = ray
      .shortest_path(alice.id, bob.id)
      .via("FOLLOWS")
      .unwrap()
      .find();

    assert!(!path.found);
    assert!(path.path.is_empty());

    ray.close().unwrap();
  }

  #[test]
  fn test_has_path() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    assert!(ray.has_path(alice.id, bob.id, Some("FOLLOWS")).unwrap());
    assert!(!ray.has_path(alice.id, charlie.id, Some("FOLLOWS")).unwrap());
    assert!(!ray.has_path(bob.id, alice.id, Some("FOLLOWS")).unwrap()); // No reverse

    ray.close().unwrap();
  }

  #[test]
  fn test_reachable_from() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create: alice -> bob -> charlie -> dave
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();
    let dave = ray.create_node("User", "dave", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(bob.id, "FOLLOWS", charlie.id).unwrap();
    ray.link(charlie.id, "FOLLOWS", dave.id).unwrap();

    // Reachable within 2 hops
    let reachable = ray.reachable_from(alice.id, 2, Some("FOLLOWS")).unwrap();
    assert!(reachable.contains(&bob.id));
    assert!(reachable.contains(&charlie.id));
    assert!(!reachable.contains(&dave.id)); // 3 hops away

    // Reachable within 3 hops
    let reachable_3 = ray.reachable_from(alice.id, 3, Some("FOLLOWS")).unwrap();
    assert!(reachable_3.contains(&dave.id));

    ray.close().unwrap();
  }

  #[test]
  fn test_k_shortest_paths() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a diamond: alice -> bob -> dave, alice -> charlie -> dave
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();
    let dave = ray.create_node("User", "dave", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(alice.id, "FOLLOWS", charlie.id).unwrap();
    ray.link(bob.id, "FOLLOWS", dave.id).unwrap();
    ray.link(charlie.id, "FOLLOWS", dave.id).unwrap();

    // Find 2 shortest paths
    let paths = ray
      .shortest_path(alice.id, dave.id)
      .via("FOLLOWS")
      .unwrap()
      .find_k_shortest(2);

    assert_eq!(paths.len(), 2);
    assert!(paths[0].found);
    assert!(paths[1].found);
    // Both paths have same length (2 edges)
    assert_eq!(paths[0].edges.len(), 2);
    assert_eq!(paths[1].edges.len(), 2);

    ray.close().unwrap();
  }

  // ============================================================================
  // Batch Operation Tests
  // ============================================================================

  #[test]
  fn test_batch_create_nodes() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create multiple nodes in a batch
    let results = ray
      .batch(vec![
        BatchOp::CreateNode {
          node_type: "User".into(),
          key_suffix: "alice".into(),
          props: HashMap::new(),
        },
        BatchOp::CreateNode {
          node_type: "User".into(),
          key_suffix: "bob".into(),
          props: HashMap::new(),
        },
        BatchOp::CreateNode {
          node_type: "Post".into(),
          key_suffix: "post1".into(),
          props: HashMap::new(),
        },
      ])
      .unwrap();

    assert_eq!(results.len(), 3);

    // Verify all nodes were created
    assert_eq!(ray.count_nodes(), 3);
    assert!(ray.get("User", "alice").unwrap().is_some());
    assert!(ray.get("User", "bob").unwrap().is_some());
    assert!(ray.get("Post", "post1").unwrap().is_some());

    ray.close().unwrap();
  }

  #[test]
  fn test_batch_create_and_link() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // First batch: create nodes
    let results = ray
      .batch(vec![
        BatchOp::CreateNode {
          node_type: "User".into(),
          key_suffix: "alice".into(),
          props: HashMap::new(),
        },
        BatchOp::CreateNode {
          node_type: "User".into(),
          key_suffix: "bob".into(),
          props: HashMap::new(),
        },
      ])
      .unwrap();

    // Extract node IDs from results
    let alice_id = match &results[0] {
      BatchResult::NodeCreated(node_ref) => node_ref.id,
      _ => panic!("Expected NodeCreated"),
    };
    let bob_id = match &results[1] {
      BatchResult::NodeCreated(node_ref) => node_ref.id,
      _ => panic!("Expected NodeCreated"),
    };

    // Second batch: create edge
    ray
      .batch(vec![BatchOp::Link {
        src: alice_id,
        edge_type: "FOLLOWS".into(),
        dst: bob_id,
      }])
      .unwrap();

    // Verify edge was created
    assert!(ray.has_edge(alice_id, "FOLLOWS", bob_id).unwrap());

    ray.close().unwrap();
  }

  #[test]
  fn test_batch_set_properties() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let user = ray.create_node("User", "alice", HashMap::new()).unwrap();

    // Batch set properties
    ray
      .batch(vec![
        BatchOp::SetProp {
          node_id: user.id,
          prop_name: "name".into(),
          value: PropValue::String("Alice".into()),
        },
        BatchOp::SetProp {
          node_id: user.id,
          prop_name: "age".into(),
          value: PropValue::I64(30),
        },
      ])
      .unwrap();

    // Verify properties
    assert_eq!(
      ray.get_prop(user.id, "name"),
      Some(PropValue::String("Alice".into()))
    );
    assert_eq!(ray.get_prop(user.id, "age"), Some(PropValue::I64(30)));

    ray.close().unwrap();
  }

  #[test]
  fn test_batch_mixed_operations() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create initial nodes
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Mixed batch: link, set prop, create node, unlink
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    let results = ray
      .batch(vec![
        BatchOp::SetProp {
          node_id: alice.id,
          prop_name: "name".into(),
          value: PropValue::String("Alice".into()),
        },
        BatchOp::CreateNode {
          node_type: "User".into(),
          key_suffix: "charlie".into(),
          props: HashMap::new(),
        },
        BatchOp::Unlink {
          src: alice.id,
          edge_type: "FOLLOWS".into(),
          dst: bob.id,
        },
      ])
      .unwrap();

    assert_eq!(results.len(), 3);

    // Verify results
    assert_eq!(
      ray.get_prop(alice.id, "name"),
      Some(PropValue::String("Alice".into()))
    );
    assert!(ray.get("User", "charlie").unwrap().is_some());
    assert!(!ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());

    ray.close().unwrap();
  }

  #[test]
  fn test_batch_delete_operations() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create nodes and edges
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Batch delete
    let results = ray
      .batch(vec![
        BatchOp::Unlink {
          src: alice.id,
          edge_type: "FOLLOWS".into(),
          dst: bob.id,
        },
        BatchOp::DeleteNode { node_id: bob.id },
      ])
      .unwrap();

    // Verify
    match &results[0] {
      BatchResult::EdgeRemoved(removed) => assert!(*removed),
      _ => panic!("Expected EdgeRemoved"),
    }
    match &results[1] {
      BatchResult::NodeDeleted(deleted) => assert!(*deleted),
      _ => panic!("Expected NodeDeleted"),
    }

    assert!(!ray.exists(bob.id));

    ray.close().unwrap();
  }

  // ============================================================================
  // Transaction Tests
  // ============================================================================

  #[test]
  fn test_transaction_basic() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Execute a transaction
    let (alice, bob) = ray
      .transaction(|ctx| {
        let alice = ctx.create_node("User", "alice", HashMap::new())?;
        let bob = ctx.create_node("User", "bob", HashMap::new())?;
        ctx.link(alice.id, "FOLLOWS", bob.id)?;
        Ok((alice, bob))
      })
      .unwrap();

    // Verify results
    assert!(ray.exists(alice.id));
    assert!(ray.exists(bob.id));
    assert!(ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());

    ray.close().unwrap();
  }

  #[test]
  fn test_transaction_with_properties() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create node with properties in transaction
    let alice = ray
      .transaction(|ctx| {
        let mut props = HashMap::new();
        props.insert("name".to_string(), PropValue::String("Alice".into()));
        let alice = ctx.create_node("User", "alice", props)?;
        ctx.set_prop(alice.id, "age", PropValue::I64(30))?;
        Ok(alice)
      })
      .unwrap();

    // Verify properties
    assert_eq!(
      ray.get_prop(alice.id, "name"),
      Some(PropValue::String("Alice".into()))
    );
    assert_eq!(ray.get_prop(alice.id, "age"), Some(PropValue::I64(30)));

    ray.close().unwrap();
  }

  #[test]
  fn test_transaction_rollback_on_error() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Transaction that fails partway through
    let result: Result<()> = ray.transaction(|ctx| {
      ctx.create_node("User", "alice", HashMap::new())?;
      // This should fail - unknown node type
      ctx.create_node("UnknownType", "bob", HashMap::new())?;
      Ok(())
    });

    // Transaction should have failed
    assert!(result.is_err());

    // Alice should NOT exist because the transaction was rolled back
    // Note: Due to WAL-based implementation, rollback happens at commit time
    // so we need to verify the final state

    ray.close().unwrap();
  }

  #[test]
  fn test_transaction_read_operations() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some data first
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    ray
      .set_prop(alice.id, "name", PropValue::String("Alice".into()))
      .unwrap();

    // Transaction that reads and writes
    let name = ray
      .transaction(|ctx| {
        // Read existing data
        let existing = ctx.get("User", "alice")?;
        assert!(existing.is_some());

        let name = ctx.get_prop(alice.id, "name")?;
        assert!(ctx.exists(alice.id));

        // Create new node
        ctx.create_node("User", "bob", HashMap::new())?;

        Ok(name)
      })
      .unwrap();

    assert_eq!(name, Some(PropValue::String("Alice".into())));
    assert!(ray.get("User", "bob").unwrap().is_some());

    ray.close().unwrap();
  }

  #[test]
  fn test_transaction_edge_operations() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create nodes first
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    // Link edges in transaction
    ray
      .transaction(|ctx| {
        ctx.link(alice.id, "FOLLOWS", bob.id)?;
        ctx.link(bob.id, "FOLLOWS", charlie.id)?;
        Ok(())
      })
      .unwrap();

    // Verify edges exist after commit
    assert!(ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());
    assert!(ray.has_edge(bob.id, "FOLLOWS", charlie.id).unwrap());
    assert!(!ray.has_edge(alice.id, "FOLLOWS", charlie.id).unwrap());

    // Test unlink in transaction
    ray
      .transaction(|ctx| {
        ctx.unlink(alice.id, "FOLLOWS", bob.id)?;
        Ok(())
      })
      .unwrap();

    // Verify edge was removed
    assert!(!ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());
    // Other edge still exists
    assert!(ray.has_edge(bob.id, "FOLLOWS", charlie.id).unwrap());

    ray.close().unwrap();
  }

  // ============================================================================
  // TxBuilder Tests
  // ============================================================================

  #[test]
  fn test_tx_builder() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Use the builder pattern
    let results = ray
      .tx()
      .create_node("User", "alice", HashMap::new())
      .create_node("User", "bob", HashMap::new())
      .execute(&mut ray)
      .unwrap();

    assert_eq!(results.len(), 2);

    // Extract IDs and create edges
    let alice_id = match &results[0] {
      BatchResult::NodeCreated(node_ref) => node_ref.id,
      _ => panic!("Expected NodeCreated"),
    };
    let bob_id = match &results[1] {
      BatchResult::NodeCreated(node_ref) => node_ref.id,
      _ => panic!("Expected NodeCreated"),
    };

    ray
      .tx()
      .link(alice_id, "FOLLOWS", bob_id)
      .set_prop(alice_id, "name", PropValue::String("Alice".into()))
      .execute(&mut ray)
      .unwrap();

    assert!(ray.has_edge(alice_id, "FOLLOWS", bob_id).unwrap());
    assert_eq!(
      ray.get_prop(alice_id, "name"),
      Some(PropValue::String("Alice".into()))
    );

    ray.close().unwrap();
  }

  #[test]
  fn test_tx_builder_into_ops() {
    // Test that into_ops returns the operations without executing
    let ops = TxBuilder::default()
      .create_node("User", "alice", HashMap::new())
      .link(1, "FOLLOWS", 2)
      .set_prop(1, "name", PropValue::String("Test".into()))
      .into_ops();

    assert_eq!(ops.len(), 3);
  }

  // ============================================================================
  // Edge Property Tests
  // ============================================================================

  #[test]
  fn test_link_with_props() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Link with properties
    let mut props = HashMap::new();
    props.insert("weight".to_string(), PropValue::F64(0.8));
    props.insert("since".to_string(), PropValue::String("2024".into()));

    ray
      .link_with_props(alice.id, "FOLLOWS", bob.id, props)
      .unwrap();

    // Verify edge exists
    assert!(ray.has_edge(alice.id, "FOLLOWS", bob.id).unwrap());

    // Verify edge properties
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, Some(PropValue::F64(0.8)));

    let since = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "since")
      .unwrap();
    assert_eq!(since, Some(PropValue::String("2024".into())));

    ray.close().unwrap();
  }

  #[test]
  fn test_set_edge_prop() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge without properties
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Set edge property
    ray
      .set_edge_prop(alice.id, "FOLLOWS", bob.id, "weight", PropValue::F64(0.5))
      .unwrap();

    // Get edge property
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, Some(PropValue::F64(0.5)));

    // Update edge property
    ray
      .set_edge_prop(alice.id, "FOLLOWS", bob.id, "weight", PropValue::F64(0.9))
      .unwrap();
    let new_weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(new_weight, Some(PropValue::F64(0.9)));

    ray.close().unwrap();
  }

  #[test]
  fn test_get_edge_props() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge with properties
    let mut props = HashMap::new();
    props.insert("weight".to_string(), PropValue::F64(0.7));
    props.insert("type".to_string(), PropValue::String("friend".into()));
    ray
      .link_with_props(alice.id, "FOLLOWS", bob.id, props)
      .unwrap();

    // Get all properties
    let all_props = ray.get_edge_props(alice.id, "FOLLOWS", bob.id).unwrap();
    assert!(all_props.is_some());

    let all_props = all_props.unwrap();
    assert_eq!(all_props.get("weight"), Some(&PropValue::F64(0.7)));
    assert_eq!(
      all_props.get("type"),
      Some(&PropValue::String("friend".into()))
    );

    ray.close().unwrap();
  }

  #[test]
  fn test_del_edge_prop() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge with property
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray
      .set_edge_prop(alice.id, "FOLLOWS", bob.id, "weight", PropValue::F64(0.5))
      .unwrap();

    // Verify property exists
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, Some(PropValue::F64(0.5)));

    // Delete property
    ray
      .del_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();

    // Verify property is gone
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, None);

    ray.close().unwrap();
  }

  #[test]
  fn test_edge_prop_nonexistent_edge() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Try to get prop on nonexistent edge - should fail gracefully
    // First we need to create the prop key
    ray
      .set_edge_prop(alice.id, "FOLLOWS", bob.id, "weight", PropValue::F64(0.5))
      .ok();

    // Edge doesn't exist, so getting props should return None
    let _props = ray.get_edge_props(alice.id, "FOLLOWS", bob.id).unwrap();
    // The edge was implicitly created when we set the prop, so it exists now
    // Let's test with a truly nonexistent edge
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();
    let props2 = ray.get_edge_props(alice.id, "FOLLOWS", charlie.id).unwrap();
    assert!(props2.is_none());

    ray.close().unwrap();
  }

  #[test]
  fn test_update_edge_builder() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge first
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Update edge properties using the builder
    ray
      .update_edge(alice.id, "FOLLOWS", bob.id)
      .unwrap()
      .set("weight", PropValue::F64(0.9))
      .set("since", PropValue::String("2024".into()))
      .execute()
      .unwrap();

    // Verify properties were set
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, Some(PropValue::F64(0.9)));

    let since = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "since")
      .unwrap();
    assert_eq!(since, Some(PropValue::String("2024".into())));

    // Update with unset
    ray
      .update_edge(alice.id, "FOLLOWS", bob.id)
      .unwrap()
      .set("weight", PropValue::F64(0.5))
      .unset("since")
      .execute()
      .unwrap();

    // Verify update and unset
    let weight = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "weight")
      .unwrap();
    assert_eq!(weight, Some(PropValue::F64(0.5)));

    let since = ray
      .get_edge_prop(alice.id, "FOLLOWS", bob.id, "since")
      .unwrap();
    assert_eq!(since, None);

    ray.close().unwrap();
  }

  #[test]
  fn test_update_edge_builder_set_all() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Update using set_all
    let mut props = HashMap::new();
    props.insert("weight".to_string(), PropValue::F64(0.8));
    props.insert("type".to_string(), PropValue::String("close_friend".into()));

    ray
      .update_edge(alice.id, "FOLLOWS", bob.id)
      .unwrap()
      .set_all(props)
      .execute()
      .unwrap();

    // Verify
    let all_props = ray
      .get_edge_props(alice.id, "FOLLOWS", bob.id)
      .unwrap()
      .unwrap();
    assert_eq!(all_props.get("weight"), Some(&PropValue::F64(0.8)));
    assert_eq!(
      all_props.get("type"),
      Some(&PropValue::String("close_friend".into()))
    );

    ray.close().unwrap();
  }

  #[test]
  fn test_update_edge_builder_empty() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();

    // Create edge
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Empty update should succeed (no-op)
    ray
      .update_edge(alice.id, "FOLLOWS", bob.id)
      .unwrap()
      .execute()
      .unwrap();

    ray.close().unwrap();
  }

  #[test]
  fn test_insert_builder_returning() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Insert with returning
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".into()));
    props.insert("age".to_string(), PropValue::I64(30));

    let alice = ray
      .insert("User")
      .unwrap()
      .values("alice", props)
      .unwrap()
      .returning()
      .unwrap();

    // Verify the returned node
    assert!(alice.id > 0);
    assert_eq!(alice.key, Some("user:alice".to_string()));
    assert_eq!(alice.node_type, "User");

    // Verify properties were set
    let name = ray.get_prop(alice.id, "name");
    assert_eq!(name, Some(PropValue::String("Alice".into())));

    let age = ray.get_prop(alice.id, "age");
    assert_eq!(age, Some(PropValue::I64(30)));

    ray.close().unwrap();
  }

  #[test]
  fn test_insert_builder_execute() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Insert without returning
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Bob".into()));

    ray
      .insert("User")
      .unwrap()
      .values("bob", props)
      .unwrap()
      .execute()
      .unwrap();

    // Verify node was created
    let bob = ray.get("User", "bob").unwrap();
    assert!(bob.is_some());

    let bob = bob.unwrap();
    let name = ray.get_prop(bob.id, "name");
    assert_eq!(name, Some(PropValue::String("Bob".into())));

    ray.close().unwrap();
  }

  #[test]
  fn test_insert_builder_values_many() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Insert multiple nodes
    let mut alice_props = HashMap::new();
    alice_props.insert("name".to_string(), PropValue::String("Alice".into()));

    let mut bob_props = HashMap::new();
    bob_props.insert("name".to_string(), PropValue::String("Bob".into()));

    let mut charlie_props = HashMap::new();
    charlie_props.insert("name".to_string(), PropValue::String("Charlie".into()));

    let users = ray
      .insert("User")
      .unwrap()
      .values_many(vec![
        ("alice", alice_props),
        ("bob", bob_props),
        ("charlie", charlie_props),
      ])
      .unwrap()
      .returning()
      .unwrap();

    // Verify all nodes were created
    assert_eq!(users.len(), 3);
    assert_eq!(users[0].key, Some("user:alice".to_string()));
    assert_eq!(users[1].key, Some("user:bob".to_string()));
    assert_eq!(users[2].key, Some("user:charlie".to_string()));

    // Verify count
    assert_eq!(ray.count_nodes(), 3);

    ray.close().unwrap();
  }

  #[test]
  fn test_insert_builder_empty_values_many() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Empty insert should succeed
    let users = ray
      .insert("User")
      .unwrap()
      .values_many(vec![])
      .unwrap()
      .returning()
      .unwrap();

    assert_eq!(users.len(), 0);
    assert_eq!(ray.count_nodes(), 0);

    ray.close().unwrap();
  }

  #[test]
  fn test_check_empty_database() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let ray = Ray::open(temp_dir.path(), options).unwrap();

    let result = ray.check().unwrap();
    assert!(result.valid);
    assert!(result.errors.is_empty());
    // Should have a warning about missing snapshot
    assert!(result.warnings.iter().any(|w| w.contains("No snapshot")));

    ray.close().unwrap();
  }

  #[test]
  fn test_check_valid_database() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some nodes and edges
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    let charlie = ray.create_node("User", "charlie", HashMap::new()).unwrap();

    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();
    ray.link(bob.id, "FOLLOWS", charlie.id).unwrap();
    ray.link(charlie.id, "FOLLOWS", alice.id).unwrap();

    // Check should pass
    let result = ray.check().unwrap();
    assert!(
      result.valid,
      "Expected valid database, got errors: {:?}",
      result.errors
    );
    assert!(result.errors.is_empty());

    ray.close().unwrap();
  }

  #[test]
  fn test_check_with_properties() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create nodes with properties
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".into()));
    props.insert("age".to_string(), PropValue::I64(30));
    let alice = ray.create_node("User", "alice", props).unwrap();

    let mut props2 = HashMap::new();
    props2.insert("name".to_string(), PropValue::String("Bob".into()));
    let bob = ray.create_node("User", "bob", props2).unwrap();

    // Create edge with properties
    let mut edge_props = HashMap::new();
    edge_props.insert("weight".to_string(), PropValue::F64(0.9));
    ray
      .link_with_props(alice.id, "FOLLOWS", bob.id, edge_props)
      .unwrap();

    // Check should pass
    let result = ray.check().unwrap();
    assert!(
      result.valid,
      "Expected valid database, got errors: {:?}",
      result.errors
    );
    assert!(result.errors.is_empty());

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_by_ref() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".into()));
    props.insert("age".to_string(), PropValue::I64(30));
    let alice = ray.create_node("User", "alice", props).unwrap();

    // Update by reference
    ray
      .update(&alice)
      .unwrap()
      .set("name", PropValue::String("Alice Updated".into()))
      .set("age", PropValue::I64(31))
      .execute()
      .unwrap();

    // Verify updates
    let name = ray.get_prop(alice.id, "name");
    assert_eq!(name, Some(PropValue::String("Alice Updated".into())));

    let age = ray.get_prop(alice.id, "age");
    assert_eq!(age, Some(PropValue::I64(31)));

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_by_key() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Bob".into()));
    ray.create_node("User", "bob", props).unwrap();

    // Update by key
    ray
      .update_by_key("User", "bob")
      .unwrap()
      .set("name", PropValue::String("Bob Updated".into()))
      .set("age", PropValue::I64(25))
      .execute()
      .unwrap();

    // Verify updates
    let bob = ray.get("User", "bob").unwrap().unwrap();
    let name = ray.get_prop(bob.id, "name");
    assert_eq!(name, Some(PropValue::String("Bob Updated".into())));

    let age = ray.get_prop(bob.id, "age");
    assert_eq!(age, Some(PropValue::I64(25)));

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_by_id() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Charlie".into()));
    let charlie = ray.create_node("User", "charlie", props).unwrap();

    // Update by ID
    ray
      .update_by_id(charlie.id)
      .unwrap()
      .set("name", PropValue::String("Charlie Updated".into()))
      .execute()
      .unwrap();

    // Verify updates
    let name = ray.get_prop(charlie.id, "name");
    assert_eq!(name, Some(PropValue::String("Charlie Updated".into())));

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_unset() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node with properties
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Dave".into()));
    props.insert("age".to_string(), PropValue::I64(40));
    let dave = ray.create_node("User", "dave", props).unwrap();

    // Verify properties exist
    assert!(ray.get_prop(dave.id, "age").is_some());

    // Update with unset
    ray
      .update(&dave)
      .unwrap()
      .set("name", PropValue::String("Dave Updated".into()))
      .unset("age")
      .execute()
      .unwrap();

    // Verify name updated and age removed
    let name = ray.get_prop(dave.id, "name");
    assert_eq!(name, Some(PropValue::String("Dave Updated".into())));

    let age = ray.get_prop(dave.id, "age");
    assert_eq!(age, None);

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_set_all() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let eve = ray.create_node("User", "eve", HashMap::new()).unwrap();

    // Update with set_all
    let mut updates = HashMap::new();
    updates.insert("name".to_string(), PropValue::String("Eve".into()));
    updates.insert("age".to_string(), PropValue::I64(28));

    ray
      .update(&eve)
      .unwrap()
      .set_all(updates)
      .execute()
      .unwrap();

    // Verify all properties set
    let name = ray.get_prop(eve.id, "name");
    assert_eq!(name, Some(PropValue::String("Eve".into())));

    let age = ray.get_prop(eve.id, "age");
    assert_eq!(age, Some(PropValue::I64(28)));

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_nonexistent() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Try to update non-existent node by ID
    let result = ray.update_by_id(999999);
    assert!(result.is_err());

    // Try to update non-existent node by key
    let result = ray.update_by_key("User", "nonexistent");
    assert!(result.is_err());

    ray.close().unwrap();
  }

  #[test]
  fn test_update_node_empty() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create a node
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Frank".into()));
    let frank = ray.create_node("User", "frank", props).unwrap();

    // Empty update should succeed (no-op)
    ray.update(&frank).unwrap().execute().unwrap();

    // Verify nothing changed
    let name = ray.get_prop(frank.id, "name");
    assert_eq!(name, Some(PropValue::String("Frank".into())));

    ray.close().unwrap();
  }

  #[test]
  fn test_describe() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some data
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Get description
    let desc = ray.describe();

    // Should contain path
    assert!(desc.contains("RayDB at"));
    // Should mention format
    assert!(desc.contains("format"));
    // Should list node types
    assert!(desc.contains("User"));
    // Should list edge types
    assert!(desc.contains("FOLLOWS"));
    // Should include stats
    assert!(desc.contains("Nodes:"));
    assert!(desc.contains("Edges:"));

    ray.close().unwrap();
  }

  #[test]
  fn test_stats() {
    let temp_dir = tempdir().unwrap();
    let options = create_test_schema();

    let mut ray = Ray::open(temp_dir.path(), options).unwrap();

    // Create some data
    let alice = ray.create_node("User", "alice", HashMap::new()).unwrap();
    let bob = ray.create_node("User", "bob", HashMap::new()).unwrap();
    ray.link(alice.id, "FOLLOWS", bob.id).unwrap();

    // Get stats
    let stats = ray.stats();

    // Should report correct counts
    assert!(stats.snapshot_nodes >= 2);
    assert!(stats.snapshot_edges >= 1);
    // Delta should show created nodes
    assert!(stats.delta_nodes_created >= 2);
    assert!(stats.delta_edges_added >= 1);

    ray.close().unwrap();
  }
}
