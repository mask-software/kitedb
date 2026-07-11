//! Path finding builders and result types

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;

use crate::api::kite::Kite as RustKite;
use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig, PathResult};
use crate::api::traversal::TraversalDirection;
use crate::types::{ETypeId, NodeId};

use super::helpers::neighbors;
use crate::napi_bindings::validation;

// =============================================================================
// Path Builder
// =============================================================================

#[napi]
pub struct KitePath {
  ray: Arc<RwLock<Option<RustKite>>>,
  source: NodeId,
  targets: HashSet<NodeId>,
  allowed_etypes: HashSet<ETypeId>,
  direction: TraversalDirection,
  max_depth: usize,
}

impl KitePath {
  pub fn new(ray: Arc<RwLock<Option<RustKite>>>, source: NodeId, targets: Vec<NodeId>) -> Self {
    Self {
      ray,
      source,
      targets: targets.into_iter().collect(),
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
    }
  }
}

#[napi]
impl KitePath {
  #[napi]
  pub fn via(&mut self, edge_type: String) -> Result<()> {
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
    self.allowed_etypes.insert(etype_id);
    Ok(())
  }

  #[napi]
  pub fn max_depth(&mut self, depth: i64) -> Result<()> {
    self.max_depth = validation::non_negative_usize("maxDepth", depth, validation::MAX_DEPTH)?;
    Ok(())
  }

  #[napi]
  pub fn direction(&mut self, direction: String) -> Result<()> {
    self.direction = match direction.as_str() {
      "out" => TraversalDirection::Out,
      "in" => TraversalDirection::In,
      "both" => TraversalDirection::Both,
      _ => TraversalDirection::Out,
    };
    Ok(())
  }

  #[napi]
  pub fn bidirectional(&mut self) -> Result<()> {
    self.direction = TraversalDirection::Both;
    Ok(())
  }

  #[napi]
  pub fn find(&self) -> Result<JsPathResult> {
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    let config = PathConfig {
      source: self.source,
      targets: self.targets.clone(),
      allowed_etypes: self.allowed_etypes.clone(),
      direction: self.direction,
      max_depth: self.max_depth,
    };
    let result = dijkstra(
      config,
      |node_id, dir, etype| neighbors(ray.raw(), node_id, dir, etype),
      |_src, _etype, _dst| 1.0,
    );
    Ok(JsPathResult::from(result))
  }

  #[napi]
  pub fn find_bfs(&self) -> Result<JsPathResult> {
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    let config = PathConfig {
      source: self.source,
      targets: self.targets.clone(),
      allowed_etypes: self.allowed_etypes.clone(),
      direction: self.direction,
      max_depth: self.max_depth,
    };
    let result = bfs(config, |node_id, dir, etype| {
      neighbors(ray.raw(), node_id, dir, etype)
    });
    Ok(JsPathResult::from(result))
  }

  #[napi]
  pub fn find_k_shortest(&self, k: i64) -> Result<Vec<JsPathResult>> {
    let k = validation::non_negative_usize("k", k, validation::MAX_COUNT)?;
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    let config = PathConfig {
      source: self.source,
      targets: self.targets.clone(),
      allowed_etypes: self.allowed_etypes.clone(),
      direction: self.direction,
      max_depth: self.max_depth,
    };
    let results = yen_k_shortest(
      config,
      k,
      |node_id, dir, etype| neighbors(ray.raw(), node_id, dir, etype),
      |_src, _etype, _dst| 1.0,
    );
    Ok(results.into_iter().map(JsPathResult::from).collect())
  }
}

#[napi(object)]
pub struct JsPathEdge {
  pub src: i64,
  pub etype: i64,
  pub dst: i64,
}

#[napi(object)]
pub struct JsPathResult {
  pub path: Vec<i64>,
  pub edges: Vec<JsPathEdge>,
  pub total_weight: f64,
  pub found: bool,
}

impl From<PathResult> for JsPathResult {
  fn from(result: PathResult) -> Self {
    JsPathResult {
      path: result.path.into_iter().map(|id| id as i64).collect(),
      edges: result
        .edges
        .into_iter()
        .map(|(src, etype, dst)| JsPathEdge {
          src: src as i64,
          etype: etype as i64,
          dst: dst as i64,
        })
        .collect(),
      total_weight: result.total_weight,
      found: result.found,
    }
  }
}
