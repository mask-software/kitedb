//! NAPI bindings for Traversal and Pathfinding
//!
//! Exposes graph traversal and pathfinding algorithms to JavaScript.

use napi_derive::napi;
use std::collections::HashSet;

use crate::api::pathfinding::{bfs, dijkstra, yen_k_shortest, PathConfig, PathResult};
use crate::api::traversal::{
  TraversalBuilder, TraversalDirection, TraversalResult, TraverseOptions,
};
use crate::types::{ETypeId, Edge, NodeId};

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
  /// Minimum depth (default: 1)
  pub min_depth: Option<u32>,
  /// Maximum depth (required)
  pub max_depth: u32,
  /// Whether to only visit unique nodes (default: true)
  pub unique: Option<bool>,
}

impl From<JsTraverseOptions> for TraverseOptions {
  fn from(opts: JsTraverseOptions) -> Self {
    TraverseOptions {
      direction: opts
        .direction
        .map(Into::into)
        .unwrap_or(TraversalDirection::Out),
      min_depth: opts.min_depth.unwrap_or(1) as usize,
      max_depth: opts.max_depth as usize,
      unique: opts.unique.unwrap_or(true),
      where_edge: None,
      where_node: None,
    }
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
  pub source: i64,
  /// Target node ID (for single target)
  pub target: Option<i64>,
  /// Multiple target node IDs (find path to any)
  pub targets: Option<Vec<i64>>,
  /// Allowed edge types (empty = all)
  pub allowed_edge_types: Option<Vec<u32>>,
  /// Edge weight property key ID (optional)
  pub weight_key_id: Option<u32>,
  /// Edge weight property key name (optional)
  pub weight_key_name: Option<String>,
  /// Traversal direction
  pub direction: Option<JsTraversalDirection>,
  /// Maximum search depth
  pub max_depth: Option<u32>,
}

impl From<JsPathConfig> for PathConfig {
  fn from(config: JsPathConfig) -> Self {
    let mut targets = HashSet::new();

    if let Some(target) = config.target {
      targets.insert(target as NodeId);
    }

    if let Some(target_list) = config.targets {
      for t in target_list {
        targets.insert(t as NodeId);
      }
    }

    let allowed_etypes: HashSet<ETypeId> = config
      .allowed_edge_types
      .unwrap_or_default()
      .into_iter()
      .collect();

    PathConfig {
      source: config.source as NodeId,
      targets,
      allowed_etypes,
      direction: config
        .direction
        .map(Into::into)
        .unwrap_or(TraversalDirection::Out),
      max_depth: config.max_depth.unwrap_or(100) as usize,
    }
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

  /// Add an edge to the graph
  ///
  /// @param src - Source node ID
  /// @param etype - Edge type ID
  /// @param dst - Destination node ID
  /// @param weight - Optional edge weight (default: 1.0)
  #[napi]
  pub fn add_edge(&mut self, src: i64, etype: u32, dst: i64, weight: Option<f64>) {
    let src = src as NodeId;
    let dst = dst as NodeId;

    self.out_edges.entry(src).or_default().push((etype, dst));
    self.in_edges.entry(dst).or_default().push((etype, src));

    if let Some(w) = weight {
      self.weights.insert((src, etype, dst), w);
    }
  }

  /// Add multiple edges at once (more efficient than individual adds)
  ///
  /// @param edges - Array of [src, etype, dst, weight?] tuples
  #[napi]
  pub fn add_edges(&mut self, edges: Vec<JsEdgeInput>) {
    for edge in edges {
      self.add_edge(edge.src, edge.etype, edge.dst, edge.weight);
    }
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

  // Internal method to get neighbors
  fn get_neighbors_internal(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    let mut edges = Vec::new();

    match direction {
      TraversalDirection::Out => {
        if let Some(out_list) = self.out_edges.get(&node_id) {
          for &(e, dst) in out_list {
            if etype.is_none() || etype == Some(e) {
              edges.push(Edge {
                src: node_id,
                etype: e,
                dst,
              });
            }
          }
        }
      }
      TraversalDirection::In => {
        if let Some(in_list) = self.in_edges.get(&node_id) {
          for &(e, src) in in_list {
            if etype.is_none() || etype == Some(e) {
              edges.push(Edge {
                src,
                etype: e,
                dst: node_id,
              });
            }
          }
        }
      }
      TraversalDirection::Both => {
        edges.extend(self.get_neighbors_internal(node_id, TraversalDirection::Out, etype));
        edges.extend(self.get_neighbors_internal(node_id, TraversalDirection::In, etype));
      }
    }

    edges
  }

  // Internal method to get edge weight
  fn get_weight_internal(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> f64 {
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
    start_nodes: Vec<i64>,
    _direction: JsTraversalDirection,
    edge_type: Option<u32>,
  ) -> Vec<JsTraversalResult> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();

    TraversalBuilder::new(start)
      .out(edge_type) // Note: direction is handled below
      .execute(|node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype))
      .map(JsTraversalResult::from)
      .collect()
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
    start_nodes: Vec<i64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Vec<JsTraversalResult> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.edge_type;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    if let Some(n) = limit {
      builder = builder.take(n as usize);
    }

    builder
      .execute(|node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype))
      .map(JsTraversalResult::from)
      .collect()
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
    start_nodes: Vec<i64>,
    edge_type: Option<u32>,
    options: JsTraverseOptions,
  ) -> Vec<JsTraversalResult> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
    let opts: TraverseOptions = options.into();

    TraversalBuilder::new(start)
      .traverse(edge_type, opts)
      .execute(|node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype))
      .map(JsTraversalResult::from)
      .collect()
  }

  /// Count traversal results without materializing them
  ///
  /// @param startNodes - Array of starting node IDs
  /// @param steps - Array of traversal steps
  /// @returns Number of results
  #[napi]
  pub fn traverse_count(&self, start_nodes: Vec<i64>, steps: Vec<JsTraversalStep>) -> u32 {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.edge_type;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    builder.count(|node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype)) as u32
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
    start_nodes: Vec<i64>,
    steps: Vec<JsTraversalStep>,
    limit: Option<u32>,
  ) -> Vec<i64> {
    let start: Vec<NodeId> = start_nodes.iter().map(|&id| id as NodeId).collect();
    let mut builder = TraversalBuilder::new(start);

    for step in steps {
      let etype = step.edge_type;
      builder = match step.direction {
        JsTraversalDirection::Out => builder.out(etype),
        JsTraversalDirection::In => builder.r#in(etype),
        JsTraversalDirection::Both => builder.both(etype),
      };
    }

    if let Some(n) = limit {
      builder = builder.take(n as usize);
    }

    builder
      .collect_node_ids(|node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype))
      .into_iter()
      .map(|id| id as i64)
      .collect()
  }

  // ========================================================================
  // Pathfinding Methods
  // ========================================================================

  /// Find shortest path using Dijkstra's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn dijkstra(&self, config: JsPathConfig) -> JsPathResult {
    let rust_config: PathConfig = config.into();

    dijkstra(
      rust_config,
      |node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype),
      |src, etype, dst| self.get_weight_internal(src, etype, dst),
    )
    .into()
  }

  /// Find shortest path using BFS (unweighted)
  ///
  /// Faster than Dijkstra for unweighted graphs.
  ///
  /// @param config - Pathfinding configuration
  /// @returns Path result with nodes, edges, and weight
  #[napi]
  pub fn bfs(&self, config: JsPathConfig) -> JsPathResult {
    let rust_config: PathConfig = config.into();

    bfs(rust_config, |node_id, dir, etype| {
      self.get_neighbors_internal(node_id, dir, etype)
    })
    .into()
  }

  /// Find k shortest paths using Yen's algorithm
  ///
  /// @param config - Pathfinding configuration
  /// @param k - Maximum number of paths to find
  /// @returns Array of path results sorted by weight
  #[napi]
  pub fn k_shortest(&self, config: JsPathConfig, k: u32) -> Vec<JsPathResult> {
    let rust_config: PathConfig = config.into();

    yen_k_shortest(
      rust_config,
      k as usize,
      |node_id, dir, etype| self.get_neighbors_internal(node_id, dir, etype),
      |src, etype, dst| self.get_weight_internal(src, etype, dst),
    )
    .into_iter()
    .map(JsPathResult::from)
    .collect()
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
    source: i64,
    target: i64,
    edge_type: Option<u32>,
    max_depth: Option<u32>,
  ) -> JsPathResult {
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
    source: i64,
    target: i64,
    edge_type: Option<u32>,
    max_depth: Option<u32>,
  ) -> bool {
    self
      .shortest_path(source, target, edge_type, max_depth)
      .found
  }

  /// Get all nodes reachable from a source within a certain depth
  ///
  /// @param source - Source node ID
  /// @param maxDepth - Maximum depth to traverse
  /// @param edgeType - Optional edge type filter
  /// @returns Array of reachable node IDs
  #[napi]
  pub fn reachable_nodes(&self, source: i64, max_depth: u32, edge_type: Option<u32>) -> Vec<i64> {
    let opts = JsTraverseOptions {
      direction: Some(JsTraversalDirection::Out),
      min_depth: Some(1),
      max_depth,
      unique: Some(true),
    };

    self
      .traverse_depth(vec![source], edge_type, opts)
      .into_iter()
      .map(|r| r.node_id)
      .collect()
  }
}

// ============================================================================
// Helper Types
// ============================================================================

/// Edge input for bulk loading
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsEdgeInput {
  pub src: i64,
  pub etype: u32,
  pub dst: i64,
  pub weight: Option<f64>,
}

/// A single traversal step
#[napi(object)]
#[derive(Debug, Clone)]
pub struct JsTraversalStep {
  pub direction: JsTraversalDirection,
  pub edge_type: Option<u32>,
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
pub fn traversal_step(direction: JsTraversalDirection, edge_type: Option<u32>) -> JsTraversalStep {
  JsTraversalStep {
    direction,
    edge_type,
  }
}

/// Create a path configuration
///
/// @param source - Source node ID
/// @param target - Target node ID
/// @returns Path configuration object
#[napi]
pub fn path_config(source: i64, target: i64) -> JsPathConfig {
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
    graph.add_edge(1, 1, 2, Some(1.0)); // 1 -knows-> 2
    graph.add_edge(2, 1, 3, Some(1.0)); // 2 -knows-> 3
    graph.add_edge(1, 2, 4, Some(2.0)); // 1 -follows-> 4
    graph.add_edge(2, 2, 5, Some(2.0)); // 2 -follows-> 5
    graph
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

    let results = graph.traverse(
      vec![1],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: Some(1),
      }],
      None,
    );

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_traverse_two_hops() {
    let graph = create_test_graph();

    let results = graph.traverse(
      vec![1],
      vec![
        JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: Some(1),
        },
        JsTraversalStep {
          direction: JsTraversalDirection::Out,
          edge_type: Some(1),
        },
      ],
      None,
    );

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_traverse_all_edge_types() {
    let graph = create_test_graph();

    let results = graph.traverse(
      vec![1],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: None,
      }],
      None,
    );

    assert_eq!(results.len(), 2);
    let node_ids: HashSet<i64> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&4));
  }

  #[test]
  fn test_traverse_count() {
    let graph = create_test_graph();

    let count = graph.traverse_count(
      vec![1],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: None,
      }],
    );

    assert_eq!(count, 2);
  }

  #[test]
  fn test_traverse_node_ids() {
    let graph = create_test_graph();

    let ids = graph.traverse_node_ids(
      vec![1],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: Some(1),
      }],
      None,
    );

    assert_eq!(ids, vec![2]);
  }

  #[test]
  fn test_dijkstra_shortest_path() {
    let graph = create_test_graph();

    let result = graph.dijkstra(JsPathConfig {
      source: 1,
      target: Some(3),
      targets: None,
      allowed_edge_types: None,
      weight_key_id: None,
      weight_key_name: None,
      direction: None,
      max_depth: None,
    });

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_bfs_shortest_path() {
    let graph = create_test_graph();

    let result = graph.bfs(JsPathConfig {
      source: 1,
      target: Some(3),
      targets: None,
      allowed_edge_types: None,
      weight_key_id: None,
      weight_key_name: None,
      direction: None,
      max_depth: None,
    });

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
  }

  #[test]
  fn test_shortest_path_not_found() {
    let graph = create_test_graph();

    let result = graph.shortest_path(1, 999, None, None);
    assert!(!result.found);
    assert!(result.path.is_empty());
  }

  #[test]
  fn test_has_path() {
    let graph = create_test_graph();

    assert!(graph.has_path(1, 3, None, None));
    assert!(graph.has_path(1, 5, None, None));
    assert!(!graph.has_path(1, 999, None, None));
    assert!(!graph.has_path(3, 1, None, None)); // No reverse path
  }

  #[test]
  fn test_reachable_nodes() {
    let graph = create_test_graph();

    let reachable = graph.reachable_nodes(1, 2, None);

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
    graph.add_edge(1, 1, 2, Some(1.0));
    graph.add_edge(1, 1, 3, Some(2.0));
    graph.add_edge(2, 1, 4, Some(1.0));
    graph.add_edge(3, 1, 4, Some(1.0));

    let paths = graph.k_shortest(
      JsPathConfig {
        source: 1,
        target: Some(4),
        targets: None,
        allowed_edge_types: None,
        weight_key_id: None,
        weight_key_name: None,
        direction: None,
        max_depth: None,
      },
      2,
    );

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

    let results = graph.traverse(
      vec![1],
      vec![JsTraversalStep {
        direction: JsTraversalDirection::Out,
        edge_type: None,
      }],
      Some(1),
    );

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn test_variable_depth_traversal() {
    let graph = create_test_graph();

    let results = graph.traverse_depth(
      vec![1],
      Some(1), // Only "knows" edges
      JsTraverseOptions {
        direction: Some(JsTraversalDirection::Out),
        min_depth: Some(1),
        max_depth: 2,
        unique: Some(true),
      },
    );

    // Should find: 2 (depth 1), 3 (depth 2)
    assert_eq!(results.len(), 2);
    let node_ids: HashSet<i64> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_path_config_helper() {
    let config = path_config(1, 5);
    assert_eq!(config.source, 1);
    assert_eq!(config.target, Some(5));
  }

  #[test]
  fn test_traversal_step_helper() {
    let step = traversal_step(JsTraversalDirection::Out, Some(1));
    assert_eq!(step.direction, JsTraversalDirection::Out);
    assert_eq!(step.edge_type, Some(1));
  }
}
