//! NAPI bindings for Traversal and Pathfinding
//!
//! Exposes graph traversal and pathfinding algorithms to JavaScript.

use napi_derive::napi;
use std::collections::HashSet;

use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig, PathResult};
use crate::api::traversal::{
  edges_in_direction, TraversalBuilder, TraversalDirection, TraversalResult, TraverseOptions,
};
use crate::types::{ETypeId, Edge, NodeId};

use super::validation;

// ============================================================================
// Traversal Direction
// ============================================================================

/// Direction for graph traversal
#[napi(string_enum)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsTraversalDirection {
  /// Follow outgoing edges
  Out,
  /// Follow incoming edges
  In,
  /// Follow edges in both directions
  Both,
}

impl From<JsTraversalDirection> for TraversalDirection {
  fn from(dir: JsTraversalDirection) -> Self {
    match dir {
      JsTraversalDirection::Out => TraversalDirection::Out,
      JsTraversalDirection::In => TraversalDirection::In,
      JsTraversalDirection::Both => TraversalDirection::Both,
    }
  }
}

impl From<TraversalDirection> for JsTraversalDirection {
  fn from(dir: TraversalDirection) -> Self {
    match dir {
      TraversalDirection::Out => JsTraversalDirection::Out,
      TraversalDirection::In => JsTraversalDirection::In,
      TraversalDirection::Both => JsTraversalDirection::Both,
    }
  }
}

// ============================================================================
// Traversal Result Types
// ============================================================================

/// A single result from a traversal
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsTraversalResult {
  /// The node ID that was reached
  pub node_id: i64,
  /// The depth (number of hops) from the start
  pub depth: u32,
  /// Source node of the edge used (if any)
  pub edge_src: Option<i64>,
  /// Destination node of the edge used (if any)
  pub edge_dst: Option<i64>,
  /// Edge type used (if any)
  pub edge_type: Option<u32>,
}

impl From<TraversalResult> for JsTraversalResult {
  fn from(result: TraversalResult) -> Self {
    let (edge_src, edge_dst, edge_type) = match result.edge {
      Some(edge) => (
        Some(edge.src as i64),
        Some(edge.dst as i64),
        Some(edge.etype),
      ),
      None => (None, None, None),
    };

    Self {
      node_id: result.node_id as i64,
      depth: result.depth as u32,
      edge_src,
      edge_dst,
      edge_type,
    }
  }
}

/// Options for variable-depth traversal
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct JsTraverseOptions {
  /// Direction of traversal
  pub direction: Option<JsTraversalDirection>,
  /// Minimum depth (default: 1; 0 includes the starting node)
  pub min_depth: Option<f64>,
  /// Maximum depth (0 performs no hops)
  pub max_depth: f64,
  /// Whether to only visit unique nodes (default: true)
  pub unique: Option<bool>,
}

impl JsTraverseOptions {
  pub(crate) fn to_rust(&self) -> napi::Result<TraverseOptions> {
    let min_depth = validation::count(
      "minDepth",
      self.min_depth.unwrap_or(1.0),
      validation::MAX_DEPTH,
    )?;
    let max_depth = validation::count("maxDepth", self.max_depth, validation::MAX_DEPTH)?;
    if min_depth > max_depth {
      return Err(validation::invalid_argument("minDepth must be <= maxDepth"));
    }
    Ok(TraverseOptions {
      direction: self
        .direction
        .map(Into::into)
        .unwrap_or(TraversalDirection::Out),
      min_depth,
      max_depth,
      unique: self.unique.unwrap_or(true),
      where_edge: None,
      where_node: None,
    })
  }
}

// ============================================================================
// Pathfinding Result Types
// ============================================================================

/// Result of a pathfinding query
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsPathResult {
  /// Nodes in order from source to target
  pub path: Vec<i64>,
  /// Edges as [src, etype, dst] triples
  pub edges: Vec<JsPathEdge>,
  /// Sum of edge weights along the path
  pub total_weight: f64,
  /// Whether a path was found
  pub found: bool,
}

/// An edge in a path result
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsPathEdge {
  pub src: i64,
  pub etype: u32,
  pub dst: i64,
}

impl From<PathResult> for JsPathResult {
  fn from(result: PathResult) -> Self {
    Self {
      path: result.path.iter().map(|&id| id as i64).collect(),
      edges: result
        .edges
        .iter()
        .map(|&(src, etype, dst)| JsPathEdge {
          src: src as i64,
          etype,
          dst: dst as i64,
        })
        .collect(),
      total_weight: result.total_weight,
      found: result.found,
    }
  }
}

/// Configuration for pathfinding
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsPathConfig {
  /// Source node ID
  pub source: f64,
  /// Target node ID (for single target)
  pub target: Option<f64>,
  /// Multiple target node IDs (find path to any)
  pub targets: Option<Vec<f64>>,
  /// Allowed edge types (empty = all)
  pub allowed_edge_types: Option<Vec<f64>>,
  /// Edge weight property key ID (optional)
  pub weight_key_id: Option<f64>,
  /// Edge weight property key name (optional)
  pub weight_key_name: Option<String>,
  /// Traversal direction
  pub direction: Option<JsTraversalDirection>,
  /// Maximum search depth
  pub max_depth: Option<f64>,
}

impl JsPathConfig {
  /// The validated `weightKeyId`, if set.
  pub(crate) fn weight_key_id(&self) -> napi::Result<Option<u32>> {
    validation::opt_u32_value("weightKeyId", self.weight_key_id)
  }

  pub(crate) fn to_rust(&self) -> napi::Result<PathConfig> {
    let mut targets = HashSet::new();

    if let Some(target) = self.target {
      targets.insert(validation::node_id("target", target)?);
    }

    if let Some(target_list) = self.targets.as_ref() {
      for t in target_list {
        targets.insert(validation::node_id("targets", *t)?);
      }
    }

    let allowed_etypes: HashSet<ETypeId> = validation::u32_values(
      "allowedEdgeTypes",
      self.allowed_edge_types.as_deref().unwrap_or_default(),
    )?
    .into_iter()
    .collect();

    let max_depth = validation::count(
      "maxDepth",
      self.max_depth.unwrap_or(100.0),
      validation::MAX_DEPTH,
    )?;

    Ok(PathConfig {
      source: validation::node_id("source", self.source)?,
      targets,
      allowed_etypes,
      direction: self
        .direction
        .map(Into::into)
        .unwrap_or(TraversalDirection::Out),
      max_depth,
    })
  }
}

// ============================================================================
// Graph Accessor (for callbacks)
// ============================================================================

/// Stored graph data for traversal operations
///
/// Since NAPI doesn't support passing closures directly, we need to
/// store the graph data and query it. This struct holds edge lists
/// indexed by source and destination.
#[napi]
#[derive(Debug, Default)]
pub struct JsGraphAccessor {
  /// Outgoing edges: source -> [(etype, dst)]
  out_edges: std::collections::HashMap<NodeId, Vec<(ETypeId, NodeId)>>,
  /// Incoming edges: dst -> [(etype, src)]
  in_edges: std::collections::HashMap<NodeId, Vec<(ETypeId, NodeId)>>,
  /// Edge weights: (src, etype, dst) -> weight
  weights: std::collections::HashMap<(NodeId, ETypeId, NodeId), f64>,
}

#[napi]
impl JsGraphAccessor {
  /// Create a new empty graph accessor
  #[napi(constructor)]
  pub fn new() -> Self {
    Self::default()
  }

  fn start_nodes(start_nodes: Vec<f64>) -> napi::Result<Vec<NodeId>> {
    validation::node_ids("startNodes", &start_nodes)
  }

  /// Add an edge to the graph
  ///
  /// @param src - Source node ID
  /// @param etype - Edge type ID
  /// @param dst - Destination node ID
  /// @param weight - Optional edge weight (default: 1.0)
  #[napi]
  pub fn add_edge(
    &mut self,
    src: f64,
    etype: f64,
    dst: f64,
    weight: Option<f64>,
  ) -> napi::Result<()> {
    let etype = validation::u32_value("etype", etype)?;
    let src = validation::node_id("src", src)?;
    let dst = validation::node_id("dst", dst)?;
    if let Some(w) = weight {
      check_weight(w).map_err(|reason| {
        validation::invalid_argument(format!("weight of edge {src}->{dst}: {reason}"))
      })?;
    }

    self.out_edges.entry(src).or_default().push((etype, dst));
    self.in_edges.entry(dst).or_default().push((etype, src));

    if let Some(w) = weight {
      self.weights.insert((src, etype, dst), w);
    }
    Ok(())
  }

  /// Add multiple edges at once (more efficient than individual adds)
  ///
  /// @param edges - Array of [src, etype, dst, weight?] tuples
  #[napi]
  pub fn add_edges(&mut self, edges: Vec<JsEdgeInput>) -> napi::Result<()> {
    for edge in edges {
      self.add_edge(edge.src, edge.etype, edge.dst, edge.weight)?;
    }
    Ok(())
  }

  /// Clear all edges
  #[napi]
  pub fn clear(&mut self) {
    self.out_edges.clear();
    self.in_edges.clear();
    self.weights.clear();
  }

  /// Get the number of edges
  #[napi]
  pub fn edge_count(&self) -> u32 {
    self.out_edges.values().map(|v| v.len()).sum::<usize>() as u32
  }

  /// Get the number of unique nodes
  #[napi]
  pub fn node_count(&self) -> u32 {
    let mut nodes: HashSet<NodeId> = HashSet::new();
    nodes.extend(self.out_edges.keys());
    nodes.extend(self.in_edges.keys());
    nodes.len() as u32
  }

  /// The edges a hop expands from `node_id`, as the database's traversals list
  /// them (`edges_in_direction`): `Both` lists a self-loop once.
  fn neighbors_internal(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    type Adjacency = std::collections::HashMap<NodeId, Vec<(ETypeId, NodeId)>>;
    let typed = |adjacency: &Adjacency| -> Vec<(ETypeId, NodeId)> {
      adjacency
        .get(&node_id)
        .into_iter()
        .flatten()
        .copied()
        .filter(|&(e, _)| etype.is_none_or(|etype| etype == e))
        .collect()
    };
    edges_in_direction(
      node_id,
      direction,
      || typed(&self.out_edges),
      || typed(&self.in_edges),
    )
  }

  // Internal method to get edge weight
  fn weight_internal(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> f64 {
    self.weights.get(&(src, etype, dst)).copied().unwrap_or(1.0)
  }

  // ========================================================================
  // Traversal Methods
  // ========================================================================

  /// Execute a single-hop traversal from start nodes
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param direction - Traversal direction
  /// @param edgeType - Optional edge type filter
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse_single(
    &self,
    start_nodes: Vec<f64>,
    direction: JsTraversalDirection,
    edge_type: Option<f64>,
  ) -> napi::Result<Vec<JsTraversalResult>> {
    let edge_type = validation::opt_u32_value("edgeType", edge_type)?;
    let start = Self::start_nodes(start_nodes)?;
    let builder = TraversalBuilder::new(start);
    let builder = match direction {
      JsTraversalDirection::Out => builder.out(edge_type),
      JsTraversalDirection::In => builder.r#in(edge_type),
      JsTraversalDirection::Both => builder.both(edge_type),
    };

    Ok(
      builder
        .execute(|node_id, dir, etype| self.neighbors_internal(node_id, dir, etype))
        .map(JsTraversalResult::from)
        .collect(),
    )
  }

  /// Execute a multi-hop traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps (direction, edgeType)
  /// @param limit - Maximum number of results
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse(
    &self,
    start_nodes: Vec<f64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<f64>,
  ) -> napi::Result<Vec<JsTraversalResult>> {
    let limit = validation::opt_u32_value("limit", limit)?;
    let start = Self::start_nodes(start_nodes)?;
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.etype()?;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    if let Some(n) = limit {
      let n = validation::non_negative_usize("limit", n as i64, validation::MAX_COUNT)?;
      builder = builder.take(n);
    }

    Ok(
      builder
        .execute(|node_id, dir, etype| self.neighbors_internal(node_id, dir, etype))
        .map(JsTraversalResult::from)
        .collect(),
    )
  }

  /// Execute a variable-depth traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param edgeType - Optional edge type filter
  /// @param options - Traversal options (maxDepth, minDepth, direction, unique)
  /// @returns Array of traversal results
  #[napi]
  pub fn traverse_depth(
    &self,
    start_nodes: Vec<f64>,
    edge_type: Option<f64>,
    options: JsTraverseOptions,
  ) -> napi::Result<Vec<JsTraversalResult>> {
    let edge_type = validation::opt_u32_value("edgeType", edge_type)?;
    let start = Self::start_nodes(start_nodes)?;
    let opts = options.to_rust()?;

    Ok(
      TraversalBuilder::new(start)
        .traverse(edge_type, opts)
        .execute(|node_id, dir, etype| self.neighbors_internal(node_id, dir, etype))
        .map(JsTraversalResult::from)
        .collect(),
    )
  }

  /// Count traversal results without materializing them
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps
  /// @returns Number of results
  #[napi]
  pub fn traverse_count(
    &self,
    start_nodes: Vec<f64>,
    steps: Vec<JsTraversalStep>,
  ) -> napi::Result<u32> {
    let start = Self::start_nodes(start_nodes)?;
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.etype()?;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    Ok(builder.count(|node_id, dir, etype| self.neighbors_internal(node_id, dir, etype)) as u32)
  }

  /// Get just the node IDs from a traversal
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps
  /// @param limit - Maximum number of results
  /// @returns Array of node IDs
  #[napi]
  pub fn traverse_node_ids(
    &self,
    start_nodes: Vec<f64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<f64>,
  ) -> napi::Result<Vec<i64>> {
    let limit = validation::opt_u32_value("limit", limit)?;
    let start = Self::start_nodes(start_nodes)?;
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.etype()?;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    if let Some(n) = limit {
      let n = validation::non_negative_usize("limit", n as i64, validation::MAX_COUNT)?;
      builder = builder.take(n);
    }

    Ok(
      builder
        .collect_node_ids(|node_id, dir, etype| self.neighbors_internal(node_id, dir, etype))
        .into_iter()
        .map(|id| id as i64)
        .collect(),
    )
  }

  // ========================================================================
  // Pathfinding Methods
  // ========================================================================

  /// Find shortest path using Dijkstra's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn dijkstra(&self, config: JsPathConfig) -> napi::Result<JsPathResult> {
    let rust_config = config.to_rust()?;

    Ok(
      dijkstra(
        rust_config,
        |node_id, dir, etype| self.neighbors_internal(node_id, dir, etype),
        |src, etype, dst| self.weight_internal(src, etype, dst),
      )
      .into(),
    )
  }

  /// Find shortest path using BFS (unweighted)
  ///
  /// Faster than Dijkstra for unweighted graphs.
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn bfs(&self, config: JsPathConfig) -> napi::Result<JsPathResult> {
    let rust_config = config.to_rust()?;

    Ok(
      bfs(rust_config, |node_id, dir, etype| {
        self.neighbors_internal(node_id, dir, etype)
      })
      .into(),
    )
  }

  /// Find k shortest paths using Yen's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @param k - Maximum number of paths to find
  /// @returns Array of path results sorted by weight
  #[napi]
  pub fn k_shortest(&self, config: JsPathConfig, k: f64) -> napi::Result<Vec<JsPathResult>> {
    let k = validation::u32_value("k", k)?;
    let rust_config = config.to_rust()?;
    let k = validation::non_negative_usize("k", k as i64, validation::MAX_COUNT)?;

    Ok(
      yen_k_shortest(
        rust_config,
        k,
        |node_id, dir, etype| self.neighbors_internal(node_id, dir, etype),
        |src, etype, dst| self.weight_internal(src, etype, dst),
      )
      .into_iter()
      .map(JsPathResult::from)
      .collect(),
    )
  }

  /// Find shortest path between two nodes (convenience method)
  ///
  /// @param source - Source node ID
  /// @param target - Target node ID
  /// @param edgeType - Optional edge type filter
  /// @param maxDepth - Maximum search depth
  /// @returns Path result
  #[napi]
  pub fn shortest_path(
    &self,
    source: f64,
    target: f64,
    edge_type: Option<f64>,
    max_depth: Option<f64>,
  ) -> napi::Result<JsPathResult> {
    let config = JsPathConfig {
      source,
      target: Some(target),
      targets: None,
      allowed_edge_types: edge_type.map(|e| vec![e]),
      weight_key_id: None,
      weight_key_name: None,
      direction: Some(JsTraversalDirection::Out),
      max_depth,
    };

    self.dijkstra(config)
  }

  /// Check if a path exists between two nodes
  ///
  /// @param source - Source node ID
  /// @param target - Target node ID
  /// @param edgeType - Optional edge type filter
  /// @param maxDepth - Maximum search depth
  /// @returns true if path exists
  #[napi]
  pub fn has_path(
    &self,
    source: f64,
    target: f64,
    edge_type: Option<f64>,
    max_depth: Option<f64>,
  ) -> napi::Result<bool> {
    Ok(
      self
        .shortest_path(source, target, edge_type, max_depth)?
        .found,
    )
  }

  /// Get all nodes reachable from a source within a certain depth
  ///
  /// @param source - Source node ID
  /// @param maxDepth - Maximum depth to traverse
  /// @param edgeType - Optional edge type filter
  /// @returns Array of reachable node IDs
  #[napi]
  pub fn reachable_nodes(
    &self,
    source: f64,
    max_depth: f64,
    edge_type: Option<f64>,
  ) -> napi::Result<Vec<i64>> {
    let opts = JsTraverseOptions {
      direction: Some(JsTraversalDirection::Out),
      min_depth: Some(1.0),
      max_depth,
      unique: Some(true),
    };

    Ok(
      self
        .traverse_depth(vec![source], edge_type, opts)?
        .into_iter()
        .map(|r| r.node_id)
        .collect(),
    )
  }
}

// ============================================================================
// Helper Types
// ============================================================================

/// Edge input for bulk loading
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsEdgeInput {
  pub src: f64,
  pub etype: f64,
  pub dst: f64,
  pub weight: Option<f64>,
}

/// A single traversal step
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsTraversalStep {
  pub direction: JsTraversalDirection,
  pub edge_type: Option<f64>,
}

impl JsTraversalStep {
  /// The validated edge type filter of this step.
  pub(crate) fn etype(&self) -> napi::Result<Option<ETypeId>> {
    validation::opt_u32_value("edgeType", self.edge_type)
  }
}

/// Why `weight` cannot be a Dijkstra edge weight, if it cannot.
///
/// Dijkstra needs non-negative weights: a negative one makes it return paths
/// that are not the shortest. 0 is a valid weight; NaN breaks the ordering.
pub(crate) fn check_weight(weight: f64) -> std::result::Result<f64, String> {
  if weight.is_nan() {
    Err("NaN is not a valid edge weight".to_string())
  } else if weight < 0.0 {
    Err(format!(
      "negative weight {weight} is not supported (Dijkstra needs weights >= 0)"
    ))
  } else {
    Ok(weight)
  }
}

// ============================================================================
// Standalone Functions
// ============================================================================

/// Create a traversal step
///
/// @param direction - Traversal direction
/// @param edgeType - Optional edge type filter
/// @returns Traversal step object
#[napi]
pub fn traversal_step(
  direction: JsTraversalDirection,
  edge_type: Option<f64>,
) -> napi::Result<JsTraversalStep> {
  let step = JsTraversalStep {
    direction,
    edge_type,
  };
  step.etype()?;
  Ok(step)
}

/// Create a path configuration
///
/// @param source - Source node ID
/// @param target - Target node ID
/// @returns Path configuration object
#[napi]
pub fn path_config(source: f64, target: f64) -> JsPathConfig {
  JsPathConfig {
    source,
    target: Some(target),
    targets: None,
    allowed_edge_types: None,
    weight_key_id: None,
    weight_key_name: None,
    direction: None,
    max_depth: None,
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn create_test_graph() -> JsGraphAccessor {
    let mut graph = JsGraphAccessor::new();
    // 1 --knows(1)--> 2 --knows(1)--> 3
    // 1 --follows(2)--> 4
    // 2 --follows(2)--> 5
    graph
      .add_edge(1.0, 1.0, 2.0, Some(1.0))
      .expect("valid edge"); // 1 -knows-> 2
    graph
      .add_edge(2.0, 1.0, 3.0, Some(1.0))
      .expect("valid edge"); // 2 -knows-> 3
    graph
      .add_edge(1.0, 2.0, 4.0, Some(2.0))
      .expect("valid edge"); // 1 -follows-> 4
    graph
      .add_edge(2.0, 2.0, 5.0, Some(2.0))
      .expect("valid edge"); // 2 -follows-> 5
    graph
  }

  /// A self-loop is both an out-edge and an in-edge of its node: `Both` lists it once, as
  /// `Kite::neighbors` does.
  #[test]
  fn graph_accessor_lists_a_self_loop_once_in_both() {
    let mut graph = JsGraphAccessor::new();
    graph.add_edge(1.0, 1.0, 1.0, None).expect("self-loop");
    graph.add_edge(1.0, 1.0, 2.0, None).expect("edge");
    graph.add_edge(3.0, 1.0, 1.0, None).expect("edge");
    let edge = |src, dst| Edge { src, etype: 1, dst };
    assert_eq!(
      graph.neighbors_internal(1, TraversalDirection::Both, None),
      vec![edge(1, 1), edge(1, 2), edge(3, 1)]
    );
    assert_eq!(
      graph.neighbors_internal(1, TraversalDirection::In, Some(1)),
      vec![edge(1, 1), edge(3, 1)]
    );
  }

  #[test]
  fn test_graph_accessor_basic() {
    let graph = create_test_graph();
    assert_eq!(graph.edge_count(), 4);
    assert_eq!(graph.node_count(), 5);
  }

  #[test]
  fn test_traverse_single_hop() {
    let graph = create_test_graph();

    let results = graph
      .traverse(
        vec![1.0],
        vec![JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: Some(1.0),
        }],
        None,
      )
      .expect("valid traversal");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_traverse_two_hops() {
    let graph = create_test_graph();

    let results = graph
      .traverse(
        vec![1.0],
        vec![
          JsTraversalStep {
            direction: JsTraversalDirection::Out,
            edge_type: Some(1.0),
          },
          JsTraversalStep {
            direction: JsTraversalDirection::Out,
            edge_type: Some(1.0),
          },
        ],
        None,
      )
      .expect("valid traversal");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_traverse_all_edge_types() {
    let graph = create_test_graph();

    let results = graph
      .traverse(
        vec![1.0],
        vec![JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: None,
        }],
        None,
      )
      .expect("valid traversal");

    assert_eq!(results.len(), 2);
    let node_ids: HashSet<i64> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&4));
  }

  #[test]
  fn test_traverse_count() {
    let graph = create_test_graph();

    let count = graph.traverse_count(
      vec![1.0],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: None,
      }],
    );

    assert_eq!(count.expect("valid traversal"), 2);
  }

  #[test]
  fn test_traverse_node_ids() {
    let graph = create_test_graph();

    let ids = graph
      .traverse_node_ids(
        vec![1.0],
        vec![JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: Some(1.0),
        }],
        None,
      )
      .expect("valid traversal");

    assert_eq!(ids, vec![2]);
  }

  #[test]
  fn test_dijkstra_shortest_path() {
    let graph = create_test_graph();

    let result = graph
      .dijkstra(JsPathConfig {
        source: 1.0,
        target: Some(3.0),
        targets: None,
        allowed_edge_types: None,
        weight_key_id: None,
        weight_key_name: None,
        direction: None,
        max_depth: None,
      })
      .expect("valid path");

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_bfs_shortest_path() {
    let graph = create_test_graph();

    let result = graph
      .bfs(JsPathConfig {
        source: 1.0,
        target: Some(3.0),
        targets: None,
        allowed_edge_types: None,
        weight_key_id: None,
        weight_key_name: None,
        direction: None,
        max_depth: None,
      })
      .expect("valid path");

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
  }

  #[test]
  fn test_shortest_path_not_found() {
    let graph = create_test_graph();

    let result = graph
      .shortest_path(1.0, 999.0, None, None)
      .expect("valid path");
    assert!(!result.found);
    assert!(result.path.is_empty());
  }

  #[test]
  fn test_has_path() {
    let graph = create_test_graph();

    assert!(graph.has_path(1.0, 3.0, None, None).expect("valid path"));
    assert!(graph.has_path(1.0, 5.0, None, None).expect("valid path"));
    assert!(!graph.has_path(1.0, 999.0, None, None).expect("valid path"));
    assert!(!graph.has_path(3.0, 1.0, None, None).expect("valid path")); // No reverse path
  }

  #[test]
  fn test_reachable_nodes() {
    let graph = create_test_graph();

    let reachable = graph
      .reachable_nodes(1.0, 2.0, None)
      .expect("valid traversal");

    assert_eq!(reachable.len(), 4); // 2, 3, 4, 5
    let ids: HashSet<i64> = reachable.into_iter().collect();
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
    assert!(ids.contains(&4));
    assert!(ids.contains(&5));
  }

  #[test]
  fn test_k_shortest_paths() {
    let mut graph = JsGraphAccessor::new();
    // Create a diamond graph:
    //     2
    //    / \
    //   1   4
    //    \ /
    //     3
    graph
      .add_edge(1.0, 1.0, 2.0, Some(1.0))
      .expect("valid edge");
    graph
      .add_edge(1.0, 1.0, 3.0, Some(2.0))
      .expect("valid edge");
    graph
      .add_edge(2.0, 1.0, 4.0, Some(1.0))
      .expect("valid edge");
    graph
      .add_edge(3.0, 1.0, 4.0, Some(1.0))
      .expect("valid edge");

    let paths = graph
      .k_shortest(
        JsPathConfig {
          source: 1.0,
          target: Some(4.0),
          targets: None,
          allowed_edge_types: None,
          weight_key_id: None,
          weight_key_name: None,
          direction: None,
          max_depth: None,
        },
        2.0,
      )
      .expect("valid paths");

    assert_eq!(paths.len(), 2);
    // First path should be 1 -> 2 -> 4 (weight 2)
    assert!(paths[0].found);
    assert_eq!(paths[0].path, vec![1, 2, 4]);
    assert_eq!(paths[0].total_weight, 2.0);
    // Second path should be 1 -> 3 -> 4 (weight 3)
    assert!(paths[1].found);
    assert_eq!(paths[1].path, vec![1, 3, 4]);
    assert_eq!(paths[1].total_weight, 3.0);
  }

  #[test]
  fn test_traverse_with_limit() {
    let graph = create_test_graph();

    let results = graph
      .traverse(
        vec![1.0],
        vec![JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: None,
        }],
        Some(1.0),
      )
      .expect("valid traversal");

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn test_variable_depth_traversal() {
    let graph = create_test_graph();

    let results = graph
      .traverse_depth(
        vec![1.0],
        Some(1.0), // Only "knows" edges
        JsTraverseOptions {
          direction: Some(JsTraversalDirection::Out),
          min_depth: Some(1.0),
          max_depth: 2.0,
          unique: Some(true),
        },
      )
      .expect("valid traversal");

    // Should find: 2 (depth 1), 3 (depth 2)
    assert_eq!(results.len(), 2);
    let node_ids: HashSet<i64> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_path_config_helper() {
    let config = path_config(1.0, 5.0);
    assert_eq!(config.source, 1.0);
    assert_eq!(config.target, Some(5.0));
  }

  #[test]
  fn test_traversal_step_helper() {
    let step = traversal_step(JsTraversalDirection::Out, Some(1.0)).expect("valid step");
    assert_eq!(step.direction, JsTraversalDirection::Out);
    assert_eq!(step.edge_type, Some(1.0));
    assert!(traversal_step(JsTraversalDirection::Out, Some(-1.0)).is_err());
  }

  #[test]
  fn validates_depth_limits_and_zero_semantics() {
    assert!(JsTraverseOptions {
      direction: None,
      min_depth: Some(0.0),
      max_depth: 0.0,
      unique: None,
    }
    .to_rust()
    .is_ok());
    assert!(JsTraverseOptions {
      direction: None,
      min_depth: Some(1.0),
      max_depth: 0.0,
      unique: None,
    }
    .to_rust()
    .is_err());
    assert!(JsTraverseOptions {
      direction: None,
      min_depth: None,
      max_depth: u32::MAX as f64,
      unique: None,
    }
    .to_rust()
    .is_err());
    assert!(JsPathConfig {
      source: -1.0,
      target: Some(1.0),
      targets: None,
      allowed_edge_types: None,
      weight_key_id: None,
      weight_key_name: None,
      direction: None,
      max_depth: None,
    }
    .to_rust()
    .is_err());
  }

  #[test]
  fn traverse_single_honours_direction() {
    let graph = create_test_graph();
    let incoming = graph
      .traverse_single(vec![2.0], JsTraversalDirection::In, None)
      .expect("valid traversal");
    let sources: Vec<i64> = incoming.iter().map(|r| r.node_id).collect();
    assert_eq!(sources, vec![1]);
  }

  #[test]
  fn add_edge_rejects_negative_and_nan_weights() {
    let mut graph = JsGraphAccessor::new();
    assert!(graph.add_edge(1.0, 1.0, 2.0, Some(0.0)).is_ok());
    assert!(graph.add_edge(1.0, 1.0, 3.0, Some(-1.0)).is_err());
    assert!(graph.add_edge(1.0, 1.0, 4.0, Some(f64::NAN)).is_err());
    assert!(graph.add_edge(1.0, -1.0, 5.0, None).is_err());
  }
}
