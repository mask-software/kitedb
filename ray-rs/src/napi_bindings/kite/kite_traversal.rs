//! Traversal builder for fluent graph traversal API

#![allow(clippy::arc_with_non_send_sync)]

use napi::bindgen_prelude::*;
use napi::UnknownRef;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;

use crate::api::kite::Kite as RustKite;
use crate::api::traversal::{TraversalBuilder, TraversalDirection, TraversalStep, TraverseOptions};
use crate::types::{ETypeId, Edge, NodeId};

use super::helpers::{
  call_filter, edge_filter_arg, edge_filter_data, get_neighbors, node_filter_arg, node_filter_data,
  node_to_js, TraversalFilterItem,
};
use crate::napi_bindings::database::JsFullEdge;
use crate::napi_bindings::traversal::{JsTraversalDirection, JsTraverseOptions};

// =============================================================================
// Traversal Builder
// =============================================================================

#[napi]
pub struct KiteTraversal {
  pub(crate) ray: Arc<RwLock<Option<RustKite>>>,
  pub(crate) start_nodes: Vec<NodeId>,
  pub(crate) steps: StepChain,
  pub(crate) limit: Option<usize>,
  pub(crate) selected_props: Option<Vec<String>>,
  pub(crate) where_edge: Option<Arc<UnknownRef<false>>>,
  pub(crate) where_node: Option<Arc<UnknownRef<false>>>,
}

#[derive(Clone, Default)]
pub(crate) struct StepChain {
  head: Option<Arc<StepNode>>,
  len: usize,
}

struct StepNode {
  step: TraversalStep,
  prev: Option<Arc<StepNode>>,
}

impl StepChain {
  fn push(&self, step: TraversalStep) -> Self {
    Self {
      head: Some(Arc::new(StepNode {
        step,
        prev: self.head.clone(),
      })),
      len: self.len + 1,
    }
  }

  fn to_vec(&self) -> Vec<TraversalStep> {
    let mut steps = Vec::with_capacity(self.len);
    let mut current = self.head.as_ref().map(Arc::clone);
    while let Some(node) = current {
      steps.push(node.step.clone());
      current = node.prev.as_ref().map(Arc::clone);
    }
    steps.reverse();
    steps
  }
}

impl KiteTraversal {
  fn fork(&self) -> KiteTraversal {
    KiteTraversal {
      ray: self.ray.clone(),
      start_nodes: self.start_nodes.clone(),
      steps: self.steps.clone(),
      limit: self.limit,
      selected_props: self.selected_props.clone(),
      where_edge: self.where_edge.clone(),
      where_node: self.where_node.clone(),
    }
  }

  fn build_builder(&self) -> TraversalBuilder {
    let mut builder = TraversalBuilder::new(self.start_nodes.clone());
    for step in self.steps.to_vec() {
      builder.push_step(step);
    }
    if let Some(limit) = self.limit {
      builder = builder.take(limit);
    }
    if let Some(ref props) = self.selected_props {
      builder = builder.select(props.clone());
    }
    builder
  }
}

#[napi]
impl KiteTraversal {
  #[napi(js_name = "whereEdge")]
  pub fn where_edge(&self, env: Env, func: UnknownRef<false>) -> Result<KiteTraversal> {
    let value = func.get_value(&env)?;
    if value.get_type()? != ValueType::Function {
      return Err(Error::from_reason("whereEdge requires a function"));
    }
    let mut next = self.fork();
    next.where_edge = Some(Arc::new(func));
    Ok(next)
  }

  #[napi(js_name = "whereNode")]
  pub fn where_node(&self, env: Env, func: UnknownRef<false>) -> Result<KiteTraversal> {
    let value = func.get_value(&env)?;
    if value.get_type()? != ValueType::Function {
      return Err(Error::from_reason("whereNode requires a function"));
    }
    let mut next = self.fork();
    next.where_node = Some(Arc::new(func));
    Ok(next)
  }

  #[napi]
  pub fn out(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    let mut next = self.fork();
    let etype = next.resolve_etype(edge_type)?;
    next.steps = next.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::Out,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    Ok(next)
  }

  #[napi(js_name = "in")]
  pub fn in_(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    let mut next = self.fork();
    let etype = next.resolve_etype(edge_type)?;
    next.steps = next.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::In,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    Ok(next)
  }

  #[napi]
  pub fn both(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    let mut next = self.fork();
    let etype = next.resolve_etype(edge_type)?;
    next.steps = next.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::Both,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    Ok(next)
  }

  #[napi]
  pub fn traverse(
    &self,
    edge_type: Option<String>,
    options: JsTraverseOptions,
  ) -> Result<KiteTraversal> {
    let mut next = self.fork();
    let etype = next.resolve_etype(edge_type)?;
    let opts = TraverseOptions {
      max_depth: options.max_depth as usize,
      min_depth: options.min_depth.unwrap_or(1) as usize,
      direction: options
        .direction
        .map(|d| match d {
          JsTraversalDirection::Out => TraversalDirection::Out,
          JsTraversalDirection::In => TraversalDirection::In,
          JsTraversalDirection::Both => TraversalDirection::Both,
        })
        .unwrap_or(TraversalDirection::Out),
      unique: options.unique.unwrap_or(true),
      where_edge: None,
      where_node: None,
    };
    next.steps = next.steps.push(TraversalStep::Traverse { etype, options: opts });
    Ok(next)
  }

  #[napi]
  pub fn take(&self, limit: i64) -> Result<KiteTraversal> {
    let mut next = self.fork();
    next.limit = Some(limit as usize);
    Ok(next)
  }

  #[napi]
  pub fn select(&self, props: Vec<String>) -> Result<KiteTraversal> {
    let mut next = self.fork();
    next.selected_props = Some(props);
    Ok(next)
  }

  #[napi]
  pub fn nodes(&self, env: Env) -> Result<Vec<i64>> {
    let builder = self.build_builder();
    let selected_props = builder
      .selected_properties()
      .map(|props| props.iter().cloned().collect::<HashSet<String>>());

    let items = {
      let ray = self.ray.clone();
      let guard = ray.read();
      let ray = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("Kite is closed"))?;

      let results: Vec<_> = builder
        .execute(|node_id, dir, etype| get_neighbors(ray.raw(), node_id, dir, etype))
        .collect();

      let mut items = Vec::with_capacity(results.len());
      for result in results {
        let edge = result.edge.map(|edge| Edge {
          src: edge.src,
          etype: edge.etype,
          dst: edge.dst,
        });
        let edge_info = edge.as_ref().map(|edge| edge_filter_data(ray, edge));
        let node = node_filter_data(ray, result.node_id, selected_props.as_ref());
        items.push(TraversalFilterItem {
          node_id: result.node_id,
          edge,
          node,
          edge_info,
        });
      }
      items
    };

    let mut out = Vec::new();
    for item in items {
      if let Some(ref edge_filter) = self.where_edge {
        if let Some(ref edge_info) = item.edge_info {
          let arg = edge_filter_arg(&env, edge_info)?;
          if !call_filter(&env, edge_filter, arg)? {
            continue;
          }
        }
      }

      if let Some(ref node_filter) = self.where_node {
        let arg = node_filter_arg(&env, &item.node)?;
        if !call_filter(&env, node_filter, arg)? {
          continue;
        }
      }

      out.push(item.node_id as i64);
    }

    Ok(out)
  }

  #[napi(js_name = "nodesWithProps")]
  pub fn nodes_with_props(&self, env: Env) -> Result<Vec<Object>> {
    let builder = self.build_builder();
    let selected_props = builder
      .selected_properties()
      .map(|props| props.iter().cloned().collect::<HashSet<String>>());

    let items = {
      let ray = self.ray.clone();
      let guard = ray.read();
      let ray = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("Kite is closed"))?;

      let results: Vec<_> = builder
        .execute(|node_id, dir, etype| get_neighbors(ray.raw(), node_id, dir, etype))
        .collect();

      let mut items = Vec::with_capacity(results.len());
      for result in results {
        let edge = result.edge.map(|edge| Edge {
          src: edge.src,
          etype: edge.etype,
          dst: edge.dst,
        });
        let edge_info = edge.as_ref().map(|edge| edge_filter_data(ray, edge));
        let node = node_filter_data(ray, result.node_id, selected_props.as_ref());
        items.push(TraversalFilterItem {
          node_id: result.node_id,
          edge,
          node,
          edge_info,
        });
      }
      items
    };

    let mut out = Vec::new();
    for item in items {
      if let Some(ref edge_filter) = self.where_edge {
        if let Some(ref edge_info) = item.edge_info {
          let arg = edge_filter_arg(&env, edge_info)?;
          if !call_filter(&env, edge_filter, arg)? {
            continue;
          }
        }
      }

      if let Some(ref node_filter) = self.where_node {
        let arg = node_filter_arg(&env, &item.node)?;
        if !call_filter(&env, node_filter, arg)? {
          continue;
        }
      }

      let node = item.node;
      out.push(node_to_js(
        &env,
        node.id,
        Some(node.key),
        &node.node_type,
        node.props,
      )?);
    }

    Ok(out)
  }

  #[napi]
  pub fn edges(&self, env: Env) -> Result<Vec<JsFullEdge>> {
    let builder = self.build_builder();
    let selected_props = builder
      .selected_properties()
      .map(|props| props.iter().cloned().collect::<HashSet<String>>());

    let items = {
      let ray = self.ray.clone();
      let guard = ray.read();
      let ray = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("Kite is closed"))?;

      let results: Vec<_> = builder
        .execute(|node_id, dir, etype| get_neighbors(ray.raw(), node_id, dir, etype))
        .collect();

      let mut items = Vec::with_capacity(results.len());
      for result in results {
        let edge = result.edge.map(|edge| Edge {
          src: edge.src,
          etype: edge.etype,
          dst: edge.dst,
        });
        let edge_info = edge.as_ref().map(|edge| edge_filter_data(ray, edge));
        let node = node_filter_data(ray, result.node_id, selected_props.as_ref());
        items.push(TraversalFilterItem {
          node_id: result.node_id,
          edge,
          node,
          edge_info,
        });
      }
      items
    };

    let mut edges = Vec::new();
    for item in items {
      if let Some(ref edge_filter) = self.where_edge {
        if let Some(ref edge_info) = item.edge_info {
          let arg = edge_filter_arg(&env, edge_info)?;
          if !call_filter(&env, edge_filter, arg)? {
            continue;
          }
        }
      }

      if let Some(ref node_filter) = self.where_node {
        let arg = node_filter_arg(&env, &item.node)?;
        if !call_filter(&env, node_filter, arg)? {
          continue;
        }
      }

      if let Some(edge) = item.edge {
        edges.push(JsFullEdge {
          src: edge.src as i64,
          etype: edge.etype,
          dst: edge.dst as i64,
        });
      }
    }

    Ok(edges)
  }

  #[napi]
  pub fn count(&self, env: Env) -> Result<i64> {
    let builder = self.build_builder();
    let selected_props = builder
      .selected_properties()
      .map(|props| props.iter().cloned().collect::<HashSet<String>>());

    let items = {
      let ray = self.ray.clone();
      let guard = ray.read();
      let ray = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("Kite is closed"))?;

      let results: Vec<_> = builder
        .execute(|node_id, dir, etype| get_neighbors(ray.raw(), node_id, dir, etype))
        .collect();

      let mut items = Vec::with_capacity(results.len());
      for result in results {
        let edge = result.edge.map(|edge| Edge {
          src: edge.src,
          etype: edge.etype,
          dst: edge.dst,
        });
        let edge_info = edge.as_ref().map(|edge| edge_filter_data(ray, edge));
        let node = node_filter_data(ray, result.node_id, selected_props.as_ref());
        items.push(TraversalFilterItem {
          node_id: result.node_id,
          edge,
          node,
          edge_info,
        });
      }
      items
    };

    let mut count = 0i64;
    for item in items {
      if let Some(ref edge_filter) = self.where_edge {
        if let Some(ref edge_info) = item.edge_info {
          let arg = edge_filter_arg(&env, edge_info)?;
          if !call_filter(&env, edge_filter, arg)? {
            continue;
          }
        }
      }

      if let Some(ref node_filter) = self.where_node {
        let arg = node_filter_arg(&env, &item.node)?;
        if !call_filter(&env, node_filter, arg)? {
          continue;
        }
      }

      count += 1;
    }

    Ok(count)
  }

  fn resolve_etype(&self, edge_type: Option<String>) -> Result<Option<ETypeId>> {
    let edge_type = match edge_type {
      Some(edge_type) => edge_type,
      None => return Ok(None),
    };
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    let edge_def = ray
      .edge_def(&edge_type)
      .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
    let etype_id = edge_def
      .etype_id
      .ok_or_else(|| Error::from_reason("Edge type not initialized"))?;
    Ok(Some(etype_id))
  }
}
