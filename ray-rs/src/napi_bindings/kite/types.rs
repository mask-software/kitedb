//! Schema input types for Kite database configuration
//!
//! These types define the schema passed when opening a Kite database,
//! including node types, edge types, and their properties.

use napi_derive::napi;
use std::collections::HashMap;

use super::super::database::{JsPropValue, JsSyncMode};

// =============================================================================
// Schema Input Types
// =============================================================================

/// Property specification for a node or edge type
#[napi(object)]
pub struct JsPropSpec {
  /// Property type: "string", "int", "float", "bool", "vector", "any"
  pub r#type: String,
  /// Whether the property is optional (default: false)
  pub optional: Option<bool>,
  /// Default value if not provided
  pub r#default: Option<JsPropValue>,
}

/// Key specification for a node type
#[napi(object)]
#[derive(Clone)]
pub struct JsKeySpec {
  /// Key generation strategy: "prefix", "template", "parts"
  pub kind: String,
  /// Key prefix (e.g., "User:")
  pub prefix: Option<String>,
  /// Template string with placeholders (e.g., "User:{id}")
  pub template: Option<String>,
  /// Field names for parts-based keys
  pub fields: Option<Vec<String>>,
  /// Separator for parts-based keys (default: ":")
  pub separator: Option<String>,
}

/// Node type specification
#[napi(object)]
pub struct JsNodeSpec {
  /// Name of the node type
  pub name: String,
  /// Key specification (optional, defaults to prefix-based)
  pub key: Option<JsKeySpec>,
  /// Property definitions
  pub props: Option<HashMap<String, JsPropSpec>>,
}

/// Edge type specification
#[napi(object)]
pub struct JsEdgeSpec {
  /// Name of the edge type
  pub name: String,
  /// Property definitions
  pub props: Option<HashMap<String, JsPropSpec>>,
}

/// Options for opening a Kite database
#[napi(object)]
pub struct JsKiteOptions {
  /// Node type definitions
  pub nodes: Vec<JsNodeSpec>,
  /// Edge type definitions
  pub edges: Vec<JsEdgeSpec>,
  /// Open in read-only mode
  pub read_only: Option<bool>,
  /// Create database if it doesn't exist
  pub create_if_missing: Option<bool>,
  /// Enable MVCC (snapshot isolation + conflict detection)
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth
  pub mvcc_max_chain_depth: Option<i64>,
  /// Sync mode: "Full", "Normal", or "Off" (default: "Full")
  pub sync_mode: Option<JsSyncMode>,
}
