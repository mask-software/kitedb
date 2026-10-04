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
use helpers::{batch_result_to_js, execute_batch_ops, node_props, node_props_selected, node_to_js};
use key_spec::{node_prop_spec_to_def, parse_key_spec, prop_spec_to_def, KeySpec};

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::api::kite::{BatchOp, EdgeDef, Kite as RustKite, KiteOptions, NodeDef};

use super::database::{
  log_transport, log_transport_json, snapshot_transport, snapshot_transport_json,
  JsPrimaryRetentionOutcome, JsReplicationLogTransportPage, JsReplicationSnapshotTransport,
};
use super::database::{
  CheckResult, DbStats, JsPrimaryReplicationStatus, JsReplicaReplicationStatus, MvccStats,
};
use super::database::{JsFullEdge, JsPropValue};
use super::validation;

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

fn apply_kite_open_options(options: &JsKiteOptions, kite_opts: &mut KiteOptions) -> Result<()> {
  kite_opts.read_only = options.read_only.unwrap_or(false);
  kite_opts.create_if_missing = options.create_if_missing.unwrap_or(true);
  if let Some(mvcc) = options.mvcc {
    kite_opts.mvcc = mvcc;
  }
  kite_opts.strict_schema = options.strict_schema.unwrap_or(false);

  if let Some(value) = options.mvcc_gc_interval_ms {
    kite_opts.mvcc_gc_interval_ms = Some(validation::positive_u64(
      "mvccGcIntervalMs",
      value,
      validation::MAX_DURATION_MS as u64,
    )?);
  }
  if let Some(value) = options.mvcc_retention_ms {
    kite_opts.mvcc_retention_ms = Some(validation::non_negative_u64(
      "mvccRetentionMs",
      value,
      validation::MAX_DURATION_MS as u64,
    )?);
  }
  if let Some(value) = options.mvcc_max_chain_depth {
    kite_opts.mvcc_max_chain_depth = Some(validation::positive_usize(
      "mvccMaxChainDepth",
      value,
      validation::MAX_DEPTH,
    )?);
  }
  if let Some(mode) = options.sync_mode.as_ref() {
    kite_opts.sync_mode = mode.into();
  }
  if let Some(enabled) = options.group_commit_enabled {
    kite_opts.group_commit_enabled = enabled;
  }
  if let Some(value) = options.group_commit_window_ms {
    kite_opts.group_commit_window_ms = validation::non_negative_u64(
      "groupCommitWindowMs",
      value,
      validation::MAX_DURATION_MS as u64,
    )?;
  }
  if let Some(value) = options.wal_size_mb {
    let megabytes = validation::positive_u64(
      "walSizeMb",
      value,
      (validation::MAX_BYTES as u64) / (1024 * 1024),
    )?;
    kite_opts.wal_size = Some(
      usize::try_from(
        megabytes
          .checked_mul(1024 * 1024)
          .ok_or_else(|| validation::invalid_argument("walSizeMb is too large"))?,
      )
      .map_err(|_| validation::invalid_argument("walSizeMb does not fit in a platform usize"))?,
    );
  }
  if let Some(value) = options.checkpoint_threshold {
    // Deprecated, without effect: checked, then ignored.
    validation::ratio("checkpointThreshold", value)?;
  }
  if let Some(value) = options.checkpoint_thread {
    kite_opts.checkpoint_thread = Some(value);
  }
  if let Some(value) = options.checkpoint_log_ratio {
    kite_opts.checkpoint_log_ratio = Some(validation::non_negative_number(
      "checkpointLogRatio",
      value,
    )?);
  }
  if let Some(value) = options.checkpoint_log_budget {
    kite_opts.checkpoint_log_budget =
      Some(validation::positive_bytes("checkpointLogBudget", value)?);
  }
  if let Some(value) = options.wal_segment_size {
    kite_opts.wal_segment_size = Some(validation::positive_bytes("walSegmentSize", value)?);
  }
  if let Some(value) = options.wal_segment_limit {
    kite_opts.wal_segment_limit = Some(validation::positive_bytes("walSegmentLimit", value)?);
  }
  if let Some(value) = options.close_checkpoint_if_wal_usage_at_least {
    kite_opts.close_checkpoint_if_wal_usage_at_least = Some(validation::ratio(
      "closeCheckpointIfWalUsageAtLeast",
      value,
    )?);
  }
  if let Some(role) = options.replication_role.as_ref() {
    kite_opts.replication_role = role.into();
  }
  kite_opts.replication_sidecar_path = options.replication_sidecar_path.as_ref().map(Into::into);
  kite_opts.replication_source_db_path =
    options.replication_source_db_path.as_ref().map(Into::into);
  kite_opts.replication_source_sidecar_path = options
    .replication_source_sidecar_path
    .as_ref()
    .map(Into::into);
  if let Some(value) = options.replication_segment_max_bytes {
    kite_opts.replication_segment_max_bytes = Some(validation::positive_u64(
      "replicationSegmentMaxBytes",
      value,
      validation::MAX_BYTES as u64,
    )?);
  }
  if let Some(value) = options.replication_retention_min_entries {
    kite_opts.replication_retention_min_entries = Some(validation::non_negative_u64(
      "replicationRetentionMinEntries",
      value,
      validation::MAX_COUNT as u64,
    )?);
  }
  if let Some(value) = options.replication_retention_min_ms {
    kite_opts.replication_retention_min_ms = Some(validation::non_negative_u64(
      "replicationRetentionMinMs",
      value,
      validation::MAX_DURATION_MS as u64,
    )?);
  }

  Ok(())
}

#[napi]
impl Kite {
  /// Open a Kite database
  #[allow(clippy::arc_with_non_send_sync)]
  #[napi(factory)]
  pub fn open(path: String, options: JsKiteOptions) -> Result<Self> {
    let mut node_specs: HashMap<String, Arc<KeySpec>> = HashMap::new();
    let mut kite_opts = KiteOptions::new();
    apply_kite_open_options(&options, &mut kite_opts)?;

    for node in options.nodes {
      let key_spec = Arc::new(parse_key_spec(&node.name, node.key)?);
      let prefix = key_spec.prefix().to_string();

      let mut node_def = NodeDef::new(&node.name, &prefix);
      if let Some(props) = node.props.as_ref() {
        for (prop_name, prop_spec) in props {
          node_def = node_def.prop(node_prop_spec_to_def(&node.name, prop_name, prop_spec)?);
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
  ) -> Result<Option<Object<'_>>> {
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
          let props = node_props_selected(ray, node_id, selected_props.as_ref());
          let obj = node_to_js(&env, node_id, node_key, &node_type, props)?;
          Ok(Some(obj))
        }
        None => Ok(None),
      }
    })
  }

  /// Get a node by ID (returns node object with props)
  #[napi(js_name = "get_by_id")]
  pub fn by_id(
    &self,
    env: Env,
    node_id: f64,
    props: Option<Vec<String>>,
  ) -> Result<Option<Object<'_>>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let selected_props = props.map(|props| props.into_iter().collect::<HashSet<String>>());
    self.with_kite(move |ray| {
      let node_ref = ray
        .node_by_id(node_id)
        .map_err(|e| Error::from_reason(e.to_string()))?;
      match node_ref {
        Some(node_ref) => {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let props = node_props_selected(ray, node_id, selected_props.as_ref());
          let obj = node_to_js(&env, node_id, node_key, &node_type, props)?;
          Ok(Some(obj))
        }
        None => Ok(None),
      }
    })
  }

  /// Get a lightweight node reference by key (no properties)
  #[napi(js_name = "get_ref")]
  pub fn node_ref(&self, env: Env, node_type: String, key: Unknown) -> Result<Option<Object<'_>>> {
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
  #[napi(js_name = "get_id")]
  pub fn node_id(&self, env: Env, node_type: String, key: Unknown) -> Result<Option<i64>> {
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
  #[napi(js_name = "get_by_ids")]
  pub fn by_ids(
    &self,
    env: Env,
    node_ids: Vec<f64>,
    props: Option<Vec<String>>,
  ) -> Result<Vec<Object<'_>>> {
    if node_ids.is_empty() {
      return Ok(Vec::new());
    }
    let node_ids = validation::node_ids("nodeIds", &node_ids)?;

    let selected_props = props.map(|props| props.into_iter().collect::<HashSet<String>>());
    self.with_kite(move |ray| {
      let mut out = Vec::with_capacity(node_ids.len());
      for node_id in node_ids {
        let node_ref = ray
          .node_by_id(node_id)
          .map_err(|e| Error::from_reason(e.to_string()))?;
        if let Some(node_ref) = node_ref {
          let (node_id, node_key, node_type) = node_ref.into_parts();
          let props = node_props_selected(ray, node_id, selected_props.as_ref());
          out.push(node_to_js(&env, node_id, node_key, &node_type, props)?);
        }
      }
      Ok(out)
    })
  }

  /// Get a node property value
  #[napi(js_name = "get_prop")]
  pub fn prop(&self, node_id: f64, prop_name: String) -> Result<Option<JsPropValue>> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let value = self.with_kite(|ray| Ok(ray.prop(node_id, &prop_name)))?;
    Ok(value.map(JsPropValue::from))
  }

  /// Set a node property value
  #[napi]
  pub fn set_prop(&self, env: Env, node_id: f64, prop_name: String, value: Unknown) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.with_kite_mut(|ray| {
      ray
        .set_prop(node_id, &prop_name, prop_value)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Set multiple node property values
  #[napi]
  pub fn set_props(&self, env: Env, node_id: f64, props: Object) -> Result<()> {
    let node_id = validation::node_id("nodeId", node_id)?;
    let props_map = js_props_to_map(&env, Some(props))?;
    self.with_kite_mut(|ray| {
      ray
        .set_props(node_id, props_map)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Check if a node exists
  #[napi]
  pub fn exists(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    self.with_kite(|ray| Ok(ray.exists(node_id)))
  }

  /// Delete a node by ID
  #[napi]
  pub fn delete_by_id(&self, node_id: f64) -> Result<bool> {
    let node_id = validation::node_id("nodeId", node_id)?;
    self.with_kite_mut(|ray| {
      ray
        .delete_node(node_id)
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
  pub fn update_by_id(&self, node_id: f64) -> Result<KiteUpdateBuilder> {
    let node_id = validation::node_id("nodeId", node_id)?;
    Ok(KiteUpdateBuilder::new(self.inner.clone(), node_id))
  }

  /// Create an upsert builder by node ID
  #[napi]
  pub fn upsert_by_id(&self, node_type: String, node_id: f64) -> Result<KiteUpsertByIdBuilder> {
    let node_id = validation::node_id("nodeId", node_id)?;
    Ok(KiteUpsertByIdBuilder::new(
      self.inner.clone(),
      node_type,
      node_id,
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
    src: f64,
    edge_type: String,
    dst: f64,
    props: Option<Object>,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let props_map = js_props_to_map(&env, props)?;
    self.with_kite_mut(|ray| {
      if props_map.is_empty() {
        ray
          .link(src, &edge_type, dst)
          .map_err(|e| Error::from_reason(e.to_string()))
      } else {
        ray
          .link_with_props(src, &edge_type, dst, props_map)
          .map_err(|e| Error::from_reason(e.to_string()))
      }
    })
  }

  /// Unlink two nodes
  #[napi]
  pub fn unlink(&self, src: f64, edge_type: String, dst: f64) -> Result<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    self.with_kite_mut(|ray| {
      ray
        .unlink(src, &edge_type, dst)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Check if an edge exists
  #[napi]
  pub fn has_edge(&self, src: f64, edge_type: String, dst: f64) -> Result<bool> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    self.with_kite(move |ray| {
      ray
        .has_edge(src, &edge_type, dst)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Get an edge property value
  #[napi(js_name = "get_edge_prop")]
  pub fn edge_prop(
    &self,
    src: f64,
    edge_type: String,
    dst: f64,
    prop_name: String,
  ) -> Result<Option<JsPropValue>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let value = self.with_kite(|ray| {
      ray
        .edge_prop(src, &edge_type, dst, &prop_name)
        .map_err(|e| Error::from_reason(e.to_string()))
    })?;
    Ok(value.map(JsPropValue::from))
  }

  /// Get all edge properties
  #[napi(js_name = "get_edge_props")]
  pub fn edge_props(
    &self,
    src: f64,
    edge_type: String,
    dst: f64,
  ) -> Result<HashMap<String, JsPropValue>> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let props = self
      .with_kite(|ray| {
        ray
          .edge_props(src, &edge_type, dst)
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
    src: f64,
    edge_type: String,
    dst: f64,
    prop_name: String,
    value: Unknown,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.with_kite_mut(|ray| {
      ray
        .set_edge_prop(src, &edge_type, dst, &prop_name, prop_value)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Set multiple edge properties
  #[napi]
  pub fn set_edge_props(
    &self,
    env: Env,
    src: f64,
    edge_type: String,
    dst: f64,
    props: Option<Object>,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    let props_map = js_props_to_map(&env, props)?;
    self.with_kite_mut(|ray| {
      ray
        .set_edge_props(src, &edge_type, dst, props_map)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Delete an edge property
  #[napi]
  pub fn del_edge_prop(
    &self,
    src: f64,
    edge_type: String,
    dst: f64,
    prop_name: String,
  ) -> Result<()> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    self.with_kite_mut(|ray| {
      ray
        .del_edge_prop(src, &edge_type, dst, &prop_name)
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Update edge properties with a builder
  #[napi]
  pub fn update_edge(
    &self,
    src: f64,
    edge_type: String,
    dst: f64,
  ) -> Result<KiteUpdateEdgeBuilder> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    self.with_kite(|ray| {
      ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      Ok(())
    })?;

    Ok(KiteUpdateEdgeBuilder::new(
      self.inner.clone(),
      src,
      edge_type,
      dst,
    ))
  }

  /// Upsert edge properties with a builder
  #[napi]
  pub fn upsert_edge(
    &self,
    src: f64,
    edge_type: String,
    dst: f64,
  ) -> Result<KiteUpsertEdgeBuilder> {
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    self.with_kite(|ray| {
      ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      Ok(())
    })?;

    Ok(KiteUpsertEdgeBuilder::new(
      self.inner.clone(),
      src,
      edge_type,
      dst,
    ))
  }

  /// List all nodes of a type (returns array of node objects)
  #[napi]
  pub fn all(&self, env: Env, node_type: String) -> Result<Vec<Object<'_>>> {
    self.with_kite(|ray| {
      let nodes = ray
        .all(&node_type)
        .map_err(|e| Error::from_reason(e.to_string()))?;
      let mut out = Vec::new();
      for node_ref in nodes {
        let (node_id, node_key, node_type) = node_ref.into_parts();
        let props = node_props(ray, node_id);
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
            src: edge.src as f64,
            etype: edge.etype,
            dst: edge.dst as f64,
          })
          .collect(),
      )
    })
  }

  /// Check if a path exists between two nodes
  #[napi]
  pub fn has_path(&self, source: f64, target: f64, edge_type: Option<String>) -> Result<bool> {
    let source = validation::node_id("source", source)?;
    let target = validation::node_id("target", target)?;
    self.with_kite_mut(|ray| {
      ray
        .has_path(source, target, edge_type.as_deref())
        .map_err(|e| Error::from_reason(e.to_string()))
    })
  }

  /// Get all nodes reachable within a maximum depth
  #[napi]
  pub fn reachable_from(
    &self,
    source: f64,
    max_depth: i64,
    edge_type: Option<String>,
  ) -> Result<Vec<i64>> {
    let source = validation::node_id("source", source)?;
    let max_depth = validation::non_negative_usize("maxDepth", max_depth, validation::MAX_DEPTH)?;
    self.with_kite(|ray| {
      let nodes = ray
        .reachable_from(source, max_depth, edge_type.as_deref())
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

  /// Begin a bulk-load transaction: the fast path for loading data. It runs
  /// alone among writers (it waits for open write transactions, and they
  /// wait for it); readers never wait for it.
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

  /// Primary replication status when role=primary, else null.
  #[napi]
  pub fn primary_replication_status(&self) -> Result<Option<JsPrimaryReplicationStatus>> {
    self.with_kite(|ray| Ok(ray.raw().primary_replication_status().map(Into::into)))
  }

  /// Replica replication status when role=replica, else null.
  #[napi]
  pub fn replica_replication_status(&self) -> Result<Option<JsReplicaReplicationStatus>> {
    self.with_kite(|ray| Ok(ray.raw().replica_replication_status().map(Into::into)))
  }

  /// Pull and apply up to maxFrames replication frames on replica.
  #[napi]
  pub fn replica_catch_up_once(&self, max_frames: i64) -> Result<i64> {
    let max_frames =
      validation::non_negative_usize("maxFrames", max_frames, validation::MAX_COUNT)?;
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .replica_catch_up_once(max_frames)
        .map(|count| count as i64)
        .map_err(|e| Error::from_reason(format!("Failed replica catch-up: {e}")))
    })
  }

  /// Force a replica reseed from current primary snapshot.
  #[napi]
  pub fn replica_reseed_from_snapshot(&self) -> Result<()> {
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .replica_reseed_from_snapshot()
        .map_err(|e| Error::from_reason(format!("Failed to reseed replica: {e}")))
    })
  }

  /// Promote this primary to the next replication epoch.
  #[napi]
  pub fn primary_promote_to_next_epoch(&self) -> Result<i64> {
    self.with_kite_mut(|ray| {
      ray
        .raw()
        .primary_promote_to_next_epoch()
        .map(|epoch| epoch as i64)
        .map_err(|e| Error::from_reason(format!("Failed to promote primary: {e}")))
    })
  }

  /// Report a replica's applied position (primary role), for retention.
  #[napi]
  pub fn primary_report_replica_progress(
    &self,
    replica_id: String,
    epoch: i64,
    applied_log_index: i64,
  ) -> Result<()> {
    let epoch = validation::non_negative_u64("epoch", epoch, i64::MAX as u64)?;
    let applied_log_index =
      validation::non_negative_u64("appliedLogIndex", applied_log_index, i64::MAX as u64)?;
    self.with_kite(|ray| {
      ray
        .raw()
        .primary_report_replica_progress(&replica_id, epoch, applied_log_index)
        .map_err(|e| Error::from_reason(format!("Failed to report replica progress: {e}")))
    })
  }

  /// Forget a replica's reported progress, so a decommissioned replica stops
  /// holding back retention. Returns whether it had progress recorded.
  #[napi]
  pub fn primary_remove_replica_progress(&self, replica_id: String) -> Result<bool> {
    self.with_kite(|ray| {
      ray
        .raw()
        .primary_remove_replica_progress(&replica_id)
        .map_err(|e| Error::from_reason(format!("Failed to remove replica progress: {e}")))
    })
  }

  /// Run replication retention (primary role).
  #[napi]
  pub fn primary_run_retention(&self) -> Result<JsPrimaryRetentionOutcome> {
    self.with_kite(|ray| {
      ray
        .raw()
        .primary_run_retention()
        .map(Into::into)
        .map_err(|e| Error::from_reason(format!("Failed to run retention: {e}")))
    })
  }

  /// Export a consistent snapshot (metadata, and the database file copy when
  /// includeData, up to 32 MiB) as transport JSON, with the data in base64.
  #[napi]
  pub fn export_replication_snapshot_transport_json(
    &self,
    include_data: Option<bool>,
  ) -> Result<String> {
    self.with_kite(|ray| snapshot_transport_json(ray.raw(), include_data))
  }

  /// Export a consistent snapshot with the database file copy (when
  /// includeData, up to 1 GiB) as a Buffer.
  #[napi]
  pub fn export_replication_snapshot_transport(
    &self,
    include_data: Option<bool>,
  ) -> Result<JsReplicationSnapshotTransport> {
    self.with_kite(|ray| snapshot_transport(ray.raw(), include_data))
  }

  /// Export a replication log page (cursor + limits) as transport JSON.
  #[napi]
  pub fn export_replication_log_transport_json(
    &self,
    cursor: Option<String>,
    max_frames: Option<i64>,
    max_bytes: Option<i64>,
    include_payload: Option<bool>,
  ) -> Result<String> {
    self.with_kite(|ray| {
      log_transport_json(ray.raw(), cursor, max_frames, max_bytes, include_payload)
    })
  }

  /// Export a replication log page (cursor + limits) with payloads as Buffers.
  #[napi]
  pub fn export_replication_log_transport(
    &self,
    cursor: Option<String>,
    max_frames: Option<i64>,
    max_bytes: Option<i64>,
    include_payload: Option<bool>,
  ) -> Result<JsReplicationLogTransportPage> {
    self.with_kite(|ray| log_transport(ray.raw(), cursor, max_frames, max_bytes, include_payload))
  }

  /// Replication metrics in Prometheus text format.
  #[napi]
  pub fn replication_metrics_prometheus(&self) -> Result<String> {
    self.with_kite(|ray| {
      Ok(crate::metrics::collect_replication_metrics_prometheus_single_file(ray.raw()))
    })
  }

  /// Replication metrics as OpenTelemetry JSON.
  #[napi]
  pub fn replication_metrics_otel_json(&self) -> Result<String> {
    self.with_kite(|ray| {
      Ok(crate::metrics::collect_replication_metrics_otel_json_single_file(ray.raw()))
    })
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

  /// The error of the last automatic checkpoint, if it failed and no
  /// checkpoint installed since; `null` otherwise (see
  /// `Database.checkpointError`).
  #[napi]
  pub fn checkpoint_error(&self) -> Result<Option<String>> {
    self.with_kite(|ray| Ok(ray.raw().checkpoint_error()))
  }

  /// Execute a batch of operations atomically
  #[napi]
  pub fn batch(&self, env: Env, ops: Vec<Object>) -> Result<Vec<Object<'_>>> {
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
          let node_id = validation::node_id("nodeId", op.get_named_property("nodeId")?)?;
          rust_ops.push(BatchOp::DeleteNode { node_id });
        }
        "link" => {
          let src = validation::node_id("src", op.get_named_property("src")?)?;
          let dst = validation::node_id("dst", op.get_named_property("dst")?)?;
          let edge_type: String = op.get_named_property("edgeType")?;
          rust_ops.push(BatchOp::Link {
            src,
            edge_type,
            dst,
          });
        }
        "linkWithProps" => {
          let src = validation::node_id("src", op.get_named_property("src")?)?;
          let dst = validation::node_id("dst", op.get_named_property("dst")?)?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let props: Option<Object> = op.get_named_property("props")?;
          let props_map = js_props_to_map(&env, props)?;
          rust_ops.push(BatchOp::LinkWithProps {
            src,
            edge_type,
            dst,
            props: props_map,
          });
        }
        "unlink" => {
          let src = validation::node_id("src", op.get_named_property("src")?)?;
          let dst = validation::node_id("dst", op.get_named_property("dst")?)?;
          let edge_type: String = op.get_named_property("edgeType")?;
          rust_ops.push(BatchOp::Unlink {
            src,
            edge_type,
            dst,
          });
        }
        "setProp" => {
          let node_id = validation::node_id("nodeId", op.get_named_property("nodeId")?)?;
          let prop_name: String = op.get_named_property("propName")?;
          let value: Unknown = op.get_named_property("value")?;
          let prop_value = js_value_to_prop_value(&env, value)?;
          rust_ops.push(BatchOp::SetProp {
            node_id,
            prop_name,
            value: prop_value,
          });
        }
        "setEdgeProp" => {
          let src = validation::node_id("src", op.get_named_property("src")?)?;
          let dst = validation::node_id("dst", op.get_named_property("dst")?)?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let prop_name: String = op.get_named_property("propName")?;
          let value: Unknown = op.get_named_property("value")?;
          let prop_value = js_value_to_prop_value(&env, value)?;
          rust_ops.push(BatchOp::SetEdgeProp {
            src,
            edge_type,
            dst,
            prop_name,
            value: prop_value,
          });
        }
        "setEdgeProps" => {
          let src = validation::node_id("src", op.get_named_property("src")?)?;
          let dst = validation::node_id("dst", op.get_named_property("dst")?)?;
          let edge_type: String = op.get_named_property("edgeType")?;
          let props: Option<Object> = op.get_named_property("props")?;
          let props_map = js_props_to_map(&env, props)?;
          rust_ops.push(BatchOp::SetEdgeProps {
            src,
            edge_type,
            dst,
            props: props_map,
          });
        }
        "delProp" => {
          let node_id = validation::node_id("nodeId", op.get_named_property("nodeId")?)?;
          let prop_name: String = op.get_named_property("propName")?;
          rust_ops.push(BatchOp::DelProp { node_id, prop_name });
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
  pub fn from(&self, node_id: f64) -> Result<KiteTraversal> {
    let node_id = validation::node_id("nodeId", node_id)?;
    Ok(KiteTraversal::new(self.inner.clone(), vec![node_id]))
  }

  /// Begin a traversal from multiple nodes
  #[napi]
  pub fn from_nodes(&self, node_ids: Vec<f64>) -> Result<KiteTraversal> {
    Ok(KiteTraversal::new(
      self.inner.clone(),
      validation::node_ids("nodeIds", &node_ids)?,
    ))
  }

  /// Begin a path finding query
  #[napi]
  pub fn path(&self, source: f64, target: f64) -> Result<KitePath> {
    let source = validation::node_id("source", source)?;
    let target = validation::node_id("target", target)?;
    Ok(KitePath::new(self.inner.clone(), source, vec![target]))
  }

  /// Begin a path finding query to multiple targets
  #[napi]
  pub fn path_to_any(&self, source: f64, targets: Vec<f64>) -> Result<KitePath> {
    let source = validation::node_id("source", source)?;
    let targets = validation::node_ids("targets", &targets)?;
    Ok(KitePath::new(self.inner.clone(), source, targets))
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
    apply_kite_open_options(&self.options, &mut kite_opts)?;

    for node in &self.options.nodes {
      let key_spec = Arc::new(parse_key_spec(&node.name, node.key.clone())?);
      let prefix = key_spec.prefix().to_string();

      let mut node_def = NodeDef::new(&node.name, &prefix);
      if let Some(props) = node.props.as_ref() {
        for (prop_name, prop_spec) in props {
          node_def = node_def.prop(node_prop_spec_to_def(&node.name, prop_name, prop_spec)?);
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

#[cfg(test)]
mod option_validation_tests {
  use super::*;

  fn options() -> JsKiteOptions {
    JsKiteOptions {
      nodes: Vec::new(),
      edges: Vec::new(),
      read_only: None,
      create_if_missing: None,
      mvcc: None,
      mvcc_gc_interval_ms: None,
      mvcc_retention_ms: None,
      mvcc_max_chain_depth: None,
      sync_mode: None,
      group_commit_enabled: None,
      group_commit_window_ms: None,
      wal_size_mb: None,
      checkpoint_threshold: None,
      checkpoint_thread: None,
      checkpoint_log_ratio: None,
      checkpoint_log_budget: None,
      wal_segment_size: None,
      wal_segment_limit: None,
      close_checkpoint_if_wal_usage_at_least: None,
      replication_role: None,
      replication_sidecar_path: None,
      replication_source_db_path: None,
      replication_source_sidecar_path: None,
      replication_segment_max_bytes: None,
      replication_retention_min_entries: None,
      replication_retention_min_ms: None,
      strict_schema: None,
    }
  }

  #[test]
  fn validates_high_level_open_options_once_for_sync_and_async_paths() {
    let mut valid = options();
    valid.wal_size_mb = Some(1);
    valid.mvcc_gc_interval_ms = Some(1);
    valid.mvcc_retention_ms = Some(0);
    valid.mvcc_max_chain_depth = Some(1);
    valid.group_commit_window_ms = Some(0);
    valid.checkpoint_threshold = Some(0.0);
    valid.checkpoint_log_ratio = Some(0.0);
    valid.checkpoint_log_budget = Some(1.0);
    valid.wal_segment_size = Some(1.0);
    valid.wal_segment_limit = Some(1.0);
    valid.close_checkpoint_if_wal_usage_at_least = Some(1.0);
    valid.replication_segment_max_bytes = Some(1);
    valid.replication_retention_min_entries = Some(0);
    valid.replication_retention_min_ms = Some(0);
    let mut rust = KiteOptions::new();
    assert!(apply_kite_open_options(&valid, &mut rust).is_ok());

    for invalid in [
      ("mvcc_gc_interval_ms", 0),
      ("mvcc_retention_ms", -1),
      ("mvcc_max_chain_depth", 0),
      ("group_commit_window_ms", -1),
      ("wal_size_mb", 0),
      ("replication_segment_max_bytes", 0),
    ] {
      let mut candidate = options();
      match invalid.0 {
        "mvcc_gc_interval_ms" => candidate.mvcc_gc_interval_ms = Some(invalid.1),
        "mvcc_retention_ms" => candidate.mvcc_retention_ms = Some(invalid.1),
        "mvcc_max_chain_depth" => candidate.mvcc_max_chain_depth = Some(invalid.1),
        "group_commit_window_ms" => candidate.group_commit_window_ms = Some(invalid.1),
        "wal_size_mb" => candidate.wal_size_mb = Some(invalid.1),
        "replication_segment_max_bytes" => {
          candidate.replication_segment_max_bytes = Some(invalid.1)
        }
        _ => unreachable!(),
      }
      assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
    }

    let mut candidate = options();
    candidate.checkpoint_threshold = Some(2.0);
    assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
    let mut candidate = options();
    candidate.checkpoint_log_ratio = Some(-1.0);
    assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
    let mut candidate = options();
    candidate.checkpoint_log_budget = Some(0.0);
    assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
    let mut candidate = options();
    candidate.wal_segment_limit = Some(0.5);
    assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
    let mut candidate = options();
    candidate.wal_size_mb = Some((validation::MAX_BYTES / (1024 * 1024)) + 1);
    assert!(apply_kite_open_options(&candidate, &mut KiteOptions::new()).is_err());
  }

  #[test]
  fn unset_wal_size_mb_reopens_a_file_with_its_own_wal_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kite-wal-size.kitedb");
    let mut create = options();
    create.wal_size_mb = Some(1);
    let mut create_opts = KiteOptions::new();
    apply_kite_open_options(&create, &mut create_opts).expect("create options");
    RustKite::open(&path, create_opts)
      .expect("create")
      .close()
      .expect("close");

    let mut reopen_opts = KiteOptions::new();
    apply_kite_open_options(&options(), &mut reopen_opts).expect("default options");
    assert_eq!(reopen_opts.wal_size, None);
    RustKite::open(&path, reopen_opts)
      .expect("reopen without walSizeMb")
      .close()
      .expect("close reopened");
  }
}
