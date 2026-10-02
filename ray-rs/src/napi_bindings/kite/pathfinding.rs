//! Path finding builders and result types

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use crate::api::kite::Kite as RustKite;
use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig, PathResult};
use crate::api::traversal::TraversalDirection;
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue};

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
  /// Edge prop whose value weighs each edge in `find` and `findKShortest` (unset: weight 1).
  weight_prop: Option<String>,
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
      weight_prop: None,
    }
  }

  fn config(&self) -> PathConfig {
    PathConfig {
      source: self.source,
      targets: self.targets.clone(),
      allowed_etypes: self.allowed_etypes.clone(),
      direction: self.direction,
      max_depth: self.max_depth,
    }
  }

  fn with_ray<R>(&self, f: impl FnOnce(&RustKite) -> Result<R>) -> Result<R> {
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    f(ray)
  }
}

/// Parse a traversal direction case-insensitively: "out", "In", `TraversalDirection.Both`, ...
fn parse_direction(direction: &str) -> Result<TraversalDirection> {
  match direction.to_ascii_lowercase().as_str() {
    "out" => Ok(TraversalDirection::Out),
    "in" => Ok(TraversalDirection::In),
    "both" => Ok(TraversalDirection::Both),
    _ => Err(validation::invalid_argument(format!(
      "direction must be 'out', 'in' or 'both', got {direction:?}"
    ))),
  }
}

/// Edge weights for Dijkstra and Yen: the value of the weight prop on each edge, 1 for an edge
/// without it. A non-numeric, negative or non-finite value is an error; the weight callback
/// cannot fail, so the first error is kept and returned by `finish`.
struct EdgeWeights<'a> {
  ray: &'a RustKite,
  /// The weight prop's name and key id (`None` until some value has been written).
  prop: Option<(&'a str, Option<PropKeyId>)>,
  error: RefCell<Option<Error>>,
}

impl<'a> EdgeWeights<'a> {
  fn new(ray: &'a RustKite, prop: Option<&'a str>) -> Self {
    Self {
      ray,
      prop: prop.map(|name| (name, ray.raw().propkey_id(name))),
      error: RefCell::new(None),
    }
  }

  fn weight(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> f64 {
    let Some((name, Some(key_id))) = self.prop else {
      return 1.0;
    };
    let invalid = match self.ray.raw().edge_prop(src, etype, dst, key_id) {
      None | Some(PropValue::Null) => return 1.0,
      Some(PropValue::I64(value)) if value >= 0 => return value as f64,
      Some(PropValue::F64(value)) if value.is_finite() && value >= 0.0 => return value,
      Some(PropValue::I64(value)) => format!("{value}"),
      Some(PropValue::F64(value)) => format!("{value}"),
      Some(other) => format!("a {:?}", other.tag()),
    };
    self.error.borrow_mut().get_or_insert_with(|| {
      validation::invalid_argument(format!(
        "weight prop '{name}' of edge {src}-[{etype}]->{dst} must be a finite number >= 0, got {invalid}"
      ))
    });
    // Any valid weight: the search result is discarded.
    1.0
  }

  fn finish<T>(self, result: T) -> Result<T> {
    match self.error.into_inner() {
      Some(error) => Err(error),
      None => Ok(result),
    }
  }
}

#[napi]
impl KitePath {
  #[napi]
  pub fn via(&mut self, edge_type: String) -> Result<()> {
    let etype_id = self.with_ray(|ray| {
      let edge_def = ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      edge_def
        .etype_id
        .ok_or_else(|| Error::from_reason("Edge type not initialized"))
    })?;
    self.allowed_etypes.insert(etype_id);
    Ok(())
  }

  #[napi]
  pub fn max_depth(&mut self, depth: i64) -> Result<()> {
    self.max_depth = validation::non_negative_usize("maxDepth", depth, validation::MAX_DEPTH)?;
    Ok(())
  }

  /// Set the direction: 'out' (default), 'in' or 'both', in any letter case. Throws on any
  /// other value.
  #[napi]
  pub fn direction(&mut self, direction: String) -> Result<()> {
    self.direction = parse_direction(&direction)?;
    Ok(())
  }

  /// Weigh each edge by its numeric edge prop `propName` in `find` (Dijkstra) and
  /// `findKShortest`; an edge without the prop weighs 1. A non-numeric, negative or
  /// non-finite value makes the search throw. `findBfs` ignores weights.
  #[napi]
  pub fn weight(&mut self, prop_name: String) -> Result<()> {
    self.weight_prop = Some(prop_name);
    Ok(())
  }

  #[napi]
  pub fn bidirectional(&mut self) -> Result<()> {
    self.direction = TraversalDirection::Both;
    Ok(())
  }

  #[napi]
  pub fn find(&self) -> Result<JsPathResult> {
    self.with_ray(|ray| {
      let weights = EdgeWeights::new(ray, self.weight_prop.as_deref());
      let result = dijkstra(
        self.config(),
        |node_id, dir, etype| ray.neighbors(node_id, dir, etype),
        |src, etype, dst| weights.weight(src, etype, dst),
      );
      weights.finish(JsPathResult::from(result))
    })
  }

  #[napi]
  pub fn find_bfs(&self) -> Result<JsPathResult> {
    self.with_ray(|ray| {
      let result = bfs(self.config(), |node_id, dir, etype| {
        ray.neighbors(node_id, dir, etype)
      });
      Ok(JsPathResult::from(result))
    })
  }

  #[napi]
  pub fn find_k_shortest(&self, k: i64) -> Result<Vec<JsPathResult>> {
    let k = validation::non_negative_usize("k", k, validation::MAX_COUNT)?;
    self.with_ray(|ray| {
      let weights = EdgeWeights::new(ray, self.weight_prop.as_deref());
      let results = yen_k_shortest(
        self.config(),
        k,
        |node_id, dir, etype| ray.neighbors(node_id, dir, etype),
        |src, etype, dst| weights.weight(src, etype, dst),
      );
      weights.finish(results.into_iter().map(JsPathResult::from).collect())
    })
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn direction_names_are_case_insensitive_and_unknown_names_fail() {
    for (name, expected) in [
      ("out", TraversalDirection::Out),
      ("Out", TraversalDirection::Out),
      ("in", TraversalDirection::In),
      ("In", TraversalDirection::In),
      ("IN", TraversalDirection::In),
      ("both", TraversalDirection::Both),
      ("Both", TraversalDirection::Both),
    ] {
      assert_eq!(parse_direction(name).expect(name), expected, "{name}");
    }
    for name in ["", "sideways", "inbound", " in"] {
      assert!(parse_direction(name).is_err(), "{name:?} must be rejected");
    }
  }
}
