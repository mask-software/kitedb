//! Schema input types for Kite database configuration
//!
//! These types define the schema passed when opening a Kite database,
//! including node types, edge types, and their properties.

use napi_derive::napi;
use std::collections::HashMap;

use super::super::database::{JsPropValue, JsReplicationRole, JsSyncMode};

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
  /// MVCC: snapshot-isolated transactions and conflict detection between
  /// concurrent write transactions (default: true). `false` is deprecated
  /// and will be removed in a later release.
  pub mvcc: Option<bool>,
  /// MVCC GC interval in ms
  pub mvcc_gc_interval_ms: Option<i64>,
  /// MVCC retention in ms (0 means retain no historical window)
  pub mvcc_retention_ms: Option<i64>,
  /// MVCC max version chain depth (must be positive)
  pub mvcc_max_chain_depth: Option<i64>,
  /// Sync mode: "Full", "Normal", or "Off" (default: "Full")
  pub sync_mode: Option<JsSyncMode>,
  /// Has no effect, kept for compatibility: every commit is group-committed
  /// (commits that arrive while others are written are written together, in
  /// every sync mode)
  pub group_commit_enabled: Option<bool>,
  /// Has no effect, kept for compatibility: no commit waits for others to
  /// join its group
  pub group_commit_window_ms: Option<i64>,
  /// WAL size in megabytes (must be positive), fixed when the file is created.
  /// Unset: a new file gets a 4MB WAL and an existing file keeps its own.
  /// Set: a new file gets this size; an existing file with a different WAL
  /// size fails to open.
  pub wal_size_mb: Option<i64>,
  /// @deprecated No effect: automatic checkpoints follow the log (see `checkpointLogRatio` and `checkpointLogBudget`). Still accepted (in [0, 1]) so existing callers keep working.
  pub checkpoint_threshold: Option<f64>,
  /// Run automatic checkpoints on a thread of the database's own (default:
  /// true)
  pub checkpoint_thread: Option<bool>,
  /// Checkpoint once the log the snapshot does not cover reaches this
  /// fraction of the snapshot's size (default: 0.5)
  pub checkpoint_log_ratio: Option<f64>,
  /// The most log, in bytes, an automatic checkpoint waits for (default:
  /// 128 MiB; the in-memory delta takes about ten times the log's size)
  pub checkpoint_log_budget: Option<f64>,
  /// Bytes of a WAL segment extent (default: a sixteenth of the segment
  /// limit, from two WALs to 32 MiB)
  pub wal_segment_size: Option<f64>,
  /// The most bytes of WAL segments before writers wait for a checkpoint
  pub wal_segment_limit: Option<f64>,
  /// On close, checkpoint if the log the snapshot does not cover is at least
  /// this fraction of the checkpoint trigger (default: 0.2)
  pub close_checkpoint_if_wal_usage_at_least: Option<f64>,
  /// Replication role: "Disabled", "Primary", or "Replica"
  pub replication_role: Option<JsReplicationRole>,
  /// Replication sidecar path override
  pub replication_sidecar_path: Option<String>,
  /// Source primary db path (replica role only)
  pub replication_source_db_path: Option<String>,
  /// Source primary sidecar path (replica role only)
  pub replication_source_sidecar_path: Option<String>,
  /// Segment rotation threshold in bytes (primary role only)
  pub replication_segment_max_bytes: Option<i64>,
  /// Minimum retained entries window (0 imposes no entry-count floor)
  pub replication_retention_min_entries: Option<i64>,
  /// Minimum retained segment age in milliseconds (0 imposes no age floor)
  pub replication_retention_min_ms: Option<i64>,
  /// Enforce node schemas on writes (default: false). Creating a node fails if a required prop
  /// (any prop not marked optional) is missing or null, and every node write fails if a
  /// declared prop's value does not match its type (int<->float only when lossless). Props
  /// outside the schema are kept, and declared defaults are applied on create in both modes.
  pub strict_schema: Option<bool>,
}
