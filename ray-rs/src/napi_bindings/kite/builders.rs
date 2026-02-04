//! Builder pattern implementations for Kite operations
//!
//! Contains insert, upsert, and update builders for nodes and edges.

#![allow(clippy::type_complexity)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::api::kite::{Kite as RustKite, NodeRef};
use crate::types::{NodeId, PropValue};

use super::conversion::{js_props_to_map, js_value_to_prop_value, key_suffix_from_js};
use super::helpers::{get_node_props, node_to_js};
use super::key_spec::KeySpec;

// =============================================================================
// Insert Builder
// =============================================================================

/// Builder for inserting new nodes
#[napi]
pub struct KiteInsertBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) node_type: String,
  pub(crate) key_prefix: String,
  pub(crate) key_spec: Arc<KeySpec>,
}

impl KiteInsertBuilder {
  pub(crate) fn new(
    ray: Arc<RwLock<Option<RustKite>>>,
    node_type: String,
    key_prefix: String,
    key_spec: Arc<KeySpec>,
  ) -> Self {
    Self {
      ray,
      node_type,
      key_prefix,
      key_spec,
    }
  }
}

#[napi]
impl KiteInsertBuilder {
  /// Specify values for a single insert
  #[napi]
  pub fn values(
    &self,
    env: Env,
    key: Unknown,
    props: Option<Object>,
  ) -> Result<KiteInsertExecutorSingle> {
    let key_suffix = key_suffix_from_js(&env, self.key_spec.as_ref(), key)?;
    let props_map = js_props_to_map(&env, props)?;
    Ok(KiteInsertExecutorSingle {
      ray: self.ray.clone(),
      node_type: self.node_type.clone(),
      key_suffix,
      props: props_map,
    })
  }

  /// Specify values for multiple inserts
  #[napi]
  pub fn values_many(&self, env: Env, entries: Vec<Unknown>) -> Result<KiteInsertExecutorMany> {
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
      let obj = entry.coerce_to_object()?;
      let key: Unknown = obj.get_named_property("key")?;
      let props: Option<Object> = obj.get_named_property("props")?;
      let key_suffix = key_suffix_from_js(&env, self.key_spec.as_ref(), key)?;
      let props_map = js_props_to_map(&env, props)?;
      items.push((key_suffix, props_map));
    }
    Ok(KiteInsertExecutorMany {
      ray: self.ray.clone(),
      node_type: self.node_type.clone(),
      entries: items,
    })
  }
}

/// Executor for a single insert operation
#[napi]
pub struct KiteInsertExecutorSingle {
  ray: Arc<RwLock<Option<RustKite>>>,
  node_type: String,
  key_suffix: String,
  props: HashMap<String, PropValue>,
}

#[napi]
impl KiteInsertExecutorSingle {
  /// Execute the insert without returning
  #[napi]
  pub fn execute(&mut self) -> Result<()> {
    let props = std::mem::take(&mut self.props);
    insert_single_execute(&self.ray, &self.node_type, &self.key_suffix, props)
  }

  /// Execute the insert and return the node
  #[napi]
  pub fn returning(&mut self, env: Env) -> Result<Object> {
    let props = std::mem::take(&mut self.props);
    let (node_ref, props) =
      insert_single_returning(&self.ray, &self.node_type, &self.key_suffix, props)?;
    let (node_id, node_key, node_type) = node_ref.into_parts();
    node_to_js(&env, node_id, node_key, &node_type, props)
  }
}

/// Executor for multiple insert operations
#[napi]
pub struct KiteInsertExecutorMany {
  ray: Arc<RwLock<Option<RustKite>>>,
  node_type: String,
  entries: Vec<(String, HashMap<String, PropValue>)>,
}

#[napi]
impl KiteInsertExecutorMany {
  /// Execute the inserts without returning
  #[napi]
  pub fn execute(&mut self) -> Result<()> {
    let entries = std::mem::take(&mut self.entries);
    let _ = insert_many(&self.ray, &self.node_type, entries, false)?;
    Ok(())
  }

  /// Execute the inserts and return nodes
  #[napi]
  pub fn returning(&mut self, env: Env) -> Result<Vec<Object>> {
    let entries = std::mem::take(&mut self.entries);
    let results = insert_many(&self.ray, &self.node_type, entries, true)?;
    let mut out = Vec::with_capacity(results.len());
    for (node_ref, props) in results.into_iter() {
      let props =
        props.ok_or_else(|| Error::from_reason("Insert returning=true did not yield props"))?;
      let (node_id, node_key, node_type) = node_ref.into_parts();
      out.push(node_to_js(&env, node_id, node_key, &node_type, props)?);
    }
    Ok(out)
  }
}

fn insert_single_execute(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  key_suffix: &str,
  props: HashMap<String, PropValue>,
) -> Result<()> {
  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  ray
    .insert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values(key_suffix, props)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .execute()
    .map_err(|e| Error::from_reason(e.to_string()))
}

fn insert_single_returning(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  key_suffix: &str,
  props: HashMap<String, PropValue>,
) -> Result<(NodeRef, HashMap<String, PropValue>)> {
  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  let props_for_return = props.clone();
  let node_ref = ray
    .insert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values(key_suffix, props)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .returning()
    .map_err(|e| Error::from_reason(e.to_string()))?;

  Ok((node_ref, props_for_return))
}

fn insert_many(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  entries: Vec<(String, HashMap<String, PropValue>)>,
  load_props: bool,
) -> Result<Vec<(NodeRef, Option<HashMap<String, PropValue>>)>> {
  if entries.is_empty() {
    return Ok(Vec::new());
  }

  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  if !load_props {
    ray
      .insert(node_type)
      .map_err(|e| Error::from_reason(e.to_string()))?
      .values_many_owned(entries)
      .map_err(|e| Error::from_reason(e.to_string()))?
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    return Ok(Vec::new());
  }

  let props_for_return: Vec<HashMap<String, PropValue>> =
    entries.iter().map(|(_, props)| props.clone()).collect();

  let node_refs = ray
    .insert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values_many_owned(entries)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .returning()
    .map_err(|e| Error::from_reason(e.to_string()))?;

  Ok(
    node_refs
      .into_iter()
      .zip(props_for_return)
      .map(|(node_ref, props)| (node_ref, Some(props)))
      .collect(),
  )
}

// =============================================================================
// Upsert Builder
// =============================================================================

/// Builder for upserting nodes (insert or update)
#[napi]
pub struct KiteUpsertBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) node_type: String,
  pub(crate) key_prefix: String,
  pub(crate) key_spec: Arc<KeySpec>,
}

impl KiteUpsertBuilder {
  pub(crate) fn new(
    ray: Arc<RwLock<Option<RustKite>>>,
    node_type: String,
    key_prefix: String,
    key_spec: Arc<KeySpec>,
  ) -> Self {
    Self {
      ray,
      node_type,
      key_prefix,
      key_spec,
    }
  }
}

#[napi]
impl KiteUpsertBuilder {
  /// Specify values for a single upsert
  #[napi]
  pub fn values(
    &self,
    env: Env,
    key: Unknown,
    props: Option<Object>,
  ) -> Result<KiteUpsertExecutorSingle> {
    let key_suffix = key_suffix_from_js(&env, self.key_spec.as_ref(), key)?;
    let props_map = js_props_to_map(&env, props)?;
    Ok(KiteUpsertExecutorSingle {
      ray: self.ray.clone(),
      node_type: self.node_type.clone(),
      key_suffix,
      props: props_map,
    })
  }

  /// Specify values for multiple upserts
  #[napi]
  pub fn values_many(&self, env: Env, entries: Vec<Unknown>) -> Result<KiteUpsertExecutorMany> {
    let mut items = Vec::with_capacity(entries.len());
    for entry in entries {
      let obj = entry.coerce_to_object()?;
      let key: Unknown = obj.get_named_property("key")?;
      let props: Option<Object> = obj.get_named_property("props")?;
      let key_suffix = key_suffix_from_js(&env, self.key_spec.as_ref(), key)?;
      let props_map = js_props_to_map(&env, props)?;
      items.push((key_suffix, props_map));
    }
    Ok(KiteUpsertExecutorMany {
      ray: self.ray.clone(),
      node_type: self.node_type.clone(),
      entries: items,
    })
  }
}

/// Executor for a single upsert operation
#[napi]
pub struct KiteUpsertExecutorSingle {
  ray: Arc<RwLock<Option<RustKite>>>,
  node_type: String,
  key_suffix: String,
  props: HashMap<String, PropValue>,
}

#[napi]
impl KiteUpsertExecutorSingle {
  /// Execute the upsert without returning
  #[napi]
  pub fn execute(&mut self) -> Result<()> {
    let props = std::mem::take(&mut self.props);
    upsert_single_execute(&self.ray, &self.node_type, &self.key_suffix, props)
  }

  /// Execute the upsert and return the node
  #[napi]
  pub fn returning(&mut self, env: Env) -> Result<Object> {
    let props = std::mem::take(&mut self.props);
    let (node_ref, props) =
      upsert_single_returning(&self.ray, &self.node_type, &self.key_suffix, props)?;
    let (node_id, node_key, node_type) = node_ref.into_parts();
    node_to_js(&env, node_id, node_key, &node_type, props)
  }
}

/// Executor for multiple upsert operations
#[napi]
pub struct KiteUpsertExecutorMany {
  ray: Arc<RwLock<Option<RustKite>>>,
  node_type: String,
  entries: Vec<(String, HashMap<String, PropValue>)>,
}

#[napi]
impl KiteUpsertExecutorMany {
  /// Execute the upserts without returning
  #[napi]
  pub fn execute(&mut self) -> Result<()> {
    let entries = std::mem::take(&mut self.entries);
    let _ = upsert_many(&self.ray, &self.node_type, entries, false)?;
    Ok(())
  }

  /// Execute the upserts and return nodes
  #[napi]
  pub fn returning(&mut self, env: Env) -> Result<Vec<Object>> {
    let entries = std::mem::take(&mut self.entries);
    let results = upsert_many(&self.ray, &self.node_type, entries, true)?;
    let mut out = Vec::with_capacity(results.len());
    for (node_ref, props) in results.into_iter() {
      let props =
        props.ok_or_else(|| Error::from_reason("Upsert returning=true did not yield props"))?;
      let (node_id, node_key, node_type) = node_ref.into_parts();
      out.push(node_to_js(&env, node_id, node_key, &node_type, props)?);
    }
    Ok(out)
  }
}

fn upsert_single_execute(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  key_suffix: &str,
  props: HashMap<String, PropValue>,
) -> Result<()> {
  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  ray
    .upsert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values(key_suffix, props)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .execute()
    .map_err(|e| Error::from_reason(e.to_string()))
}

fn upsert_single_returning(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  key_suffix: &str,
  props: HashMap<String, PropValue>,
) -> Result<(NodeRef, HashMap<String, PropValue>)> {
  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  let node_ref = ray
    .upsert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values(key_suffix, props)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .returning()
    .map_err(|e| Error::from_reason(e.to_string()))?;

  let props_for_return = get_node_props(ray, node_ref.id());
  Ok((node_ref, props_for_return))
}

fn upsert_many(
  ray: &Arc<RwLock<Option<RustKite>>>,
  node_type: &str,
  entries: Vec<(String, HashMap<String, PropValue>)>,
  load_props: bool,
) -> Result<Vec<(NodeRef, Option<HashMap<String, PropValue>>)>> {
  if entries.is_empty() {
    return Ok(Vec::new());
  }

  let mut guard = ray.write();
  let ray = guard
    .as_mut()
    .ok_or_else(|| Error::from_reason("Kite is closed"))?;

  if !load_props {
    ray
      .upsert(node_type)
      .map_err(|e| Error::from_reason(e.to_string()))?
      .values_many_owned(entries)
      .map_err(|e| Error::from_reason(e.to_string()))?
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))?;
    return Ok(Vec::new());
  }

  let node_refs = ray
    .upsert(node_type)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .values_many_owned(entries)
    .map_err(|e| Error::from_reason(e.to_string()))?
    .returning()
    .map_err(|e| Error::from_reason(e.to_string()))?;

  Ok(
    node_refs
      .into_iter()
      .map(|node_ref| {
        let props = get_node_props(ray, node_ref.id());
        (node_ref, Some(props))
      })
      .collect(),
  )
}

// =============================================================================
// Update Builder
// =============================================================================

/// Builder for updating node properties
#[napi]
pub struct KiteUpdateBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) node_id: NodeId,
  pub(crate) updates: HashMap<String, Option<PropValue>>,
}

impl KiteUpdateBuilder {
  pub(crate) fn new(ray: Arc<RwLock<Option<RustKite>>>, node_id: NodeId) -> Self {
    Self {
      ray,
      node_id,
      updates: HashMap::new(),
    }
  }
}

#[napi]
impl KiteUpdateBuilder {
  /// Set a node property
  #[napi]
  pub fn set(&mut self, env: Env, prop_name: String, value: Unknown) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.updates.insert(prop_name, Some(prop_value));
    Ok(())
  }

  /// Remove a node property
  #[napi]
  pub fn unset(&mut self, prop_name: String) -> Result<()> {
    self.updates.insert(prop_name, None);
    Ok(())
  }

  /// Set multiple properties at once
  #[napi]
  pub fn set_all(&mut self, env: Env, props: Object) -> Result<()> {
    let props_map = js_props_to_map(&env, Some(props))?;
    for (prop_name, value) in props_map {
      self.updates.insert(prop_name, Some(value));
    }
    Ok(())
  }

  /// Execute the update
  #[napi]
  pub fn execute(&self) -> Result<()> {
    if self.updates.is_empty() {
      return Ok(());
    }
    let mut guard = self.ray.write();
    let ray = guard
      .as_mut()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    let mut builder = ray
      .update_by_id(self.node_id)
      .map_err(|e| Error::from_reason(e.to_string()))?;

    for (prop_name, value_opt) in &self.updates {
      builder = match value_opt {
        Some(value) => builder.set(prop_name, value.clone()),
        None => builder.unset(prop_name),
      };
    }

    builder
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))
  }
}

// =============================================================================
// Upsert By ID Builder
// =============================================================================

/// Builder for upserting a node by ID
#[napi]
pub struct KiteUpsertByIdBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) node_type: String,
  pub(crate) node_id: NodeId,
  pub(crate) updates: HashMap<String, Option<PropValue>>,
}

impl KiteUpsertByIdBuilder {
  pub(crate) fn new(
    ray: Arc<RwLock<Option<RustKite>>>,
    node_type: String,
    node_id: NodeId,
  ) -> Self {
    Self {
      ray,
      node_type,
      node_id,
      updates: HashMap::new(),
    }
  }
}

#[napi]
impl KiteUpsertByIdBuilder {
  /// Set a node property
  #[napi]
  pub fn set(&mut self, env: Env, prop_name: String, value: Unknown) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.updates.insert(prop_name, Some(prop_value));
    Ok(())
  }

  /// Remove a node property
  #[napi]
  pub fn unset(&mut self, prop_name: String) -> Result<()> {
    self.updates.insert(prop_name, None);
    Ok(())
  }

  /// Set multiple properties at once
  #[napi]
  pub fn set_all(&mut self, env: Env, props: Object) -> Result<()> {
    let props_map = js_props_to_map(&env, Some(props))?;
    for (prop_name, value) in props_map {
      self.updates.insert(prop_name, Some(value));
    }
    Ok(())
  }

  /// Execute the upsert
  #[napi]
  pub fn execute(&self) -> Result<()> {
    let mut guard = self.ray.write();
    let ray = guard
      .as_mut()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    let mut builder = ray
      .upsert_by_id(&self.node_type, self.node_id)
      .map_err(|e| Error::from_reason(e.to_string()))?;

    for (prop_name, value_opt) in &self.updates {
      builder = match value_opt {
        Some(value) => builder.set(prop_name, value.clone()),
        None => builder.unset(prop_name),
      };
    }

    builder
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))
  }
}

// =============================================================================
// Update Edge Builder
// =============================================================================

/// Builder for updating edge properties
#[napi]
pub struct KiteUpdateEdgeBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) src: NodeId,
  pub(crate) edge_type: String,
  pub(crate) dst: NodeId,
  pub(crate) updates: HashMap<String, Option<PropValue>>,
}

impl KiteUpdateEdgeBuilder {
  pub(crate) fn new(
    ray: Arc<RwLock<Option<RustKite>>>,
    src: NodeId,
    edge_type: String,
    dst: NodeId,
  ) -> Self {
    Self {
      ray,
      src,
      edge_type,
      dst,
      updates: HashMap::new(),
    }
  }
}

#[napi]
impl KiteUpdateEdgeBuilder {
  /// Set an edge property
  #[napi]
  pub fn set(&mut self, env: Env, prop_name: String, value: Unknown) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.updates.insert(prop_name, Some(prop_value));
    Ok(())
  }

  /// Remove an edge property
  #[napi]
  pub fn unset(&mut self, prop_name: String) -> Result<()> {
    self.updates.insert(prop_name, None);
    Ok(())
  }

  /// Set multiple edge properties at once
  #[napi]
  pub fn set_all(&mut self, env: Env, props: Object) -> Result<()> {
    let props_map = js_props_to_map(&env, Some(props))?;
    for (prop_name, value) in props_map {
      self.updates.insert(prop_name, Some(value));
    }
    Ok(())
  }

  /// Execute the edge update
  #[napi]
  pub fn execute(&self) -> Result<()> {
    if self.updates.is_empty() {
      return Ok(());
    }

    let mut guard = self.ray.write();
    let ray = guard
      .as_mut()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    let mut builder = ray
      .update_edge(self.src, &self.edge_type, self.dst)
      .map_err(|e| Error::from_reason(e.to_string()))?;

    for (prop_name, value_opt) in &self.updates {
      builder = match value_opt {
        Some(value) => builder.set(prop_name, value.clone()),
        None => builder.unset(prop_name),
      };
    }

    builder
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))
  }
}

// =============================================================================
// Upsert Edge Builder
// =============================================================================

/// Builder for upserting edges (create if not exists, update properties)
#[napi]
pub struct KiteUpsertEdgeBuilder {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) src: NodeId,
  pub(crate) edge_type: String,
  pub(crate) dst: NodeId,
  pub(crate) updates: HashMap<String, Option<PropValue>>,
}

impl KiteUpsertEdgeBuilder {
  pub(crate) fn new(
    ray: Arc<RwLock<Option<RustKite>>>,
    src: NodeId,
    edge_type: String,
    dst: NodeId,
  ) -> Self {
    Self {
      ray,
      src,
      edge_type,
      dst,
      updates: HashMap::new(),
    }
  }
}

#[napi]
impl KiteUpsertEdgeBuilder {
  /// Set an edge property
  #[napi]
  pub fn set(&mut self, env: Env, prop_name: String, value: Unknown) -> Result<()> {
    let prop_value = js_value_to_prop_value(&env, value)?;
    self.updates.insert(prop_name, Some(prop_value));
    Ok(())
  }

  /// Remove an edge property
  #[napi]
  pub fn unset(&mut self, prop_name: String) -> Result<()> {
    self.updates.insert(prop_name, None);
    Ok(())
  }

  /// Set multiple edge properties at once
  #[napi]
  pub fn set_all(&mut self, env: Env, props: Object) -> Result<()> {
    let props_map = js_props_to_map(&env, Some(props))?;
    for (prop_name, value) in props_map {
      self.updates.insert(prop_name, Some(value));
    }
    Ok(())
  }

  /// Execute the upsert
  #[napi]
  pub fn execute(&self) -> Result<()> {
    let mut guard = self.ray.write();
    let ray = guard
      .as_mut()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;

    let mut builder = ray
      .upsert_edge(self.src, &self.edge_type, self.dst)
      .map_err(|e| Error::from_reason(e.to_string()))?;

    for (prop_name, value_opt) in &self.updates {
      builder = match value_opt {
        Some(value) => builder.set(prop_name, value.clone()),
        None => builder.unset(prop_name),
      };
    }

    builder
      .execute()
      .map_err(|e| Error::from_reason(e.to_string()))
  }
}
