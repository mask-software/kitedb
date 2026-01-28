//! NAPI bindings for SingleFileDB
//!
//! Provides Node.js/Bun access to the single-file database format.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB as RustSingleFileDB,
  SingleFileOpenOptions as RustOpenOptions, SyncMode as RustSyncMode,
};
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue};

// ============================================================================
// Sync Mode
// ============================================================================

/// Synchronization mode for WAL writes
///
/// Controls the durability vs performance trade-off for commits.
/// - Full: Fsync on every commit (durable to OS, slowest)
/// - Normal: Fsync only on checkpoint (~1000x faster, safe from app crash)
/// - Off: No fsync (fastest, data may be lost on any crash)
#[napi(string_enum)]
#[derive(Debug)]
pub enum JsSyncMode {
  /// Fsync on every commit (durable to OS, slowest)
  Full,
  /// Fsync on checkpoint only (balanced)
  Normal,
  /// No fsync (fastest, least safe)
  Off,
}

impl From<JsSyncMode> for RustSyncMode {
  fn from(mode: JsSyncMode) -> Self {
    match mode {
      JsSyncMode::Full => RustSyncMode::Full,
      JsSyncMode::Normal => RustSyncMode::Normal,
      JsSyncMode::Off => RustSyncMode::Off,
    }
  }
}

// ============================================================================
// Open Options
// ============================================================================

/// Options for opening a database
#[napi(object)]
#[derive(Debug, Default)]
pub struct OpenOptions {
  /// Open in read-only mode
  pub read_only: Option<bool>,
  /// Create database if it doesn't exist
  pub create_if_missing: Option<bool>,
  /// Page size in bytes (default 4096)
  pub page_size: Option<u32>,
  /// WAL size in bytes (default 1MB)
  pub wal_size: Option<u32>,
  /// Enable auto-checkpoint when WAL usage exceeds threshold
  pub auto_checkpoint: Option<bool>,
  /// WAL usage threshold (0.0-1.0) to trigger auto-checkpoint
  pub checkpoint_threshold: Option<f64>,
  /// Use background (non-blocking) checkpoint
  pub background_checkpoint: Option<bool>,
  /// Enable caching
  pub cache_enabled: Option<bool>,
  /// Max node properties in cache
  pub cache_max_node_props: Option<i64>,
  /// Max edge properties in cache
  pub cache_max_edge_props: Option<i64>,
  /// Max traversal cache entries
  pub cache_max_traversal_entries: Option<i64>,
  /// Max query cache entries
  pub cache_max_query_entries: Option<i64>,
  /// Query cache TTL in milliseconds
  pub cache_query_ttl_ms: Option<i64>,
  /// Sync mode: "Full", "Normal", or "Off" (default: "Full")
  pub sync_mode: Option<JsSyncMode>,
}

impl From<OpenOptions> for RustOpenOptions {
  fn from(opts: OpenOptions) -> Self {
    use crate::types::{CacheOptions, PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};

    let mut rust_opts = RustOpenOptions::new();
    if let Some(v) = opts.read_only {
      rust_opts = rust_opts.read_only(v);
    }
    if let Some(v) = opts.create_if_missing {
      rust_opts = rust_opts.create_if_missing(v);
    }
    if let Some(v) = opts.page_size {
      rust_opts = rust_opts.page_size(v as usize);
    }
    if let Some(v) = opts.wal_size {
      rust_opts = rust_opts.wal_size(v as usize);
    }
    if let Some(v) = opts.auto_checkpoint {
      rust_opts = rust_opts.auto_checkpoint(v);
    }
    if let Some(v) = opts.checkpoint_threshold {
      rust_opts = rust_opts.checkpoint_threshold(v);
    }
    if let Some(v) = opts.background_checkpoint {
      rust_opts = rust_opts.background_checkpoint(v);
    }

    // Cache options
    if opts.cache_enabled == Some(true) {
      let property_cache = Some(PropertyCacheConfig {
        max_node_props: opts.cache_max_node_props.unwrap_or(10000) as usize,
        max_edge_props: opts.cache_max_edge_props.unwrap_or(10000) as usize,
      });

      let traversal_cache = Some(TraversalCacheConfig {
        max_entries: opts.cache_max_traversal_entries.unwrap_or(5000) as usize,
        max_neighbors_per_entry: 100,
      });

      let query_cache = Some(QueryCacheConfig {
        max_entries: opts.cache_max_query_entries.unwrap_or(1000) as usize,
        ttl_ms: opts.cache_query_ttl_ms.map(|v| v as u64),
      });

      rust_opts = rust_opts.cache(Some(CacheOptions {
        enabled: true,
        property_cache,
        traversal_cache,
        query_cache,
      }));
    }

    // Sync mode
    if let Some(mode) = opts.sync_mode {
      rust_opts = rust_opts.sync_mode(mode.into());
    }

    rust_opts
  }
}

// ============================================================================
// Database Statistics
// ============================================================================

/// Database statistics
#[napi(object)]
pub struct DbStats {
  pub snapshot_gen: i64,
  pub snapshot_nodes: i64,
  pub snapshot_edges: i64,
  pub snapshot_max_node_id: i64,
  pub delta_nodes_created: i64,
  pub delta_nodes_deleted: i64,
  pub delta_edges_added: i64,
  pub delta_edges_deleted: i64,
  pub wal_bytes: i64,
  pub recommend_compact: bool,
}

/// Cache statistics
#[napi(object)]
pub struct JsCacheStats {
  pub property_cache_hits: i64,
  pub property_cache_misses: i64,
  pub property_cache_size: i64,
  pub traversal_cache_hits: i64,
  pub traversal_cache_misses: i64,
  pub traversal_cache_size: i64,
  pub query_cache_hits: i64,
  pub query_cache_misses: i64,
  pub query_cache_size: i64,
}

// ============================================================================
// Property Value (JS-compatible)
// ============================================================================

/// Property value types
#[napi(string_enum)]
pub enum PropType {
  Null,
  Bool,
  Int,
  Float,
  String,
}

/// Property value wrapper for JS
#[napi(object)]
pub struct JsPropValue {
  pub prop_type: PropType,
  pub bool_value: Option<bool>,
  pub int_value: Option<i64>,
  pub float_value: Option<f64>,
  pub string_value: Option<String>,
}

impl From<PropValue> for JsPropValue {
  fn from(value: PropValue) -> Self {
    match value {
      PropValue::Null => JsPropValue {
        prop_type: PropType::Null,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: None,
      },
      PropValue::Bool(v) => JsPropValue {
        prop_type: PropType::Bool,
        bool_value: Some(v),
        int_value: None,
        float_value: None,
        string_value: None,
      },
      PropValue::I64(v) => JsPropValue {
        prop_type: PropType::Int,
        bool_value: None,
        int_value: Some(v),
        float_value: None,
        string_value: None,
      },
      PropValue::F64(v) => JsPropValue {
        prop_type: PropType::Float,
        bool_value: None,
        int_value: None,
        float_value: Some(v),
        string_value: None,
      },
      PropValue::String(v) => JsPropValue {
        prop_type: PropType::String,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: Some(v),
      },
      PropValue::VectorF32(_) => JsPropValue {
        // Vector not directly supported in this simple binding
        prop_type: PropType::Null,
        bool_value: None,
        int_value: None,
        float_value: None,
        string_value: None,
      },
    }
  }
}

impl From<JsPropValue> for PropValue {
  fn from(value: JsPropValue) -> Self {
    match value.prop_type {
      PropType::Null => PropValue::Null,
      PropType::Bool => PropValue::Bool(value.bool_value.unwrap_or(false)),
      PropType::Int => PropValue::I64(value.int_value.unwrap_or(0)),
      PropType::Float => PropValue::F64(value.float_value.unwrap_or(0.0)),
      PropType::String => PropValue::String(value.string_value.unwrap_or_default()),
    }
  }
}

// ============================================================================
// Edge Result
// ============================================================================

/// Edge representation for JS (neighbor style)
#[napi(object)]
pub struct JsEdge {
  pub etype: u32,
  pub node_id: i64,
}

/// Full edge representation for JS (src, etype, dst)
#[napi(object)]
pub struct JsFullEdge {
  pub src: i64,
  pub etype: u32,
  pub dst: i64,
}

// ============================================================================
// Node Property Result
// ============================================================================

/// Node property key-value pair for JS
#[napi(object)]
pub struct JsNodeProp {
  pub key_id: u32,
  pub value: JsPropValue,
}

// ============================================================================
// SingleFileDB NAPI Wrapper
// ============================================================================

/// Single-file graph database
#[napi]
pub struct Database {
  inner: Option<RustSingleFileDB>,
}

#[napi]
impl Database {
  /// Open a database file
  #[napi(factory)]
  pub fn open(path: String, options: Option<OpenOptions>) -> Result<Database> {
    let opts: RustOpenOptions = options.unwrap_or_default().into();
    let db = open_single_file(&path, opts)
      .map_err(|e| Error::from_reason(format!("Failed to open database: {e}")))?;
    Ok(Database { inner: Some(db) })
  }

  /// Close the database
  #[napi]
  pub fn close(&mut self) -> Result<()> {
    if let Some(db) = self.inner.take() {
      close_single_file(db)
        .map_err(|e| Error::from_reason(format!("Failed to close database: {e}")))?;
    }
    Ok(())
  }

  /// Check if database is open
  #[napi(getter)]
  pub fn is_open(&self) -> bool {
    self.inner.is_some()
  }

  /// Get database path
  #[napi(getter)]
  pub fn path(&self) -> Result<String> {
    let db = self.get_db()?;
    Ok(db.path.to_string_lossy().to_string())
  }

  /// Check if database is read-only
  #[napi(getter)]
  pub fn read_only(&self) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.read_only)
  }

  // ========================================================================
  // Transaction Methods
  // ========================================================================

  /// Begin a transaction
  #[napi]
  pub fn begin(&self, read_only: Option<bool>) -> Result<i64> {
    let db = self.get_db()?;
    let txid = db
      .begin(read_only.unwrap_or(false))
      .map_err(|e| Error::from_reason(format!("Failed to begin transaction: {e}")))?;
    Ok(txid as i64)
  }

  /// Commit the current transaction
  #[napi]
  pub fn commit(&self) -> Result<()> {
    let db = self.get_db()?;
    db.commit()
      .map_err(|e| Error::from_reason(format!("Failed to commit: {e}")))
  }

  /// Rollback the current transaction
  #[napi]
  pub fn rollback(&self) -> Result<()> {
    let db = self.get_db()?;
    db.rollback()
      .map_err(|e| Error::from_reason(format!("Failed to rollback: {e}")))
  }

  /// Check if there's an active transaction
  #[napi]
  pub fn has_transaction(&self) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.has_transaction())
  }

  // ========================================================================
  // Node Operations
  // ========================================================================

  /// Create a new node
  #[napi]
  pub fn create_node(&self, key: Option<String>) -> Result<i64> {
    let db = self.get_db()?;
    let node_id = db
      .create_node(key.as_deref())
      .map_err(|e| Error::from_reason(format!("Failed to create node: {e}")))?;
    Ok(node_id as i64)
  }

  /// Delete a node
  #[napi]
  pub fn delete_node(&self, node_id: i64) -> Result<()> {
    let db = self.get_db()?;
    db.delete_node(node_id as NodeId)
      .map_err(|e| Error::from_reason(format!("Failed to delete node: {e}")))
  }

  /// Check if a node exists
  #[napi]
  pub fn node_exists(&self, node_id: i64) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.node_exists(node_id as NodeId))
  }

  /// Get node by key
  #[napi]
  pub fn get_node_by_key(&self, key: String) -> Result<Option<i64>> {
    let db = self.get_db()?;
    Ok(db.get_node_by_key(&key).map(|id| id as i64))
  }

  /// Get the key for a node
  #[napi]
  pub fn get_node_key(&self, node_id: i64) -> Result<Option<String>> {
    let db = self.get_db()?;
    Ok(db.get_node_key(node_id as NodeId))
  }

  /// List all node IDs
  #[napi]
  pub fn list_nodes(&self) -> Result<Vec<i64>> {
    let db = self.get_db()?;
    Ok(db.list_nodes().into_iter().map(|id| id as i64).collect())
  }

  /// Count all nodes
  #[napi]
  pub fn count_nodes(&self) -> Result<i64> {
    let db = self.get_db()?;
    Ok(db.count_nodes() as i64)
  }

  // ========================================================================
  // Edge Operations
  // ========================================================================

  /// Add an edge
  #[napi]
  pub fn add_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    let db = self.get_db()?;
    db.add_edge(src as NodeId, etype as ETypeId, dst as NodeId)
      .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}")))
  }

  /// Add an edge by type name
  #[napi]
  pub fn add_edge_by_name(&self, src: i64, etype_name: String, dst: i64) -> Result<()> {
    let db = self.get_db()?;
    db.add_edge_by_name(src as NodeId, &etype_name, dst as NodeId)
      .map_err(|e| Error::from_reason(format!("Failed to add edge: {e}")))
  }

  /// Delete an edge
  #[napi]
  pub fn delete_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    let db = self.get_db()?;
    db.delete_edge(src as NodeId, etype as ETypeId, dst as NodeId)
      .map_err(|e| Error::from_reason(format!("Failed to delete edge: {e}")))
  }

  /// Check if an edge exists
  #[napi]
  pub fn edge_exists(&self, src: i64, etype: u32, dst: i64) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.edge_exists(src as NodeId, etype as ETypeId, dst as NodeId))
  }

  /// Get outgoing edges for a node
  #[napi]
  pub fn get_out_edges(&self, node_id: i64) -> Result<Vec<JsEdge>> {
    let db = self.get_db()?;
    Ok(
      db.get_out_edges(node_id as NodeId)
        .into_iter()
        .map(|(etype, dst)| JsEdge {
          etype,
          node_id: dst as i64,
        })
        .collect(),
    )
  }

  /// Get incoming edges for a node
  #[napi]
  pub fn get_in_edges(&self, node_id: i64) -> Result<Vec<JsEdge>> {
    let db = self.get_db()?;
    Ok(
      db.get_in_edges(node_id as NodeId)
        .into_iter()
        .map(|(etype, src)| JsEdge {
          etype,
          node_id: src as i64,
        })
        .collect(),
    )
  }

  /// Get out-degree for a node
  #[napi]
  pub fn get_out_degree(&self, node_id: i64) -> Result<i64> {
    let db = self.get_db()?;
    Ok(db.get_out_degree(node_id as NodeId) as i64)
  }

  /// Get in-degree for a node
  #[napi]
  pub fn get_in_degree(&self, node_id: i64) -> Result<i64> {
    let db = self.get_db()?;
    Ok(db.get_in_degree(node_id as NodeId) as i64)
  }

  /// Count all edges
  #[napi]
  pub fn count_edges(&self) -> Result<i64> {
    let db = self.get_db()?;
    Ok(db.count_edges() as i64)
  }

  /// List all edges in the database
  ///
  /// Returns an array of {src, etype, dst} objects representing all edges.
  /// Optionally filter by edge type.
  #[napi]
  pub fn list_edges(&self, etype: Option<u32>) -> Result<Vec<JsFullEdge>> {
    let db = self.get_db()?;
    Ok(
      db.list_edges(etype)
        .into_iter()
        .map(|e| JsFullEdge {
          src: e.src as i64,
          etype: e.etype,
          dst: e.dst as i64,
        })
        .collect(),
    )
  }

  /// List edges by type name
  ///
  /// Returns an array of {src, etype, dst} objects for the given edge type.
  #[napi]
  pub fn list_edges_by_name(&self, etype_name: String) -> Result<Vec<JsFullEdge>> {
    let db = self.get_db()?;
    let etype = db
      .get_etype_id(&etype_name)
      .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
    Ok(
      db.list_edges(Some(etype))
        .into_iter()
        .map(|e| JsFullEdge {
          src: e.src as i64,
          etype: e.etype,
          dst: e.dst as i64,
        })
        .collect(),
    )
  }

  /// Count edges by type
  #[napi]
  pub fn count_edges_by_type(&self, etype: u32) -> Result<i64> {
    let db = self.get_db()?;
    Ok(db.count_edges_by_type(etype) as i64)
  }

  /// Count edges by type name
  #[napi]
  pub fn count_edges_by_name(&self, etype_name: String) -> Result<i64> {
    let db = self.get_db()?;
    let etype = db
      .get_etype_id(&etype_name)
      .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {etype_name}")))?;
    Ok(db.count_edges_by_type(etype) as i64)
  }

  // ========================================================================
  // Property Operations
  // ========================================================================

  /// Set a node property
  #[napi]
  pub fn set_node_prop(&self, node_id: i64, key_id: u32, value: JsPropValue) -> Result<()> {
    let db = self.get_db()?;
    db.set_node_prop(node_id as NodeId, key_id as PropKeyId, value.into())
      .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))
  }

  /// Set a node property by key name
  #[napi]
  pub fn set_node_prop_by_name(
    &self,
    node_id: i64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    let db = self.get_db()?;
    db.set_node_prop_by_name(node_id as NodeId, &key_name, value.into())
      .map_err(|e| Error::from_reason(format!("Failed to set property: {e}")))
  }

  /// Delete a node property
  #[napi]
  pub fn delete_node_prop(&self, node_id: i64, key_id: u32) -> Result<()> {
    let db = self.get_db()?;
    db.delete_node_prop(node_id as NodeId, key_id as PropKeyId)
      .map_err(|e| Error::from_reason(format!("Failed to delete property: {e}")))
  }

  /// Get a specific node property
  #[napi]
  pub fn get_node_prop(&self, node_id: i64, key_id: u32) -> Result<Option<JsPropValue>> {
    let db = self.get_db()?;
    Ok(
      db.get_node_prop(node_id as NodeId, key_id as PropKeyId)
        .map(|v| v.into()),
    )
  }

  /// Get all properties for a node (returns array of {key_id, value} pairs)
  #[napi]
  pub fn get_node_props(&self, node_id: i64) -> Result<Option<Vec<JsNodeProp>>> {
    let db = self.get_db()?;
    Ok(db.get_node_props(node_id as NodeId).map(|props| {
      props
        .into_iter()
        .map(|(k, v)| JsNodeProp {
          key_id: k,
          value: v.into(),
        })
        .collect()
    }))
  }

  // ========================================================================
  // Edge Property Operations
  // ========================================================================

  /// Set an edge property
  #[napi]
  pub fn set_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
    value: JsPropValue,
  ) -> Result<()> {
    let db = self.get_db()?;
    db.set_edge_prop(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      key_id as PropKeyId,
      value.into(),
    )
    .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}")))
  }

  /// Set an edge property by key name
  #[napi]
  pub fn set_edge_prop_by_name(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_name: String,
    value: JsPropValue,
  ) -> Result<()> {
    let db = self.get_db()?;
    db.set_edge_prop_by_name(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      &key_name,
      value.into(),
    )
    .map_err(|e| Error::from_reason(format!("Failed to set edge property: {e}")))
  }

  /// Delete an edge property
  #[napi]
  pub fn delete_edge_prop(&self, src: i64, etype: u32, dst: i64, key_id: u32) -> Result<()> {
    let db = self.get_db()?;
    db.delete_edge_prop(
      src as NodeId,
      etype as ETypeId,
      dst as NodeId,
      key_id as PropKeyId,
    )
    .map_err(|e| Error::from_reason(format!("Failed to delete edge property: {e}")))
  }

  /// Get a specific edge property
  #[napi]
  pub fn get_edge_prop(
    &self,
    src: i64,
    etype: u32,
    dst: i64,
    key_id: u32,
  ) -> Result<Option<JsPropValue>> {
    let db = self.get_db()?;
    Ok(
      db.get_edge_prop(
        src as NodeId,
        etype as ETypeId,
        dst as NodeId,
        key_id as PropKeyId,
      )
      .map(|v| v.into()),
    )
  }

  /// Get all properties for an edge (returns array of {key_id, value} pairs)
  #[napi]
  pub fn get_edge_props(&self, src: i64, etype: u32, dst: i64) -> Result<Option<Vec<JsNodeProp>>> {
    let db = self.get_db()?;
    Ok(
      db.get_edge_props(src as NodeId, etype as ETypeId, dst as NodeId)
        .map(|props| {
          props
            .into_iter()
            .map(|(k, v)| JsNodeProp {
              key_id: k,
              value: v.into(),
            })
            .collect()
        }),
    )
  }

  // ========================================================================
  // Vector Operations
  // ========================================================================

  /// Set a vector embedding for a node
  #[napi]
  pub fn set_node_vector(&self, node_id: i64, prop_key_id: u32, vector: Vec<f64>) -> Result<()> {
    let db = self.get_db()?;
    // Convert f64 to f32
    let vector_f32: Vec<f32> = vector.iter().map(|&v| v as f32).collect();
    db.set_node_vector(node_id as NodeId, prop_key_id as PropKeyId, &vector_f32)
      .map_err(|e| Error::from_reason(format!("Failed to set vector: {e}")))
  }

  /// Get a vector embedding for a node
  #[napi]
  pub fn get_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<Option<Vec<f64>>> {
    let db = self.get_db()?;
    Ok(
      db.get_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
        .map(|v| v.iter().map(|&f| f as f64).collect()),
    )
  }

  /// Delete a vector embedding for a node
  #[napi]
  pub fn delete_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<()> {
    let db = self.get_db()?;
    db.delete_node_vector(node_id as NodeId, prop_key_id as PropKeyId)
      .map_err(|e| Error::from_reason(format!("Failed to delete vector: {e}")))
  }

  /// Check if a node has a vector embedding
  #[napi]
  pub fn has_node_vector(&self, node_id: i64, prop_key_id: u32) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.has_node_vector(node_id as NodeId, prop_key_id as PropKeyId))
  }

  // ========================================================================
  // Schema Operations
  // ========================================================================

  /// Get or create a label ID
  #[napi]
  pub fn get_or_create_label(&self, name: String) -> Result<u32> {
    let db = self.get_db()?;
    Ok(db.get_or_create_label(&name))
  }

  /// Get label ID by name
  #[napi]
  pub fn get_label_id(&self, name: String) -> Result<Option<u32>> {
    let db = self.get_db()?;
    Ok(db.get_label_id(&name))
  }

  /// Get label name by ID
  #[napi]
  pub fn get_label_name(&self, id: u32) -> Result<Option<String>> {
    let db = self.get_db()?;
    Ok(db.get_label_name(id))
  }

  /// Get or create an edge type ID
  #[napi]
  pub fn get_or_create_etype(&self, name: String) -> Result<u32> {
    let db = self.get_db()?;
    Ok(db.get_or_create_etype(&name))
  }

  /// Get edge type ID by name
  #[napi]
  pub fn get_etype_id(&self, name: String) -> Result<Option<u32>> {
    let db = self.get_db()?;
    Ok(db.get_etype_id(&name))
  }

  /// Get edge type name by ID
  #[napi]
  pub fn get_etype_name(&self, id: u32) -> Result<Option<String>> {
    let db = self.get_db()?;
    Ok(db.get_etype_name(id))
  }

  /// Get or create a property key ID
  #[napi]
  pub fn get_or_create_propkey(&self, name: String) -> Result<u32> {
    let db = self.get_db()?;
    Ok(db.get_or_create_propkey(&name))
  }

  /// Get property key ID by name
  #[napi]
  pub fn get_propkey_id(&self, name: String) -> Result<Option<u32>> {
    let db = self.get_db()?;
    Ok(db.get_propkey_id(&name))
  }

  /// Get property key name by ID
  #[napi]
  pub fn get_propkey_name(&self, id: u32) -> Result<Option<String>> {
    let db = self.get_db()?;
    Ok(db.get_propkey_name(id))
  }

  // ========================================================================
  // Node Label Operations
  // ========================================================================

  /// Define a new label (requires transaction)
  #[napi]
  pub fn define_label(&self, name: String) -> Result<u32> {
    let db = self.get_db()?;
    db.define_label(&name)
      .map_err(|e| Error::from_reason(format!("Failed to define label: {e}")))
  }

  /// Add a label to a node
  #[napi]
  pub fn add_node_label(&self, node_id: i64, label_id: u32) -> Result<()> {
    let db = self.get_db()?;
    db.add_node_label(node_id as NodeId, label_id)
      .map_err(|e| Error::from_reason(format!("Failed to add label: {e}")))
  }

  /// Add a label to a node by name
  #[napi]
  pub fn add_node_label_by_name(&self, node_id: i64, label_name: String) -> Result<()> {
    let db = self.get_db()?;
    db.add_node_label_by_name(node_id as NodeId, &label_name)
      .map_err(|e| Error::from_reason(format!("Failed to add label: {e}")))
  }

  /// Remove a label from a node
  #[napi]
  pub fn remove_node_label(&self, node_id: i64, label_id: u32) -> Result<()> {
    let db = self.get_db()?;
    db.remove_node_label(node_id as NodeId, label_id)
      .map_err(|e| Error::from_reason(format!("Failed to remove label: {e}")))
  }

  /// Check if a node has a label
  #[napi]
  pub fn node_has_label(&self, node_id: i64, label_id: u32) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.node_has_label(node_id as NodeId, label_id))
  }

  /// Get all labels for a node
  #[napi]
  pub fn get_node_labels(&self, node_id: i64) -> Result<Vec<u32>> {
    let db = self.get_db()?;
    Ok(db.get_node_labels(node_id as NodeId))
  }

  // ========================================================================
  // Checkpoint / Maintenance
  // ========================================================================

  /// Perform a checkpoint (compact WAL into snapshot)
  #[napi]
  pub fn checkpoint(&self) -> Result<()> {
    let db = self.get_db()?;
    db.checkpoint()
      .map_err(|e| Error::from_reason(format!("Failed to checkpoint: {e}")))
  }

  /// Perform a background (non-blocking) checkpoint
  #[napi]
  pub fn background_checkpoint(&self) -> Result<()> {
    let db = self.get_db()?;
    db.background_checkpoint()
      .map_err(|e| Error::from_reason(format!("Failed to background checkpoint: {e}")))
  }

  /// Check if checkpoint is recommended
  #[napi]
  pub fn should_checkpoint(&self, threshold: Option<f64>) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.should_checkpoint(threshold.unwrap_or(0.8)))
  }

  /// Optimize (compact) the database
  ///
  /// This is an alias for `checkpoint()` to match the TypeScript API.
  /// For single-file databases, optimization means merging the WAL into
  /// the snapshot, which reduces file size and improves read performance.
  #[napi]
  pub fn optimize(&self) -> Result<()> {
    let db = self.get_db()?;
    db.checkpoint()
      .map_err(|e| Error::from_reason(format!("Failed to optimize: {e}")))
  }

  /// Get database statistics
  #[napi]
  pub fn stats(&self) -> Result<DbStats> {
    let db = self.get_db()?;
    let s = db.stats();
    Ok(DbStats {
      snapshot_gen: s.snapshot_gen as i64,
      snapshot_nodes: s.snapshot_nodes as i64,
      snapshot_edges: s.snapshot_edges as i64,
      snapshot_max_node_id: s.snapshot_max_node_id as i64,
      delta_nodes_created: s.delta_nodes_created as i64,
      delta_nodes_deleted: s.delta_nodes_deleted as i64,
      delta_edges_added: s.delta_edges_added as i64,
      delta_edges_deleted: s.delta_edges_deleted as i64,
      wal_bytes: s.wal_bytes as i64,
      recommend_compact: s.recommend_compact,
    })
  }

  // ========================================================================
  // Cache Operations
  // ========================================================================

  /// Check if caching is enabled
  #[napi]
  pub fn cache_is_enabled(&self) -> Result<bool> {
    let db = self.get_db()?;
    Ok(db.cache_is_enabled())
  }

  /// Invalidate all caches for a node
  #[napi]
  pub fn cache_invalidate_node(&self, node_id: i64) -> Result<()> {
    let db = self.get_db()?;
    db.cache_invalidate_node(node_id as NodeId);
    Ok(())
  }

  /// Invalidate caches for a specific edge
  #[napi]
  pub fn cache_invalidate_edge(&self, src: i64, etype: u32, dst: i64) -> Result<()> {
    let db = self.get_db()?;
    db.cache_invalidate_edge(src as NodeId, etype as ETypeId, dst as NodeId);
    Ok(())
  }

  /// Invalidate a cached key lookup
  #[napi]
  pub fn cache_invalidate_key(&self, key: String) -> Result<()> {
    let db = self.get_db()?;
    db.cache_invalidate_key(&key);
    Ok(())
  }

  /// Clear all caches
  #[napi]
  pub fn cache_clear(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_clear();
    Ok(())
  }

  /// Clear only the query cache
  #[napi]
  pub fn cache_clear_query(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_clear_query();
    Ok(())
  }

  /// Clear only the key cache
  #[napi]
  pub fn cache_clear_key(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_clear_key();
    Ok(())
  }

  /// Clear only the property cache
  #[napi]
  pub fn cache_clear_property(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_clear_property();
    Ok(())
  }

  /// Clear only the traversal cache
  #[napi]
  pub fn cache_clear_traversal(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_clear_traversal();
    Ok(())
  }

  /// Get cache statistics
  #[napi]
  pub fn cache_stats(&self) -> Result<Option<JsCacheStats>> {
    let db = self.get_db()?;
    Ok(db.cache_stats().map(|s| JsCacheStats {
      property_cache_hits: s.property_cache_hits as i64,
      property_cache_misses: s.property_cache_misses as i64,
      property_cache_size: s.property_cache_size as i64,
      traversal_cache_hits: s.traversal_cache_hits as i64,
      traversal_cache_misses: s.traversal_cache_misses as i64,
      traversal_cache_size: s.traversal_cache_size as i64,
      query_cache_hits: s.query_cache_hits as i64,
      query_cache_misses: s.query_cache_misses as i64,
      query_cache_size: s.query_cache_size as i64,
    }))
  }

  /// Reset cache statistics
  #[napi]
  pub fn cache_reset_stats(&self) -> Result<()> {
    let db = self.get_db()?;
    db.cache_reset_stats();
    Ok(())
  }

  // ========================================================================
  // Internal Helpers
  // ========================================================================

  fn get_db(&self) -> Result<&RustSingleFileDB> {
    self
      .inner
      .as_ref()
      .ok_or_else(|| Error::from_reason("Database is closed"))
  }
}

// ============================================================================
// Convenience Functions
// ============================================================================

/// Open a database file (standalone function)
#[napi]
pub fn open_database(path: String, options: Option<OpenOptions>) -> Result<Database> {
  Database::open(path, options)
}
