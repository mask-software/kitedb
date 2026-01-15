//! Ray - High-level API for RayDB
//!
//! The Ray struct provides a clean, ergonomic API for graph operations.
//! It wraps the lower-level GraphDB with schema definitions and type-safe operations.
//!
//! Ported from src/api/ray.ts

use crate::error::{RayError, Result};
use crate::graph::db::{close_graph_db, open_graph_db, GraphDB, OpenOptions};
use crate::graph::edges::{add_edge, delete_edge, edge_exists, get_neighbors_in, get_neighbors_out};
use crate::graph::nodes::{create_node, delete_node, get_node_by_key, node_exists, get_node_prop, set_node_prop, NodeOpts};
use crate::graph::tx::{begin_tx, commit};
use crate::graph::iterators::{list_nodes, count_nodes, count_edges};
use crate::types::*;

use std::collections::HashMap;
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
            for (prop_name, _prop_def) in &node_def.props {
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
            for (prop_name, _prop_def) in &edge_def.props {
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
        let node_def = self.nodes.get(node_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {}", node_type)))?
            .clone();

        let full_key = node_def.key(key_suffix);

        // Begin transaction
        let mut handle = begin_tx(&mut self.db)?;

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

    /// Get a node by key
    pub fn get(&mut self, node_type: &str, key_suffix: &str) -> Result<Option<NodeRef>> {
        let node_def = self.nodes.get(node_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown node type: {}", node_type)))?;

        let full_key = node_def.key(key_suffix);

        let mut handle = begin_tx(&mut self.db)?;
        let node_id = get_node_by_key(&handle, &full_key);
        commit(&mut handle)?;

        match node_id {
            Some(id) => Ok(Some(NodeRef::new(id, Some(full_key), node_type))),
            None => Ok(None),
        }
    }

    /// Get a node by ID
    pub fn get_by_id(&mut self, node_id: NodeId) -> Result<Option<NodeRef>> {
        let mut handle = begin_tx(&mut self.db)?;
        let exists = node_exists(&handle, node_id);
        commit(&mut handle)?;

        if exists {
            // Try to determine node type from key
            // TODO: Implement key lookup from snapshot/delta
            Ok(Some(NodeRef::new(node_id, None, "unknown")))
        } else {
            Ok(None)
        }
    }

    /// Check if a node exists
    pub fn exists(&mut self, node_id: NodeId) -> Result<bool> {
        let mut handle = begin_tx(&mut self.db)?;
        let exists = node_exists(&handle, node_id);
        commit(&mut handle)?;
        Ok(exists)
    }

    /// Delete a node
    pub fn delete_node(&mut self, node_id: NodeId) -> Result<bool> {
        let mut handle = begin_tx(&mut self.db)?;
        let deleted = delete_node(&mut handle, node_id)?;
        commit(&mut handle)?;
        Ok(deleted)
    }

    /// Get a node property
    pub fn get_prop(&mut self, node_id: NodeId, prop_name: &str) -> Result<Option<PropValue>> {
        let prop_key_id = self.db.get_propkey_id(prop_name)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown property: {}", prop_name)))?;

        let mut handle = begin_tx(&mut self.db)?;
        let value = get_node_prop(&handle, node_id, prop_key_id);
        commit(&mut handle)?;
        Ok(value)
    }

    /// Set a node property
    pub fn set_prop(&mut self, node_id: NodeId, prop_name: &str, value: PropValue) -> Result<()> {
        let prop_key_id = self.db.get_or_create_propkey(prop_name);

        let mut handle = begin_tx(&mut self.db)?;
        set_node_prop(&mut handle, node_id, prop_key_id, value)?;
        commit(&mut handle)?;
        Ok(())
    }

    // ========================================================================
    // Edge Operations
    // ========================================================================

    /// Create an edge between two nodes
    pub fn link(
        &mut self,
        src: NodeId,
        edge_type: &str,
        dst: NodeId,
    ) -> Result<()> {
        let edge_def = self.edges.get(edge_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {}", edge_type)))?;

        let etype_id = edge_def.etype_id
            .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

        let mut handle = begin_tx(&mut self.db)?;
        add_edge(&mut handle, src, etype_id, dst)?;
        commit(&mut handle)?;
        Ok(())
    }

    /// Remove an edge between two nodes
    pub fn unlink(
        &mut self,
        src: NodeId,
        edge_type: &str,
        dst: NodeId,
    ) -> Result<bool> {
        let edge_def = self.edges.get(edge_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {}", edge_type)))?;

        let etype_id = edge_def.etype_id
            .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

        let mut handle = begin_tx(&mut self.db)?;
        let deleted = delete_edge(&mut handle, src, etype_id, dst)?;
        commit(&mut handle)?;
        Ok(deleted)
    }

    /// Check if an edge exists
    pub fn has_edge(
        &mut self,
        src: NodeId,
        edge_type: &str,
        dst: NodeId,
    ) -> Result<bool> {
        let edge_def = self.edges.get(edge_type)
            .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {}", edge_type)))?;

        let etype_id = edge_def.etype_id
            .ok_or_else(|| RayError::InvalidSchema("Edge type not initialized".to_string()))?;

        let mut handle = begin_tx(&mut self.db)?;
        let exists = edge_exists(&handle, src, etype_id, dst);
        commit(&mut handle)?;
        Ok(exists)
    }

    /// Get outgoing neighbors of a node
    pub fn neighbors_out(
        &mut self,
        node_id: NodeId,
        edge_type: Option<&str>,
    ) -> Result<Vec<NodeId>> {
        let etype_id = match edge_type {
            Some(name) => {
                let edge_def = self.edges.get(name)
                    .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {}", name)))?;
                edge_def.etype_id
            }
            None => None,
        };

        let mut handle = begin_tx(&mut self.db)?;
        let neighbors = get_neighbors_out(&handle, node_id, etype_id);
        commit(&mut handle)?;
        Ok(neighbors)
    }

    /// Get incoming neighbors of a node
    pub fn neighbors_in(
        &mut self,
        node_id: NodeId,
        edge_type: Option<&str>,
    ) -> Result<Vec<NodeId>> {
        let etype_id = match edge_type {
            Some(name) => {
                let edge_def = self.edges.get(name)
                    .ok_or_else(|| RayError::InvalidSchema(format!("Unknown edge type: {}", name)))?;
                edge_def.etype_id
            }
            None => None,
        };

        let mut handle = begin_tx(&mut self.db)?;
        let neighbors = get_neighbors_in(&handle, node_id, etype_id);
        commit(&mut handle)?;
        Ok(neighbors)
    }

    // ========================================================================
    // Listing and Counting
    // ========================================================================

    /// Count all nodes
    pub fn count_nodes(&self) -> u64 {
        count_nodes(&self.db)
    }

    /// Count all edges
    pub fn count_edges(&self) -> u64 {
        count_edges(&self.db, None)
    }

    /// List all node IDs
    pub fn list_nodes(&self) -> Vec<NodeId> {
        list_nodes(&self.db)
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
        let name = ray.get_prop(user.id, "name").unwrap();
        assert_eq!(name, Some(PropValue::String("Alice".to_string())));
        
        // Set property
        ray.set_prop(user.id, "age", PropValue::I64(25)).unwrap();
        let age = ray.get_prop(user.id, "age").unwrap();
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
        assert!(ray.exists(user.id).unwrap());
        
        ray.delete_node(user.id).unwrap();
        assert!(!ray.exists(user.id).unwrap());
        
        ray.close().unwrap();
    }
}
