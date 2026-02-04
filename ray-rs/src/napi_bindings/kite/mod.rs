//! NAPI bindings for the high-level Kite API
//!
//! This module provides a fluent, type-safe API for building and querying
//! graph databases from Node.js/Bun.

mod builders;
mod conversion;
mod helpers;
mod key_spec;
mod kite_traversal;
mod pathfinding;
mod types;

// Re-export public types
pub use builders::{
  KiteInsertBuilder, KiteInsertExecutorMany, KiteInsertExecutorSingle, KiteUpdateBuilder,
  KiteUpdateEdgeBuilder, KiteUpsertBuilder, KiteUpsertByIdBuilder, KiteUpsertEdgeBuilder,
  KiteUpsertExecutorMany, KiteUpsertExecutorSingle,
};
pub use kite_traversal::KiteTraversal;
pub use pathfinding::{JsPathEdge, JsPathResult, KitePath};
pub use types::{JsEdgeSpec, JsKeySpec, JsKiteOptions, JsNodeSpec, JsPropSpec};

// Internal imports
use conversion::js_props_to_map;
use helpers::{
  batch_result_to_js, execute_batch_ops, get_node_props, get_node_props_selected, node_to_js,
};
use key_spec::{parse_key_spec, prop_spec_to_def, KeySpec};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::api::kite::{BatchOp, EdgeDef, Kite as RustKite, KiteOptions, NodeDef};
use crate::types::NodeId;

use super::database::{CheckResult, DbStats, MvccStats};
use super::database::{JsFullEdge, JsPropValue};

use conversion::{js_value_to_prop_value, key_suffix_from_js};

// =============================================================================
// Kite Handle
// =============================================================================

/// High-level Kite database handle for Node.js/Bun.
///
/// # Thread Safety and Concurrent Access
///
/// Kite uses an internal RwLock to support concurrent operations:
///
/// - **Read operations** (get, exists, neighbors, traversals) use a shared read lock,
///   allowing multiple concurrent reads without blocking each other.
/// - **Write operations** (insert, update, link, delete) use an exclusive write lock,
///   blocking all other operations until complete.
///
/// This means you can safely call multiple read methods concurrently:
///
/// ```javascript
/// // These execute concurrently - reads don't block each other
/// const [user1, user2, user3] = await Promise.all([
///   db.get("User", "alice"),
///   db.get("User", "bob"),
///   db.get("User", "charlie"),
/// ]);
/// ```
///
/// Write operations will wait for in-progress reads and block new operations:
///
/// ```javascript
/// // This will wait for any in-progress reads, then block new reads
/// await db.insert("User").key("david").set("name", "David").execute();
/// ```
#[napi]
pub struct Kite {
  inner: Arc<RwLock<Option<RustKite>>>,
  node_specs: Arc<HashMap<String, Arc<KeySpec>>>,
}

impl Kite {
  /// Execute a read operation with a shared lock.
  /// Multiple read operations can execute concurrently.
  fn with_kite<R>(&self, f: impl FnOnce(&RustKite) -> Result<R>) -> Result<R> {
    let guard = self.inner.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    f(ray)
  }

  /// Execute a write operation with an exclusive lock.
  /// This blocks all other operations until complete.
  fn with_kite_mut<R>(&self, f: impl FnOnce(&mut RustKite) -> Result<R>) -> Result<R> {
    let mut guard = self.inner.write();
    let ray = guard
      .as_mut()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    f(ray)
  }

  fn key_spec(&self, node_type: &str) -> Result<&Arc<KeySpec>> {
    self
      .node_specs
      .get(node_type)
      .ok_or_else(|| Error::from_reason(format!("Unknown node type: {node_type}")))
  }
}

#[napi]
impl Kite {
  /// Open a Kite database
  #[allow(clippy::arc_with_non_send_sync)]
  #[napi(factory)]
  pub fn open(path: String, options: JsKiteOptions) -> Result<Self> {
    let mut node_specs: HashMap<String, Arc<KeySpec>> = HashMap::new();
    let mut kite_opts = KiteOptions::new();
    kite_opts.read_only = options.read_only.unwrap_or(false);
    kite_opts.create_if_missing = options.create_if_missing.unwrap_or(true);
    kite_opts.mvcc = options.mvcc.unwrap_or(false);
    kite_opts.mvcc_gc_interval_ms = options.mvcc_gc_interval_ms.map(|v| v as u64);
    kite_opts.mvcc_retention_ms = options.mvcc_retention_ms.map(|v| v as u64);
    kite_opts.mvcc_max_chain_depth = options.mvcc_max_chain_depth.map(|v| v as usize);
    if let Some(mode) = options.sync_mode {
      kite_opts.sync_mode = mode.into();
    }
    if let Some(wal_size_mb) = options.wal_size_mb {
      if wal_size_mb > 0 {
        kite_opts.wal_size = Some((wal_size_mb as usize).saturating_mul(1024 * 1024));
      }
    }
    if let Some(threshold) = options.checkpoint_threshold {
      kite_opts.checkpoint_threshold = Some(threshold.clamp(0.0, 1.0));
    }

    for node in options.nodes {
      let key_spec = Arc::new(parse_key_spec(&node.name, node.key)?);
      let prefix = key_spec.prefix().to_string();

      let mut node_def = NodeDef::new(&node.name, &prefix);
      if let Some(props) = node.props.as_ref() {
        for (prop_name, prop_spec) in props {
          node_def = node_def.prop(prop_spec_to_def(prop_name, prop_spec)?);
        }
      }

      node_specs.insert(node.name.clone(), Arc::clone(&key_spec));
      kite_opts.nodes.push(node_def);
    }

    for edge in options.edges {
      let mut edge_def = EdgeDef::new(&edge.name);
      if let Some(props) = edge.props.as_ref() {
        for (prop_name, prop_spec) in props {
          edge_def = edge_def.prop(prop_spec_to_def(prop_name, prop_spec)?);
        }
      }
      kite_opts.edges.push(edge_def);
    }

    let ray = RustKite::open(path, kite_opts).map_err(|e| Error::from_reason(e.to_string()))?;

    Ok(Kite {
      inner: Arc::new(RwLock::new(Some(ray))),
      node_specs: Arc::new(node_specs),
    })
  }

  /// Close the database
  #[napi]
  pub fn close(&self) -> Result<()> {
    let mut guard = self.inner.write();
    if let Some(ray) = guard.as_ref() {
      if ray.raw().has_transaction() {
        ray
          .raw()
          .rollback()
          .map_err(|e| Error::from_reason(format!("Failed to rollback: {e}")))?;
      }
    }

    if let Some(ray) = guard.take() {
      ray.close().map_err(|e| Error::from_reason(e.to_string()))?;
    }
    Ok(())
  }

  /// Get a node by key (returns node object with props)
  #[napi]
  pub fn get(
    &self,
    env: Env,
    node_type: String,
    key: Unknown,
    props: Option<Vec<String>>,
  ) -> Result<Option<Object>> {
    let key_suffix = {
      let spec = self.key_spec(&node_type)?;
      key_suffix_from_js(&env, spec.as_ref(), key)?
    };
    let selected_props = props.map(|props| props.into_iter().collect::<HashSet<String>>());
    self.with_kite(move |ray| {
      let node_ref = ray
        .get(&node_type, &key_suffix)
        .map_err(|e| Error::from_reason(e.to_string()))?;

      match node_ref {
        Some(node_ref) => {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let props = get_node_props_selected(ray, node_id, selected_props.as_ref());
          let obj = node_to_js(&env, node_id, node_key, &node_type, props)?;
          Ok(Some(obj))
        }
        None => Ok(None),
      }
    })
  }

  /// Get a node by ID (returns node object with props)
  #[napi]
  pub fn get_by_id(
    &self,
    env: Env,
    node_id: i64,
    props: Option<Vec<String>>,
  ) -> Result<Option<Object>> {
    let selected_props = props.map(|props| props.into_iter().collect::<HashSet<String>>());
    self.with_kite(move |ray| {
      let node_ref = ray
        .node_by_id(node_id as NodeId)
        .map_err(|e| Error::from_reason(e.to_string()))?;
      match node_ref {
        Some(node_ref) => {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let props = get_node_props_selected(ray, node_id, selected_props.as_ref());
          let obj = node_to_js(&env, node_id, node_key, &node_type, props)?;
          Ok(Some(obj))
        }
        None => Ok(None),
      }
    })
  }

  /// Get a lightweight node reference by key (no properties)
  #[napi]
  pub fn get_ref(&self, env: Env, node_type: String, key: Unknown) -> Result<Option<Object>> {
    let key_suffix = {
      let spec = self.key_spec(&node_type)?;
      key_suffix_from_js(&env, spec.as_ref(), key)?
    };
    self.with_kite(move |ray| {
      let node_ref = ray
        .node_ref(&node_type, &key_suffix)
        .map_err(|e| Error::from_reason(e.to_string()))?;

      match node_ref {
        Some(node_ref) => {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let obj = node_to_js(&env, node_id, node_key, &node_type, HashMap::new())?;
          Ok(Some(obj))
        }
        None => Ok(None),
      }
    })
  }

  /// Get a node ID by key (no properties)
  #[napi]
  pub fn get_id(&self, env: Env, node_type: String, key: Unknown) -> Result<Option<i64>> {
    let key_suffix = {
      let spec = self.key_spec(&node_type)?;
      key_suffix_from_js(&env, spec.as_ref(), key)?
    };
    self.with_kite(move |ray| {
      Ok(
        ray
          .get(&node_type, &key_suffix)
          .map_err(|e| Error::from_reason(e.to_string()))?
          .map(|node| node.id() as i64),
      )
    })
  }

  /// Get multiple nodes by ID (returns node objects with props)
  #[napi]
  pub fn get_by_ids(
    &self,
    env: Env,
    node_ids: Vec<i64>,
    props: Option<Vec<String>>,
  ) -> Result<Vec<Object>> {
    if node_ids.is_empty() {
      return Ok(Vec::new());
    }

    let selected_props = props.map(|props| props.into_iter().collect::<HashSet<String>>());
    self.with_kite(move |ray| {
      let mut out = Vec::with_capacity(node_ids.len());
      for node_id in node_ids {
        let node_ref = ray
          .node_by_id(node_id as NodeId)
          .map_err(|e| Error::from_reason(e.to_string()))?;
        if let Some(node_ref) = node_ref {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let props = get_node_props_selected(ray, node_id, selected_props.as_ref());
          out.push(node_to_js(&env, node_id, node_key, &node_type, props)?);
        }
      }
      Ok(out)
    })
  }

  /// Get a node property value
  #[napi]
  pub fn get_prop(&self, node_id: i64, prop_name: String) -> Result<Option<JsPropValue>> {
    let value = self.with_kite(|ray| Ok(ray.prop(node_id as NodeId, &prop_name)))?;
    Ok(value.map(JsPropValue::from))
  }

  /// Set a node property value
  #[napi]
  pub fn set_prop(&self, env: Env, node_id: i64, prop_name: String, value: Unknown) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.with_kite_mut(|ray| {
      ray
        .set_prop(node_id as NodeId, &prop_name, prop_value)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Set multiple node property values
  #[napi]
  pub fn set_props(&self, env: Env, node_id: i64, props: Object) -> Result<()> {
    let props_map = js_props_to_map(&env, Some(props))?;
    self.with_kite_mut(|ray| {
      ray
        .set_props(node_id as NodeId, props_map)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Check if a node exists
  #[napi]
  pub fn exists(&self, node_id: i64) -> Result<bool> {
    self.with_kite(|ray| Ok(ray.exists(node_id as NodeId)))
  }

  /// Delete a node by ID
  #[napi]
  pub fn delete_by_id(&self, node_id: i64) -> Result<bool> {
    self.with_kite_mut(|ray| {
      ray
        .delete_node(node_id as NodeId)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Delete a node by key
  #[napi]
  pub fn delete_by_key(&self, env: Env, node_type: String, key: Unknown) -> Result<bool> {
    let key_suffix = {
      let spec = self.key_spec(&node_type)?;
      key_suffix_from_js(&env, spec.as_ref(), key)?
    };
    self.with_kite_mut(|ray| {
      let full_key = ray
        .node_def(&node_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown node type: {node_type}")))?
        .key(&key_suffix);
      let node_id = ray.raw().node_by_key(&full_key);
      match node_id {
        Some(id) => {
          let res = ray
            .delete_node(id)
            .map_err(|e| Error::from_reason(e.to_string()))?;
          Ok(res)
        }
        None => Ok(false),
      }
    })
  }

  /// Create an insert builder
  #[napi]
  pub fn insert(&self, node_type: String) -> Result<KiteInsertBuilder> {
    let spec = Arc::clone(self.key_spec(&node_type)?);
    let prefix = spec.prefix().to_string();
    Ok(KiteInsertBuilder::new(
      self.inner.clone(),
      node_type,
      prefix,
      spec,
    ))
  }

  /// Create an upsert builder
  #[napi]
  pub fn upsert(&self, node_type: String) -> Result<KiteUpsertBuilder> {
    let spec = Arc::clone(self.key_spec(&node_type)?);
    let prefix = spec.prefix().to_string();
    Ok(KiteUpsertBuilder::new(
      self.inner.clone(),
      node_type,
      prefix,
      spec,
    ))
  }

  /// Create an update builder by node ID
  #[napi]
  pub fn update_by_id(&self, node_id: i64) -> Result<KiteUpdateBuilder> {
    Ok(KiteUpdateBuilder::new(
      self.inner.clone(),
      node_id as NodeId,
    ))
  }

  /// Create an upsert builder by node ID
  #[napi]
  pub fn upsert_by_id(&self, node_type: String, node_id: i64) -> Result<KiteUpsertByIdBuilder> {
    Ok(KiteUpsertByIdBuilder::new(
      self.inner.clone(),
      node_type,
      node_id as NodeId,
    ))
  }

  /// Create an update builder by key
  #[napi]
  pub fn update_by_key(
    &self,
    env: Env,
    node_type: String,
    key: Unknown,
  ) -> Result<KiteUpdateBuilder> {
    let key_suffix = {
      let spec = self.key_spec(&node_type)?;
      key_suffix_from_js(&env, spec.as_ref(), key)?
    };
    self.with_kite(|ray| {
      let node_ref = ray
        .get(&node_type, &key_suffix)
        .map_err(|e| Error::from_reason(e.to_string()))?;
      match node_ref {
        Some(node_ref) => Ok(KiteUpdateBuilder::new(self.inner.clone(), node_ref.id())),
        None => Err(Error::from_reason("Key not found")),
      }
    })
  }

  /// Link two nodes
  #[napi]
  pub fn link(
    &self,
    env: Env,
    src: i64,
    edge_type: String,
    dst: i64,
    props: Option<Object>,
  ) -> Result<()> {
    let props_map = js_props_to_map(&env, props)?;
    self.with_kite_mut(|ray| {
      if props_map.is_empty() {
        ray
          .link(src as NodeId, &edge_type, dst as NodeId)
          .map_err(|e| Error::from_reason(e.to_string()))
      } else {
        ray
          .link_with_props(src as NodeId, &edge_type, dst as NodeId, props_map)
          .map_err(|e| Error::from_reason(e.to_string()))
      }
    })
  }

  /// Unlink two nodes
  #[napi]
  pub fn unlink(&self, src: i64, edge_type: String, dst: i64) -> Result<bool> {
    self.with_kite_mut(|ray| {
      ray
        .unlink(src as NodeId, &edge_type, dst as NodeId)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Check if an edge exists
  #[napi]
  pub fn has_edge(&self, src: i64, edge_type: String, dst: i64) -> Result<bool> {
    self.with_kite(move |ray| {
      ray
        .has_edge(src as NodeId, &edge_type, dst as NodeId)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Get an edge property value
  #[napi]
  pub fn get_edge_prop(
    &self,
    src: i64,
    edge_type: String,
    dst: i64,
    prop_name: String,
  ) -> Result<Option<JsPropValue>> {
    let value = self.with_kite(|ray| {
      ray
        .edge_prop(src as NodeId, &edge_type, dst as NodeId, &prop_name)
        .map_err(|e| Error::from_reason(e.to_string()))
    })?;
    Ok(value.map(JsPropValue::from))
  }

  /// Get all edge properties
  #[napi]
  pub fn get_edge_props(
    &self,
    src: i64,
    edge_type: String,
    dst: i64,
  ) -> Result<HashMap<String, JsPropValue>> {
    let props = self
      .with_kite(|ray| {
        ray
          .edge_props(src as NodeId, &edge_type, dst as NodeId)
          .map_err(|e| Error::from_reason(e.to_string()))
      })?
      .unwrap_or_default();

    Ok(
      props
        .into_iter()
        .map(|(key, value)| (key, JsPropValue::from(value)))
        .collect(),
    )
  }

  /// Set an edge property value
  #[napi]
  pub fn set_edge_prop(
    &self,
    env: Env,
    src: i64,
    edge_type: String,
    dst: i64,
    prop_name: String,
    value: Unknown,
  ) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.with_kite_mut(|ray| {
      ray
        .set_edge_prop(
          src as NodeId,
          &edge_type,
          dst as NodeId,
          &prop_name,
          prop_value,
        )
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Set multiple edge properties
  #[napi]
  pub fn set_edge_props(
    &self,
    env: Env,
    src: i64,
    edge_type: String,
    dst: i64,
    props: Option<Object>,
  ) -> Result<()> {
    let props_map = js_props_to_map(&env, props)?;
    self.with_kite_mut(|ray| {
      ray
        .set_edge_props(src as NodeId, &edge_type, dst as NodeId, props_map)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Delete an edge property
  #[napi]
  pub fn del_edge_prop(
    &self,
    src: i64,
    edge_type: String,
    dst: i64,
    prop_name: String,
  ) -> Result<()> {
    self.with_kite_mut(|ray| {
      ray
        .del_edge_prop(src as NodeId, &edge_type, dst as NodeId, &prop_name)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Update edge properties with a builder
  #[napi]
  pub fn update_edge(
    &self,
    src: i64,
    edge_type: String,
    dst: i64,
  ) -> Result<KiteUpdateEdgeBuilder> {
    self.with_kite(|ray| {
      ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      Ok(())
    })?;

    Ok(KiteUpdateEdgeBuilder::new(
      self.inner.clone(),
      src as NodeId,
      edge_type,
      dst as NodeId,
    ))
  }

  /// Upsert edge properties with a builder
  #[napi]
  pub fn upsert_edge(
    &self,
    src: i64,
    edge_type: String,
    dst: i64,
  ) -> Result<KiteUpsertEdgeBuilder> {
    self.with_kite(|ray| {
      ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      Ok(())
    })?;

    Ok(KiteUpsertEdgeBuilder::new(
      self.inner.clone(),
      src as NodeId,
      edge_type,
      dst as NodeId,
    ))
  }

  /// List all nodes of a type (returns array of node objects)
  #[napi]
  pub fn all(&self, env: Env, node_type: String) -> Result<Vec<Object>> {
    self.with_kite(|ray| {
      let nodes = ray
        .all(&node_type)
        .map_err(|e| Error::from_reason(e.to_string()))?;
      let mut out = Vec::new();
      for node_ref in nodes {
        let (node_id, node_key, node_type) = node_ref.into_parts();
        let props = get_node_props(ray, node_id);
        out.push(node_to_js(&env, node_id, node_key, &node_type, props)?);
      }
      Ok(out)
    })
  }

  /// Count nodes (optionally by type)
  #[napi]
  pub fn count_nodes(&self, node_type: Option<String>) -> Result<i64> {
    self.with_kite(|ray| match node_type {
      Some(node_type) => ray
        .count_nodes_by_type(&node_type)
        .map(|v| v as i64)
        .map_err(|e| Error::from_reason(e.to_string())),
      None => Ok(ray.count_nodes() as i64),
    })
  }

  /// Count edges (optionally by type)
  #[napi]
  pub fn count_edges(&self, edge_type: Option<String>) -> Result<i64> {
    self.with_kite(|ray| match edge_type {
      Some(edge_type) => ray
        .count_edges_by_type(&edge_type)
        .map(|v| v as i64)
        .map_err(|e| Error::from_reason(e.to_string())),
      None => Ok(ray.count_edges() as i64),
    })
  }

  /// List all edges (optionally by type)
  #[napi]
  pub fn all_edges(&self, edge_type: Option<String>) -> Result<Vec<JsFullEdge>> {
    self.with_kite(|ray| {
      let edges = ray
        .all_edges(edge_type.as_deref())
        .map_err(|e| Error::from_reason(e.to_string()))?;
      Ok(
        edges
          .map(|edge| JsFullEdge {
            src: edge.src as i64,
            etype: edge.etype,
            dst: edge.dst as i64,
          })
          .collect(),
      )
    })
  }

  /// Check if a path exists between two nodes
  #[napi]
  pub fn has_path(&self, source: i64, target: i64, edge_type: Option<String>) -> Result<bool> {
    self.with_kite_mut(|ray| {
      ray
        .has_path(source as NodeId, target as NodeId, edge_type.as_deref())
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Get all nodes reachable within a maximum depth
  #[napi]
  pub fn reachable_from(
    &self,
    source: i64,
    max_depth: i64,
    edge_type: Option<String>,
  ) -> Result<Vec<i64>> {
    self.with_kite(|ray| {
      let nodes = ray
        .reachable_from(source as NodeId, max_depth as usize, edge_type.as_deref())
        .map_err(|e| Error::from_reason(e.to_string()))?;
      Ok(nodes.into_iter().map(|id| id as i64).collect())
    })
  }

  /// Get all node type names
  #[napi]
  pub fn node_types(&self) -> Result<Vec<String>> {
    self.with_kite(|ray| {
      Ok(
        ray
          .node_types()
          .into_iter()
          .map(|s| s.to_string())
          .collect(),
      )
    })
  }

  /// Get all edge type names
  #[napi]
  pub fn edge_types(&self) -> Result<Vec<String>> {
    self.with_kite(|ray| {
      Ok(
        ray
          .edge_types()
          .into_iter()
          .map(|s| s.to_string())
          .collect(),
      )
    })
  }

  /// Get database statistics
  #[napi]
  pub fn stats(&self) -> Result<DbStats> {
    self.with_kite(|ray| {
      let s = ray.stats();
      Ok(DbStats {
        snapshot_gen: s.snapshot_gen as i64,
        snapshot_nodes: s.snapshot_nodes as i64,
        snapshot_edges: s.snapshot_edges as i64,
        snapshot_max_node_id: s.snapshot_max_node_id as i64,
        delta_nodes_created: s.delta_nodes_created as i64,
        delta_nodes_deleted: s.delta_nodes_deleted as i64,
        delta_edges_added: s.delta_edges_added as i64,
        delta_edges_deleted: s.delta_edges_deleted as i64,
        wal_segment: s.wal_segment as i64,
        wal_bytes: s.wal_bytes as i64,
        recommend_compact: s.recommend_compact,
        mvcc_stats: s.mvcc_stats.map(|stats| MvccStats {
          active_transactions: stats.active_transactions as i64,
          min_active_ts: stats.min_active_ts as i64,
          versions_pruned: stats.versions_pruned as i64,
          gc_runs: stats.gc_runs as i64,
          last_gc_time: stats.last_gc_time as i64,
          committed_writes_size: stats.committed_writes_size as i64,
          committed_writes_pruned: stats.committed_writes_pruned as i64,
        }),
      })
    })
  }

  /// Get a human-readable description of the database
  #[napi]
  pub fn describe(&self) -> Result<String> {
    self.with_kite(|ray| Ok(ray.describe()))
  }

  /// Check database integrity
  #[napi]
  pub fn check(&self) -> Result<CheckResult> {
    self.with_kite(|ray| {
      let result = ray.check().map_err(|e| Error::from_reason(e.to_string()))?;
      Ok(CheckResult::from(result))
    })
  }

  /// Begin a transaction
  #[napi]
  pub fn begin(&self, read_only: Option<bool>) -> Result<i64> {
    let read_only = read_only.unwrap_or(false);
    let guard = self.inner.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    ray
      .raw()
      .begin(read_only)
      .map(|txid| txid as i64)
      .map_err(|e| Error::from_reason(format!("Failed to begin transaction: {e}")))
  }

  /// Begin a bulk-load transaction (fast path, MVCC disabled)
  #[napi]
  pub fn begin_bulk(&self) -> Result<i64> {
    let guard = self.inner.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    ray
      .raw()
      .begin_bulk()
      .map(|txid| txid as i64)
      .map_err(|e| Error::from_reason(format!("Failed to begin bulk transaction: {e}")))
  }

  /// Commit the current transaction
  #[napi]
  pub fn commit(&self) -> Result<()> {
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .commit()
        .map_err(|e| Error::from_reason(format!("Failed to commit: {e}")))
    })
  }

  /// Rollback the current transaction
  #[napi]
  pub fn rollback(&self) -> Result<()> {
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .rollback()
        .map_err(|e| Error::from_reason(format!("Failed to rollback: {e}")))
    })
  }

  /// Check if there's an active transaction
  #[napi]
  pub fn has_transaction(&self) -> Result<bool> {
    self.with_kite(|ray| Ok(ray.raw().has_transaction()))
  }

  /// Perform a checkpoint (compact WAL into snapshot)
  #[napi]
  pub fn checkpoint(&self) -> Result<()> {
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .checkpoint()
        .map_err(|e| Error::from_reason(format!("Failed to checkpoint: {e}")))
    })
  }

  /// Execute a batch of operations atomically
  #[napi]
  pub fn batch(&self, env: Env, ops: Vec<Object>) -> Result<Vec<Object>> {
    let mut rust_ops = Vec::with_capacity(ops.len());

    for op in ops {
      let op_name: Option<String> = op.get_named_property("op").ok();
      let op_name = match op_name {
        Some(name) => name,
        None => op.get_named_property("type")?,
      };

      match op_name.as_str() {
        "createNode" => {
          let node_type: String = op.get_named_property("nodeType")?;
          let key: Unknown = op.get_named_property("key")?;
          let props: Option<Object> = op.get_named_property("props")?;
          let key_suffix = {
            let spec = self.key_spec(&node_type)?;
            key_suffix_from_js(&env, spec.as_ref(), key)?
          };
          let props_map = js_props_to_map(&env, props)?;
          rust_ops.push(BatchOp::CreateNode {
            node_type,
            key_suffix,
            props: props_map,
          });
        }
        "deleteNode" => {
          let node_id: i64 = op.get_named_property("nodeId")?;
          rust_ops.push(BatchOp::DeleteNode {
            node_id: node_id as NodeId,
          });
        }
        "link" => {
          let src: i64 = op.get_named_property("src")?;
          let dst: i64 = op.get_named_property("dst")?;
          let edge_type: String = op.get_named_property("edgeType")?;
          rust_ops.push(BatchOp::Link {
            src: src as NodeId,
            edge_type,
            dst: dst as NodeId,
          });
        }
        "linkWithProps" => {
          let src: i64 = op.get_named_property("src")?;
          let dst: i64 = op.get_named_property("dst")?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let props: Option<Object> = op.get_named_property("props")?;
          let props_map = js_props_to_map(&env, props)?;
          rust_ops.push(BatchOp::LinkWithProps {
            src: src as NodeId,
            edge_type,
            dst: dst as NodeId,
            props: props_map,
          });
        }
        "unlink" => {
          let src: i64 = op.get_named_property("src")?;
          let dst: i64 = op.get_named_property("dst")?;
          let edge_type: String = op.get_named_property("edgeType")?;
          rust_ops.push(BatchOp::Unlink {
            src: src as NodeId,
            edge_type,
            dst: dst as NodeId,
          });
        }
        "setProp" => {
          let node_id: i64 = op.get_named_property("nodeId")?;
          let prop_name: String = op.get_named_property("propName")?;
          let value: Unknown = op.get_named_property("value")?;
          let prop_value = js_value_to_prop_value(&env, value)?;
          rust_ops.push(BatchOp::SetProp {
            node_id: node_id as NodeId,
            prop_name,
            value: prop_value,
          });
        }
        "setEdgeProp" => {
          let src: i64 = op.get_named_property("src")?;
          let dst: i64 = op.get_named_property("dst")?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let prop_name: String = op.get_named_property("propName")?;
          let value: Unknown = op.get_named_property("value")?;
          let prop_value = js_value_to_prop_value(&env, value)?;
          rust_ops.push(BatchOp::SetEdgeProp {
            src: src as NodeId,
            edge_type,
            dst: dst as NodeId,
            prop_name,
            value: prop_value,
          });
        }
        "setEdgeProps" => {
          let src: i64 = op.get_named_property("src")?;
          let dst: i64 = op.get_named_property("dst")?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let props: Option<Object> = op.get_named_property("props")?;
          let props_map = js_props_to_map(&env, props)?;
          rust_ops.push(BatchOp::SetEdgeProps {
            src: src as NodeId,
            edge_type,
            dst: dst as NodeId,
            props: props_map,
          });
        }
        "delProp" => {
          let node_id: i64 = op.get_named_property("nodeId")?;
          let prop_name: String = op.get_named_property("propName")?;
          rust_ops.push(BatchOp::DelProp {
            node_id: node_id as NodeId,
            prop_name,
          });
        }
        other => {
          return Err(Error::from_reason(format!("Unknown batch op: {other}")));
        }
      }
    }

    let results = self.with_kite_mut(|ray| execute_batch_ops(ray, rust_ops))?;

    let mut out = Vec::with_capacity(results.len());
    for result in results {
      out.push(batch_result_to_js(&env, result)?);
    }
    Ok(out)
  }

  /// Begin a traversal from a node ID
  #[napi]
  pub fn from(&self, node_id: i64) -> Result<KiteTraversal> {
    Ok(KiteTraversal {
      ray: self.inner.clone(),
      start_nodes: vec![node_id as NodeId],
      steps: kite_traversal::StepChain::default(),
      limit: None,
      selected_props: None,
      where_edge: None,
      where_node: None,
    })
  }

  /// Begin a traversal from multiple nodes
  #[napi]
  pub fn from_nodes(&self, node_ids: Vec<i64>) -> Result<KiteTraversal> {
    Ok(KiteTraversal {
      ray: self.inner.clone(),
      start_nodes: node_ids.into_iter().map(|id| id as NodeId).collect(),
      steps: kite_traversal::StepChain::default(),
      limit: None,
      selected_props: None,
      where_edge: None,
      where_node: None,
    })
  }

  /// Begin a path finding query
  #[napi]
  pub fn path(&self, source: i64, target: i64) -> Result<KitePath> {
    Ok(KitePath::new(
      self.inner.clone(),
      source as NodeId,
      vec![target as NodeId],
    ))
  }

  /// Begin a path finding query to multiple targets
  #[napi]
  pub fn path_to_any(&self, source: i64, targets: Vec<i64>) -> Result<KitePath> {
    Ok(KitePath::new(
      self.inner.clone(),
      source as NodeId,
      targets.into_iter().map(|id| id as NodeId).collect(),
    ))
  }
}

/// Kite entrypoint - sync version
#[napi]
pub fn kite_sync(path: String, options: JsKiteOptions) -> Result<Kite> {
  Kite::open(path, options)
}

// =============================================================================
// Async Kite Open Task
// =============================================================================

/// Task for opening Kite database asynchronously
pub struct OpenKiteTask {
  path: String,
  options: JsKiteOptions,
  // Store result here to avoid public type in trait
  result: Option<(RustKite, HashMap<String, Arc<KeySpec>>)>,
}

impl napi::Task for OpenKiteTask {
  type Output = ();
  type JsValue = Kite;

  fn compute(&mut self) -> Result<Self::Output> {
    let mut node_specs: HashMap<String, Arc<KeySpec>> = HashMap::new();
    let mut kite_opts = KiteOptions::new();
    kite_opts.read_only = self.options.read_only.unwrap_or(false);
    kite_opts.create_if_missing = self.options.create_if_missing.unwrap_or(true);
    kite_opts.mvcc = self.options.mvcc.unwrap_or(false);
    kite_opts.mvcc_gc_interval_ms = self.options.mvcc_gc_interval_ms.map(|v| v as u64);
    kite_opts.mvcc_retention_ms = self.options.mvcc_retention_ms.map(|v| v as u64);
    kite_opts.mvcc_max_chain_depth = self.options.mvcc_max_chain_depth.map(|v| v as usize);
    if let Some(mode) = self.options.sync_mode.take() {
      kite_opts.sync_mode = mode.into();
    }
    if let Some(enabled) = self.options.group_commit_enabled {
      kite_opts.group_commit_enabled = enabled;
    }
    if let Some(window_ms) = self.options.group_commit_window_ms {
      if window_ms >= 0 {
        kite_opts.group_commit_window_ms = window_ms as u64;
      }
    }
    if let Some(wal_size_mb) = self.options.wal_size_mb {
      if wal_size_mb > 0 {
        kite_opts.wal_size = Some((wal_size_mb as usize).saturating_mul(1024 * 1024));
      }
    }
    if let Some(threshold) = self.options.checkpoint_threshold {
      kite_opts.checkpoint_threshold = Some(threshold.clamp(0.0, 1.0));
    }

    for node in &self.options.nodes {
      let key_spec = Arc::new(parse_key_spec(&node.name, node.key.clone())?);
      let prefix = key_spec.prefix().to_string();

      let mut node_def = NodeDef::new(&node.name, &prefix);
      if let Some(props) = node.props.as_ref() {
        for (prop_name, prop_spec) in props {
          node_def = node_def.prop(prop_spec_to_def(prop_name, prop_spec)?);
        }
      }

      node_specs.insert(node.name.clone(), Arc::clone(&key_spec));
      kite_opts.nodes.push(node_def);
    }

    for edge in &self.options.edges {
      let mut edge_def = EdgeDef::new(&edge.name);
      if let Some(props) = edge.props.as_ref() {
        for (prop_name, prop_spec) in props {
          edge_def = edge_def.prop(prop_spec_to_def(prop_name, prop_spec)?);
        }
      }
      kite_opts.edges.push(edge_def);
    }

    let ray =
      RustKite::open(&self.path, kite_opts).map_err(|e| Error::from_reason(e.to_string()))?;
    self.result = Some((ray, node_specs));
    Ok(())
  }

  #[allow(clippy::arc_with_non_send_sync)]
  fn resolve(&mut self, _env: Env, _output: Self::Output) -> Result<Self::JsValue> {
    let (ray, node_specs) = self
      .result
      .take()
      .ok_or_else(|| Error::from_reason("Task result not available"))?;
    Ok(Kite {
      inner: Arc::new(RwLock::new(Some(ray))),
      node_specs: Arc::new(node_specs),
    })
  }
}

/// Kite entrypoint - async version (recommended)
/// Opens the database on a background thread to avoid blocking the event loop
#[napi]
pub fn kite(path: String, options: JsKiteOptions) -> AsyncTask<OpenKiteTask> {
  AsyncTask::new(OpenKiteTask {
    path,
    options,
    result: None,
  })
}
