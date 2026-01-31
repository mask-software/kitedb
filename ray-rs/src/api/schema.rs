//! Schema Definition API
//!
//! Drizzle-style schema builders for defining graph nodes and edges
//! with full type safety. Provides:
//!
//! - Property type builders (string, int, float, bool, vector)
//! - Node type definitions with key generation
//! - Edge type definitions with optional properties
//!
//! # Example
//!
//! ```rust,no_run
//! use raydb::api::schema::{node, edge, prop};
//!
//! let user = node("user")
//!     .key(|id: &str| format!("user:{}", id))
//!     .prop(prop::string("name"))
//!     .prop(prop::int("age").optional())
//!     .build();
//!
//! let knows = edge("knows")
//!     .prop(prop::int("since"))
//!     .prop(prop::float("weight").optional())
//!     .build();
//!
//! // Edge without properties
//! let follows = edge("follows").build();
//! ```
//!
//! Ported from src/api/schema.ts

use crate::types::{PropKeyId, PropValue, PropValueTag};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// ============================================================================
// Property Type Definitions
// ============================================================================

/// Property type identifiers
///
/// Maps to storage types:
/// - `String` -> UTF-8 strings (PropValueTag::String)
/// - `Int` -> 64-bit signed integers (PropValueTag::I64)
/// - `Float` -> 64-bit IEEE 754 floats (PropValueTag::F64)
/// - `Bool` -> booleans (PropValueTag::Bool)
/// - `Vector` -> float32 vectors for embeddings (PropValueTag::VectorF32)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaType {
  String,
  Int,
  Float,
  Bool,
  Vector,
}

impl SchemaType {
  /// Convert to internal PropValueTag
  pub fn to_tag(&self) -> PropValueTag {
    match self {
      SchemaType::String => PropValueTag::String,
      SchemaType::Int => PropValueTag::I64,
      SchemaType::Float => PropValueTag::F64,
      SchemaType::Bool => PropValueTag::Bool,
      SchemaType::Vector => PropValueTag::VectorF32,
    }
  }
}

impl fmt::Display for SchemaType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      SchemaType::String => write!(f, "string"),
      SchemaType::Int => write!(f, "int"),
      SchemaType::Float => write!(f, "float"),
      SchemaType::Bool => write!(f, "bool"),
      SchemaType::Vector => write!(f, "vector"),
    }
  }
}

// ============================================================================
// Property Definition
// ============================================================================

/// A property definition with type and optionality information.
///
/// Created using the `prop` module builders:
/// - `prop::string("name")` - required string property
/// - `prop::int("age").optional()` - optional int property
#[derive(Debug, Clone)]
pub struct PropDef {
  /// Property name (used as key in database)
  pub name: String,
  /// Property type
  pub schema_type: SchemaType,
  /// Whether this property is optional
  pub optional: bool,
  /// Default value (if any)
  pub default: Option<PropValue>,
  /// Internal: resolved prop key ID (set during db initialization)
  pub(crate) key_id: Option<PropKeyId>,
}

impl PropDef {
  /// Create a new property definition
  fn new(name: &str, schema_type: SchemaType) -> Self {
    Self {
      name: name.to_string(),
      schema_type,
      optional: false,
      default: None,
      key_id: None,
    }
  }

  /// Mark this property as optional
  pub fn optional(mut self) -> Self {
    self.optional = true;
    self
  }

  /// Set a default value for this property
  pub fn default(mut self, value: PropValue) -> Self {
    self.default = Some(value);
    self
  }

  /// Check if a value matches this property's type
  pub fn validate(&self, value: &PropValue) -> bool {
    match (self.schema_type, value) {
      (SchemaType::String, PropValue::String(_)) => true,
      (SchemaType::Int, PropValue::I64(_)) => true,
      (SchemaType::Float, PropValue::F64(_)) => true,
      (SchemaType::Bool, PropValue::Bool(_)) => true,
      (SchemaType::Vector, PropValue::VectorF32(_)) => true,
      (_, PropValue::Null) => self.optional,
      _ => false,
    }
  }
}

// ============================================================================
// Property Builders
// ============================================================================

/// Property type builders
///
/// Use these to define typed properties on nodes and edges.
/// All builders support `.optional()` for optional properties.
///
/// # Examples
///
/// ```rust,no_run
/// use raydb::api::schema::prop;
///
/// let name = prop::string("name");           // required string
/// let age = prop::int("age").optional();     // optional int
/// let score = prop::float("score");          // required float
/// let active = prop::bool("active");         // required bool
/// let embedding = prop::vector("embedding"); // required vector
/// ```
pub mod prop {
  use super::*;

  /// Create a string property
  ///
  /// Stored as UTF-8 strings (maps to PropValueTag::String)
  pub fn string(name: &str) -> PropDef {
    PropDef::new(name, SchemaType::String)
  }

  /// Create an integer property
  ///
  /// Stored as 64-bit signed integers (maps to PropValueTag::I64)
  pub fn int(name: &str) -> PropDef {
    PropDef::new(name, SchemaType::Int)
  }

  /// Create a float property
  ///
  /// Stored as 64-bit IEEE 754 floats (maps to PropValueTag::F64)
  pub fn float(name: &str) -> PropDef {
    PropDef::new(name, SchemaType::Float)
  }

  /// Create a boolean property
  ///
  /// Stored as true/false (maps to PropValueTag::Bool)
  pub fn bool(name: &str) -> PropDef {
    PropDef::new(name, SchemaType::Bool)
  }

  /// Create a vector property for embeddings
  ///
  /// Stored as Float32 arrays (maps to PropValueTag::VectorF32)
  ///
  /// Note: Vector properties require separate handling via the vector store API.
  /// This type definition enables type validation for vector properties.
  pub fn vector(name: &str) -> PropDef {
    PropDef::new(name, SchemaType::Vector)
  }
}

// ============================================================================
// Key Function Types
// ============================================================================

/// Key generator function type
///
/// Transforms application identifiers into unique node keys.
/// Can be stored and called dynamically.
pub type KeyFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Create a key function from a closure
///
/// # Example
///
/// ```rust,no_run
/// # use raydb::api::schema::key_fn;
/// let key_fn = key_fn(|id| format!("user:{}", id));
/// ```
pub fn key_fn<F>(f: F) -> KeyFn
where
  F: Fn(&str) -> String + Send + Sync + 'static,
{
  Arc::new(f)
}

// ============================================================================
// Node Definition
// ============================================================================

/// A defined node type with metadata.
///
/// Created by `node()` and used throughout the API.
/// Contains the node type name, key generator function, and property schema.
#[derive(Clone)]
pub struct NodeSchema {
  /// Node type name (must be unique per database)
  pub name: String,
  /// Key generator function
  pub key_fn: KeyFn,
  /// Property definitions by name
  pub props: HashMap<String, PropDef>,
  /// Key prefix (extracted from key_fn for optimization)
  pub key_prefix: String,
  /// Internal: resolved label ID (set during db initialization)
  pub(crate) label_id: Option<u32>,
  /// Internal: resolved prop key IDs (set during db initialization)
  pub(crate) prop_key_ids: HashMap<String, PropKeyId>,
}

impl fmt::Debug for NodeSchema {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("NodeSchema")
      .field("name", &self.name)
      .field("key_prefix", &self.key_prefix)
      .field("props", &self.props)
      .finish()
  }
}

impl NodeSchema {
  /// Generate a full key from a key argument
  pub fn key(&self, id: &str) -> String {
    (self.key_fn)(id)
  }

  /// Get a property definition by name
  pub fn get_prop(&self, name: &str) -> Option<&PropDef> {
    self.props.get(name)
  }

  /// Get all required property names
  pub fn required_props(&self) -> Vec<&str> {
    self
      .props
      .iter()
      .filter(|(_, def)| !def.optional)
      .map(|(name, _)| name.as_str())
      .collect()
  }

  /// Validate a set of properties against this schema
  pub fn validate(&self, props: &HashMap<String, PropValue>) -> Result<(), ValidationError> {
    // Check required properties are present
    for (name, def) in &self.props {
      if !def.optional && !props.contains_key(name) && def.default.is_none() {
        return Err(ValidationError::MissingRequired(name.clone()));
      }
    }

    // Validate property types
    for (name, value) in props {
      if let Some(def) = self.props.get(name) {
        if !def.validate(value) {
          return Err(ValidationError::TypeMismatch {
            prop: name.clone(),
            expected: def.schema_type,
            got: value.tag(),
          });
        }
      }
      // Unknown properties are allowed (schema-less flexibility)
    }

    Ok(())
  }
}

/// Node schema builder
///
/// Used to fluently construct node definitions.
///
/// # Example
///
/// ```rust,no_run
/// # use raydb::api::schema::{node, prop};
/// let user = node("user")
///     .key(|id| format!("user:{}", id))
///     .prop(prop::string("name"))
///     .prop(prop::int("age").optional())
///     .build();
/// ```
pub struct NodeSchemaBuilder {
  name: String,
  key_fn: Option<KeyFn>,
  key_prefix: String,
  props: HashMap<String, PropDef>,
}

impl NodeSchemaBuilder {
  fn new(name: &str) -> Self {
    Self {
      name: name.to_string(),
      key_fn: None,
      key_prefix: format!("{name}:"),
      props: HashMap::new(),
    }
  }

  /// Set the key generator function
  ///
  /// The key function transforms application IDs into unique node keys.
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::schema::node;
  /// node("user")
  ///     .key(|id| format!("user:{}", id))
  ///     ;
  /// ```
  pub fn key<F>(mut self, f: F) -> Self
  where
    F: Fn(&str) -> String + Send + Sync + 'static,
  {
    // Try to extract prefix from a test call
    let test_key = f("__test__");
    if let Some(pos) = test_key.find("__test__") {
      self.key_prefix = test_key[..pos].to_string();
    }
    self.key_fn = Some(Arc::new(f));
    self
  }

  /// Set a custom key prefix (overrides auto-detection)
  pub fn key_prefix(mut self, prefix: &str) -> Self {
    self.key_prefix = prefix.to_string();
    self
  }

  /// Add a property definition
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::schema::{node, prop};
  /// node("user")
  ///     .prop(prop::string("name"))
  ///     .prop(prop::int("age").optional())
  ///     ;
  /// ```
  pub fn prop(mut self, prop_def: PropDef) -> Self {
    self.props.insert(prop_def.name.clone(), prop_def);
    self
  }

  /// Build the final node schema
  ///
  /// If no key function was provided, uses a default: `"{name}:{id}"`
  pub fn build(self) -> NodeSchema {
    let name = self.name.clone();
    let key_fn = self.key_fn.unwrap_or_else(|| {
      let name_clone = name.clone();
      Arc::new(move |id: &str| format!("{name_clone}:{id}"))
    });

    NodeSchema {
      name: self.name,
      key_fn,
      key_prefix: self.key_prefix,
      props: self.props,
      label_id: None,
      prop_key_ids: HashMap::new(),
    }
  }
}

/// Define a node type
///
/// Creates a builder for defining node schemas with properties and key generation.
///
/// # Examples
///
/// ```rust,no_run
/// use raydb::api::schema::{node, prop};
///
/// // Full definition with key function
/// let user = node("user")
///     .key(|id| format!("user:{}", id))
///     .prop(prop::string("name"))
///     .prop(prop::string("email"))
///     .prop(prop::int("age").optional())
///     .build();
///
/// // Simple definition with default key function
/// let post = node("post")
///     .prop(prop::string("title"))
///     .prop(prop::string("content").optional())
///     .build();
/// ```
pub fn node(name: &str) -> NodeSchemaBuilder {
  NodeSchemaBuilder::new(name)
}

/// Define a node type (deprecated alias for `node()`)
#[deprecated(since = "0.2.0", note = "Use `node()` instead")]
pub fn define_node(name: &str) -> NodeSchemaBuilder {
  node(name)
}

// ============================================================================
// Edge Definition
// ============================================================================

/// A defined edge type with metadata.
///
/// Created by `edge()` and used throughout the API.
/// Contains the edge type name and optional property schema.
#[derive(Debug, Clone)]
pub struct EdgeSchema {
  /// Edge type name (must be unique per database)
  pub name: String,
  /// Property definitions by name (optional for edges)
  pub props: HashMap<String, PropDef>,
  /// Internal: resolved edge type ID (set during db initialization)
  pub(crate) etype_id: Option<u32>,
  /// Internal: resolved prop key IDs (set during db initialization)
  pub(crate) prop_key_ids: HashMap<String, PropKeyId>,
}

impl EdgeSchema {
  /// Get a property definition by name
  pub fn get_prop(&self, name: &str) -> Option<&PropDef> {
    self.props.get(name)
  }

  /// Check if this edge has any properties
  pub fn has_props(&self) -> bool {
    !self.props.is_empty()
  }

  /// Validate a set of properties against this schema
  pub fn validate(&self, props: &HashMap<String, PropValue>) -> Result<(), ValidationError> {
    // Check required properties are present
    for (name, def) in &self.props {
      if !def.optional && !props.contains_key(name) && def.default.is_none() {
        return Err(ValidationError::MissingRequired(name.clone()));
      }
    }

    // Validate property types
    for (name, value) in props {
      if let Some(def) = self.props.get(name) {
        if !def.validate(value) {
          return Err(ValidationError::TypeMismatch {
            prop: name.clone(),
            expected: def.schema_type,
            got: value.tag(),
          });
        }
      }
    }

    Ok(())
  }
}

/// Edge schema builder
///
/// Used to fluently construct edge definitions.
///
/// # Example
///
/// ```rust,no_run
/// # use raydb::api::schema::{edge, prop};
/// let knows = edge("knows")
///     .prop(prop::int("since"))
///     .prop(prop::float("weight").optional())
///     .build();
///
/// // Edge without properties
/// let follows = edge("follows").build();
/// ```
pub struct EdgeSchemaBuilder {
  name: String,
  props: HashMap<String, PropDef>,
}

impl EdgeSchemaBuilder {
  fn new(name: &str) -> Self {
    Self {
      name: name.to_string(),
      props: HashMap::new(),
    }
  }

  /// Add a property definition
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use raydb::api::schema::{edge, prop};
  /// edge("knows")
  ///     .prop(prop::int("since"))
  ///     .prop(prop::float("weight").optional())
  ///     ;
  /// ```
  pub fn prop(mut self, prop_def: PropDef) -> Self {
    self.props.insert(prop_def.name.clone(), prop_def);
    self
  }

  /// Build the final edge schema
  pub fn build(self) -> EdgeSchema {
    EdgeSchema {
      name: self.name,
      props: self.props,
      etype_id: None,
      prop_key_ids: HashMap::new(),
    }
  }
}

/// Define an edge type
///
/// Creates a builder for defining edge schemas with optional properties.
///
/// # Examples
///
/// ```rust,no_run
/// use raydb::api::schema::{edge, prop};
///
/// // Edge with properties
/// let knows = edge("knows")
///     .prop(prop::int("since"))
///     .prop(prop::float("weight").optional())
///     .build();
///
/// // Edge without properties
/// let follows = edge("follows").build();
/// ```
pub fn edge(name: &str) -> EdgeSchemaBuilder {
  EdgeSchemaBuilder::new(name)
}

/// Define an edge type (deprecated alias for `edge()`)
#[deprecated(since = "0.2.0", note = "Use `edge()` instead")]
pub fn define_edge(name: &str) -> EdgeSchemaBuilder {
  edge(name)
}

// ============================================================================
// Validation Errors
// ============================================================================

/// Schema validation error
#[derive(Debug, Clone)]
pub enum ValidationError {
  /// A required property is missing
  MissingRequired(String),
  /// Property type doesn't match schema
  TypeMismatch {
    prop: String,
    expected: SchemaType,
    got: PropValueTag,
  },
  /// Unknown node type
  UnknownNodeType(String),
  /// Unknown edge type
  UnknownEdgeType(String),
}

impl fmt::Display for ValidationError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ValidationError::MissingRequired(prop) => {
        write!(f, "Missing required property: {prop}")
      }
      ValidationError::TypeMismatch {
        prop,
        expected,
        got,
      } => {
        write!(
          f,
          "Type mismatch for '{prop}': expected {expected}, got {got:?}"
        )
      }
      ValidationError::UnknownNodeType(name) => {
        write!(f, "Unknown node type: {name}")
      }
      ValidationError::UnknownEdgeType(name) => {
        write!(f, "Unknown edge type: {name}")
      }
    }
  }
}

impl std::error::Error for ValidationError {}

// ============================================================================
// Database Schema
// ============================================================================

/// Complete database schema
///
/// Contains all node and edge type definitions for a database.
/// Used to initialize a Ray database with type-safe operations.
#[derive(Debug, Clone, Default)]
pub struct DatabaseSchema {
  /// Node type definitions by name
  pub nodes: HashMap<String, NodeSchema>,
  /// Edge type definitions by name
  pub edges: HashMap<String, EdgeSchema>,
  /// Key prefix to node type mapping (for reverse lookups)
  key_prefix_to_node: HashMap<String, String>,
}

impl DatabaseSchema {
  /// Create a new empty schema
  pub fn new() -> Self {
    Self::default()
  }

  /// Add a node type to the schema
  pub fn node(mut self, schema: NodeSchema) -> Self {
    self
      .key_prefix_to_node
      .insert(schema.key_prefix.clone(), schema.name.clone());
    self.nodes.insert(schema.name.clone(), schema);
    self
  }

  /// Add an edge type to the schema
  pub fn edge(mut self, schema: EdgeSchema) -> Self {
    self.edges.insert(schema.name.clone(), schema);
    self
  }

  /// Get a node schema by name
  pub fn get_node(&self, name: &str) -> Option<&NodeSchema> {
    self.nodes.get(name)
  }

  /// Get an edge schema by name
  pub fn get_edge(&self, name: &str) -> Option<&EdgeSchema> {
    self.edges.get(name)
  }

  /// Get node type from a key prefix
  pub fn node_type_from_key(&self, key: &str) -> Option<&str> {
    for (prefix, node_type) in &self.key_prefix_to_node {
      if key.starts_with(prefix) {
        return Some(node_type);
      }
    }
    None
  }

  /// Get all node type names
  pub fn node_types(&self) -> Vec<&str> {
    self.nodes.keys().map(|s| s.as_str()).collect()
  }

  /// Get all edge type names
  pub fn edge_types(&self) -> Vec<&str> {
    self.edges.keys().map(|s| s.as_str()).collect()
  }
}

// ============================================================================
// Macros for Convenience
// ============================================================================

/// Macro to create a schema with nodes and edges
///
/// # Example
///
/// ```rust,no_run
/// use raydb::schema;
/// use raydb::api::schema::{node, edge, prop};
///
/// let schema = schema! {
///     nodes: [
///         node("user")
///             .prop(prop::string("name"))
///             .build(),
///         node("post")
///             .prop(prop::string("title"))
///             .build(),
///     ],
///     edges: [
///         edge("follows").build(),
///         edge("authored").build(),
///     ]
/// };
/// ```
#[macro_export]
macro_rules! schema {
    (
        nodes: [ $($node:expr),* $(,)? ],
        edges: [ $($edge:expr),* $(,)? ]
    ) => {{
        let mut schema = $crate::api::schema::DatabaseSchema::new();
        $(
            schema = schema.node($node);
        )*
        $(
            schema = schema.edge($edge);
        )*
        schema
    }};
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_prop_builders() {
    let name = prop::string("name");
    assert_eq!(name.name, "name");
    assert_eq!(name.schema_type, SchemaType::String);
    assert!(!name.optional);

    let age = prop::int("age").optional();
    assert_eq!(age.name, "age");
    assert_eq!(age.schema_type, SchemaType::Int);
    assert!(age.optional);

    let score = prop::float("score").default(PropValue::F64(0.0));
    assert_eq!(score.name, "score");
    assert!(score.default.is_some());
  }

  #[test]
  fn test_prop_validation() {
    let name_prop = prop::string("name");
    assert!(name_prop.validate(&PropValue::String("Alice".to_string())));
    assert!(!name_prop.validate(&PropValue::I64(42)));
    assert!(!name_prop.validate(&PropValue::Null));

    let age_prop = prop::int("age").optional();
    assert!(age_prop.validate(&PropValue::I64(30)));
    assert!(age_prop.validate(&PropValue::Null)); // Optional allows null
    assert!(!age_prop.validate(&PropValue::String("thirty".to_string())));
  }

  #[test]
  fn test_node() {
    let user = node("user")
      .key(|id| format!("user:{}", id))
      .prop(prop::string("name"))
      .prop(prop::int("age").optional())
      .build();

    assert_eq!(user.name, "user");
    assert_eq!(user.key_prefix, "user:");
    assert_eq!(user.key("alice"), "user:alice");
    assert_eq!(user.props.len(), 2);
    assert!(user.get_prop("name").is_some());
    assert!(user.get_prop("age").is_some());
    assert!(!user.get_prop("name").unwrap().optional);
    assert!(user.get_prop("age").unwrap().optional);
  }

  #[test]
  fn test_node_default_key() {
    let post = node("post").prop(prop::string("title")).build();

    assert_eq!(post.name, "post");
    assert_eq!(post.key("123"), "post:123");
  }

  #[test]
  fn test_edge() {
    let knows = edge("knows")
      .prop(prop::int("since"))
      .prop(prop::float("weight").optional())
      .build();

    assert_eq!(knows.name, "knows");
    assert_eq!(knows.props.len(), 2);
    assert!(knows.has_props());
  }

  #[test]
  fn test_edge_no_props() {
    let follows = edge("follows").build();

    assert_eq!(follows.name, "follows");
    assert!(follows.props.is_empty());
    assert!(!follows.has_props());
  }

  #[test]
  fn test_node_schema_validation() {
    let user = node("user")
      .prop(prop::string("name"))
      .prop(prop::int("age").optional())
      .build();

    // Valid: all required props present
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    assert!(user.validate(&props).is_ok());

    // Valid: with optional prop
    props.insert("age".to_string(), PropValue::I64(30));
    assert!(user.validate(&props).is_ok());

    // Invalid: missing required
    let empty: HashMap<String, PropValue> = HashMap::new();
    assert!(matches!(
      user.validate(&empty),
      Err(ValidationError::MissingRequired(_))
    ));

    // Invalid: wrong type
    let mut wrong_type = HashMap::new();
    wrong_type.insert("name".to_string(), PropValue::I64(42));
    assert!(matches!(
      user.validate(&wrong_type),
      Err(ValidationError::TypeMismatch { .. })
    ));
  }

  #[test]
  fn test_database_schema() {
    let user = node("user")
      .key(|id| format!("user:{}", id))
      .prop(prop::string("name"))
      .build();

    let post = node("post")
      .key(|id| format!("post:{}", id))
      .prop(prop::string("title"))
      .build();

    let follows = edge("follows").build();
    let authored = edge("authored").build();

    let schema = DatabaseSchema::new()
      .node(user)
      .node(post)
      .edge(follows)
      .edge(authored);

    assert_eq!(schema.node_types().len(), 2);
    assert_eq!(schema.edge_types().len(), 2);
    assert!(schema.get_node("user").is_some());
    assert!(schema.get_edge("follows").is_some());

    // Test key prefix lookup
    assert_eq!(schema.node_type_from_key("user:alice"), Some("user"));
    assert_eq!(schema.node_type_from_key("post:123"), Some("post"));
    assert!(schema.node_type_from_key("unknown:key").is_none());
  }

  #[test]
  fn test_schema_macro() {
    let schema = schema! {
        nodes: [
            node("user")
                .prop(prop::string("name"))
                .build(),
            node("post")
                .prop(prop::string("title"))
                .build(),
        ],
        edges: [
            edge("follows").build(),
            edge("authored").build(),
        ]
    };

    assert_eq!(schema.node_types().len(), 2);
    assert_eq!(schema.edge_types().len(), 2);
  }

  #[test]
  fn test_required_props() {
    let user = node("user")
      .prop(prop::string("name"))
      .prop(prop::string("email"))
      .prop(prop::int("age").optional())
      .build();

    let required = user.required_props();
    assert_eq!(required.len(), 2);
    assert!(required.contains(&"name"));
    assert!(required.contains(&"email"));
    assert!(!required.contains(&"age"));
  }

  #[test]
  fn test_schema_type_conversion() {
    assert_eq!(SchemaType::String.to_tag(), PropValueTag::String);
    assert_eq!(SchemaType::Int.to_tag(), PropValueTag::I64);
    assert_eq!(SchemaType::Float.to_tag(), PropValueTag::F64);
    assert_eq!(SchemaType::Bool.to_tag(), PropValueTag::Bool);
    assert_eq!(SchemaType::Vector.to_tag(), PropValueTag::VectorF32);
  }

  #[test]
  fn test_vector_prop() {
    let doc = node("document")
      .prop(prop::string("title"))
      .prop(prop::vector("embedding"))
      .build();

    assert!(doc.get_prop("embedding").is_some());
    assert_eq!(
      doc.get_prop("embedding").unwrap().schema_type,
      SchemaType::Vector
    );

    // Validate vector property
    let embedding_prop = prop::vector("embedding");
    assert!(embedding_prop.validate(&PropValue::VectorF32(vec![0.1, 0.2, 0.3])));
    assert!(!embedding_prop.validate(&PropValue::String("not a vector".to_string())));
  }

  #[test]
  fn test_edge_validation() {
    let knows = edge("knows")
      .prop(prop::int("since"))
      .prop(prop::float("weight").optional())
      .build();

    // Valid: required prop present
    let mut props = HashMap::new();
    props.insert("since".to_string(), PropValue::I64(2020));
    assert!(knows.validate(&props).is_ok());

    // Valid: with optional
    props.insert("weight".to_string(), PropValue::F64(0.95));
    assert!(knows.validate(&props).is_ok());

    // Invalid: missing required
    let empty: HashMap<String, PropValue> = HashMap::new();
    assert!(matches!(
      knows.validate(&empty),
      Err(ValidationError::MissingRequired(_))
    ));
  }

  #[test]
  fn test_prop_with_default() {
    let status = prop::string("status").default(PropValue::String("active".to_string()));
    assert!(status.default.is_some());

    // Schema with default should not require the prop
    let user = node("user")
      .prop(prop::string("name"))
      .prop(prop::string("status").default(PropValue::String("active".to_string())))
      .build();

    // Valid without status (has default)
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    assert!(user.validate(&props).is_ok());
  }
}
