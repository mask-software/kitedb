//! Internal helper functions for Kite operations
//!
//! Contains utility functions for node/edge conversion, filtering,
//! transaction handling, and batch operations.

use napi::bindgen_prelude::*;
use napi::UnknownRef;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::api::kite::{BatchOp, BatchResult, Kite as RustKite};
use crate::api::traversal::TraversalDirection;
use crate::core::single_file::SingleFileDB;
use crate::types::{ETypeId, Edge, NodeId, PropValue};

use super::key_spec::KeySpec;

// =============================================================================
// JS Value Output Conversion
// =============================================================================

/// Convert a PropValue to a JS Unknown value
pub(crate) fn prop_value_to_js(env: &Env, value: PropValue) -> Result<Unknown> {
  match value {
    PropValue::Null => Null.into_unknown(env),
    PropValue::Bool(v) => v.into_unknown(env),
    PropValue::I64(v) => v.into_unknown(env),
    PropValue::F64(v) => v.into_unknown(env),
    PropValue::String(v) => v.into_unknown(env),
    PropValue::VectorF32(v) => {
      let values: Vec<f64> = v.iter().map(|&value| value as f64).collect();
      values.into_unknown(env)
    }
  }
}

/// Convert a BatchResult to a JS Object
pub(crate) fn batch_result_to_js(env: &Env, result: BatchResult) -> Result<Object<'static>> {
  let mut obj = Object::new(env)?;
  match result {
    BatchResult::NodeCreated(node_ref) => {
      obj.set_named_property("type", "nodeCreated")?;
      let (node_id, node_key, node_type) = node_ref.into_parts();
      let node_obj = node_to_js(env, node_id, node_key, &node_type, HashMap::new())?;
      obj.set_named_property("node", node_obj)?;
    }
    BatchResult::NodeDeleted(deleted) => {
      obj.set_named_property("type", "nodeDeleted")?;
      obj.set_named_property("deleted", deleted)?;
    }
    BatchResult::EdgeCreated => {
      obj.set_named_property("type", "edgeCreated")?;
    }
    BatchResult::EdgeRemoved(deleted) => {
      obj.set_named_property("type", "edgeRemoved")?;
      obj.set_named_property("deleted", deleted)?;
    }
    BatchResult::PropSet => {
      obj.set_named_property("type", "propSet")?;
    }
    BatchResult::PropDeleted => {
      obj.set_named_property("type", "propDeleted")?;
    }
  }
  Ok(Object::from_raw(env.raw(), obj.raw()))
}

/// Create a JS node object with properties
pub(crate) fn node_to_js(
  env: &Env,
  node_id: NodeId,
  node_key: Option<String>,
  node_type: &str,
  props: HashMap<String, PropValue>,
) -> Result<Object<'static>> {
  let mut obj = Object::new(env)?;
  obj.set_named_property("id", node_id as i64)?;
  obj.set_named_property("key", node_key.as_deref().unwrap_or(""))?;
  obj.set_named_property("type", node_type)?;

  for (name, value) in props {
    let js_value = prop_value_to_js(env, value)?;
    obj.set_named_property(&name, js_value)?;
  }

  Ok(Object::from_raw(env.raw(), obj.raw()))
}

// =============================================================================
// Filter Data Structures
// =============================================================================

/// Data for filtering nodes
pub(crate) struct NodeFilterData {
  pub id: NodeId,
  pub key: String,
  pub node_type: String,
  pub props: HashMap<String, PropValue>,
}

/// Data for filtering edges
pub(crate) struct EdgeFilterData {
  pub src: NodeId,
  pub dst: NodeId,
  pub etype: ETypeId,
  pub props: HashMap<String, PropValue>,
}

/// Combined filter item for traversal
pub(crate) struct TraversalFilterItem {
  pub node_id: NodeId,
  pub edge: Option<Edge>,
  pub node: NodeFilterData,
  pub edge_info: Option<EdgeFilterData>,
}

/// Create node filter data from a node ID
pub(crate) fn node_filter_data(
  ray: &RustKite,
  node_id: NodeId,
  selected_props: Option<&HashSet<String>>,
) -> NodeFilterData {
  let node_ref = ray.node_by_id(node_id).ok().flatten();
  let (key, node_type) = match node_ref {
    Some(node_ref) => {
      let (_id, key, node_type) = node_ref.into_parts();
      (key.unwrap_or_default(), node_type.to_string())
    }
    None => ("".to_string(), "unknown".to_string()),
  };

  let props = get_node_props_selected(ray, node_id, selected_props);

  NodeFilterData {
    id: node_id,
    key,
    node_type,
    props,
  }
}

/// Create edge filter data from an edge
pub(crate) fn edge_filter_data(ray: &RustKite, edge: &Edge) -> EdgeFilterData {
  let mut props = HashMap::new();
  if let Some(props_by_id) = ray.raw().edge_props(edge.src, edge.etype, edge.dst) {
    for (key_id, value) in props_by_id {
      if let Some(name) = ray.raw().propkey_name(key_id) {
        props.insert(name, value);
      }
    }
  }

  EdgeFilterData {
    src: edge.src,
    dst: edge.dst,
    etype: edge.etype,
    props,
  }
}

/// Create a JS object for node filtering
pub(crate) fn node_filter_arg(env: &Env, data: &NodeFilterData) -> Result<Object<'static>> {
  let mut obj = Object::new(env)?;
  obj.set_named_property("id", data.id as i64)?;
  obj.set_named_property("key", data.key.as_str())?;
  obj.set_named_property("type", data.node_type.as_str())?;
  for (name, value) in &data.props {
    let js_value = prop_value_to_js(env, value.clone())?;
    obj.set_named_property(name, js_value)?;
  }
  Ok(Object::from_raw(env.raw(), obj.raw()))
}

/// Create a JS object for edge filtering
pub(crate) fn edge_filter_arg(env: &Env, data: &EdgeFilterData) -> Result<Object<'static>> {
  let mut obj = Object::new(env)?;
  obj.set_named_property("src", data.src as i64)?;
  obj.set_named_property("dst", data.dst as i64)?;
  obj.set_named_property("etype", data.etype)?;
  for (name, value) in &data.props {
    let js_value = prop_value_to_js(env, value.clone())?;
    obj.set_named_property(name, js_value)?;
  }
  Ok(Object::from_raw(env.raw(), obj.raw()))
}

/// Call a JS filter function with an argument
#[allow(clippy::arc_with_non_send_sync)]
pub(crate) fn call_filter(
  env: &Env,
  func_ref: &Arc<UnknownRef<false>>,
  arg: Object,
) -> Result<bool> {
  let func_value = func_ref.get_value(env)?;
  // SAFETY: func_ref is expected to reference a JS function.
  let func: Function<Unknown, Unknown> = unsafe { func_value.cast()? };
  let result: Unknown = func.call(arg.into_unknown(env)?)?;
  result.coerce_to_bool()
}

// =============================================================================
// Property Selection Helpers
// =============================================================================

/// Check if a property should be included based on selection
pub(crate) fn should_include_prop(selected_props: Option<&HashSet<String>>, name: &str) -> bool {
  selected_props.is_none_or(|set| set.contains(name))
}

/// Get node properties with optional selection
pub(crate) fn get_node_props_selected(
  ray: &RustKite,
  node_id: NodeId,
  selected_props: Option<&HashSet<String>>,
) -> HashMap<String, PropValue> {
  let mut props = HashMap::new();
  if let Some(props_by_id) = ray.raw().node_props(node_id) {
    for (key_id, value) in props_by_id {
      if let Some(name) = ray.raw().propkey_name(key_id) {
        if should_include_prop(selected_props, &name) {
          props.insert(name, value);
        }
      }
    }
  }
  props
}

/// Get all node properties
pub(crate) fn get_node_props(ray: &RustKite, node_id: NodeId) -> HashMap<String, PropValue> {
  get_node_props_selected(ray, node_id, None)
}

// =============================================================================
// Node Type Inference
// =============================================================================

/// Infer node type from key prefix
pub(crate) fn node_type_from_key(
  node_specs: &HashMap<String, Arc<KeySpec>>,
  key: &str,
) -> Option<String> {
  node_specs
    .iter()
    .find(|(_, spec)| key.starts_with(spec.prefix()))
    .map(|(name, _)| name.clone())
}

// =============================================================================
// Batch Operations
// =============================================================================

/// Execute a batch of operations
pub(crate) fn execute_batch_ops(ray: &mut RustKite, ops: Vec<BatchOp>) -> Result<Vec<BatchResult>> {
  ray
    .batch(ops)
    .map_err(|e| Error::from_reason(e.to_string()))
}

// =============================================================================
// Neighbor Traversal
// =============================================================================

/// Get neighbors for a node in a given direction
pub(crate) fn get_neighbors(
  db: &SingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let mut edges = Vec::new();
  match direction {
    TraversalDirection::Out => {
      for (edge_type, dst) in db.out_edges(node_id) {
        if etype.is_none() || etype == Some(edge_type) {
          edges.push(Edge {
            src: node_id,
            etype: edge_type,
            dst,
          });
        }
      }
    }
    TraversalDirection::In => {
      for (edge_type, src) in db.in_edges(node_id) {
        if etype.is_none() || etype == Some(edge_type) {
          edges.push(Edge {
            src,
            etype: edge_type,
            dst: node_id,
          });
        }
      }
    }
    TraversalDirection::Both => {
      edges.extend(get_neighbors(db, node_id, TraversalDirection::Out, etype));
      edges.extend(get_neighbors(db, node_id, TraversalDirection::In, etype));
    }
  }

  edges
}
