//! Pathfinding Algorithms
//!
//! Dijkstra and A* shortest path algorithms for graph traversal.
//! Supports weighted edges via custom weight functions.
//!
//! Ported from src/api/pathfinding.ts

use super::traversal::TraversalDirection;
use crate::types::{ETypeId, Edge, NodeId};
use crate::util::heap::IndexedMinHeap;
use std::collections::{HashMap, HashSet};

// ============================================================================
// Types
// ============================================================================

/// Result of a pathfinding query
#[derive(Debug, Clone)]
pub struct PathResult {
  /// Nodes in order from source to target
  pub path: Vec<NodeId>,
  /// Edges traversed in order (src, etype, dst)
  pub edges: Vec<(NodeId, ETypeId, NodeId)>,
  /// Sum of edge weights along the path
  pub total_weight: f64,
  /// Whether a path was found
  pub found: bool,
}

impl PathResult {
  /// Create an empty result (no path found)
  pub fn not_found() -> Self {
    Self {
      path: Vec::new(),
      edges: Vec::new(),
      total_weight: f64::INFINITY,
      found: false,
    }
  }
}

/// Internal state for pathfinding algorithms
#[derive(Debug, Clone)]
struct PathState {
  node_id: NodeId,
  cost: f64,    // g(n) - actual cost from source
  depth: usize, // Hop count from source
  parent: Option<NodeId>,
  edge: Option<(NodeId, ETypeId, NodeId)>, // Edge used to reach this node
}

/// Configuration for pathfinding
#[derive(Debug, Clone)]
pub struct PathConfig {
  /// Source node
  pub source: NodeId,
  /// Target nodes (find path to any of these)
  pub targets: HashSet<NodeId>,
  /// Allowed edge types (empty = all types)
  pub allowed_etypes: HashSet<ETypeId>,
  /// Traversal direction
  pub direction: TraversalDirection,
  /// Maximum depth to search
  pub max_depth: usize,
}

impl PathConfig {
  /// Create a new pathfinding config
  pub fn new(source: NodeId, target: NodeId) -> Self {
    let mut targets = HashSet::new();
    targets.insert(target);

    Self {
      source,
      targets,
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
    }
  }

  /// Create config with multiple targets
  pub fn with_targets(source: NodeId, targets: impl IntoIterator<Item = NodeId>) -> Self {
    Self {
      source,
      targets: targets.into_iter().collect(),
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
    }
  }

  /// Restrict to specific edge type
  pub fn via(mut self, etype: ETypeId) -> Self {
    self.allowed_etypes.insert(etype);
    self
  }

  /// Set maximum depth
  pub fn max_depth(mut self, depth: usize) -> Self {
    self.max_depth = depth;
    self
  }

  /// Set traversal direction
  pub fn direction(mut self, direction: TraversalDirection) -> Self {
    self.direction = direction;
    self
  }
}

// ============================================================================
// Dijkstra's Algorithm
// ============================================================================

/// Execute Dijkstra's shortest path algorithm
///
/// # Arguments
/// * `config` - Pathfinding configuration
/// * `get_neighbors` - Function to get neighbors for a node
/// * `get_weight` - Function to get edge weight (default: 1.0 for all edges)
///
/// # Returns
/// PathResult with the shortest path, or not_found() if no path exists
///
/// # Example
/// ```rust,no_run
/// # use raydb::api::pathfinding::{dijkstra, PathConfig};
/// # use raydb::api::traversal::TraversalDirection;
/// # use raydb::types::{Edge, ETypeId, NodeId};
/// # fn get_neighbors(
/// #   _: NodeId,
/// #   _: TraversalDirection,
/// #   _: Option<ETypeId>,
/// # ) -> Vec<Edge> {
/// #   Vec::new()
/// # }
/// # fn main() {
/// # let source_id: NodeId = 1;
/// # let target_id: NodeId = 2;
/// # let follows_etype: ETypeId = 1;
/// let config = PathConfig::new(source_id, target_id)
///     .via(follows_etype)
///     .max_depth(10);
///
/// let result = dijkstra(
///     config,
///     |node, dir, etype| get_neighbors(node, dir, etype),
///     |src, etype, dst| 1.0,  // Unweighted
/// );
///
/// if result.found {
///     println!("Path length: {}", result.path.len());
/// }
/// # }
/// ```
pub fn dijkstra<F, W>(config: PathConfig, get_neighbors: F, get_weight: W) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  let source_id = config.source;

  // Distance map: nodeId -> PathState
  let mut distances: HashMap<NodeId, PathState> = HashMap::new();
  let mut visited: HashSet<NodeId> = HashSet::new();

  // Priority queue
  let mut queue = IndexedMinHeap::new();

  // Initialize source
  distances.insert(
    source_id,
    PathState {
      node_id: source_id,
      cost: 0.0,
      depth: 0,
      parent: None,
      edge: None,
    },
  );
  queue.insert(source_id, 0.0);

  while let Some(current_id) = queue.extract_min() {
    if visited.contains(&current_id) {
      continue;
    }
    visited.insert(current_id);

    // Check if we reached a target
    if config.targets.contains(&current_id) {
      return reconstruct_path(&distances, current_id, source_id);
    }

    let current_state = distances.get(&current_id).unwrap().clone();
    if current_state.depth >= config.max_depth {
      continue;
    }

    // Get neighbors based on direction
    let directions = match config.direction {
      TraversalDirection::Both => vec![TraversalDirection::Out, TraversalDirection::In],
      dir => vec![dir],
    };

    for dir in directions {
      // Filter by edge type if specified
      let etype_filter = if config.allowed_etypes.is_empty() {
        None
      } else {
        // We need to check all allowed etypes
        // For simplicity, pass None and filter manually
        None
      };

      let neighbors = get_neighbors(current_id, dir, etype_filter);

      for edge in neighbors {
        // Filter by allowed etypes
        if !config.allowed_etypes.is_empty() && !config.allowed_etypes.contains(&edge.etype) {
          continue;
        }

        let neighbor_id = match dir {
          TraversalDirection::Out => edge.dst,
          TraversalDirection::In => edge.src,
          TraversalDirection::Both => {
            if edge.src == current_id {
              edge.dst
            } else {
              edge.src
            }
          }
        };

        if visited.contains(&neighbor_id) {
          continue;
        }

        let weight = get_weight(edge.src, edge.etype, edge.dst);
        let new_cost = current_state.cost + weight;

        // Check if we should update - use entry API to avoid borrow issues
        let existing_cost = distances.get(&neighbor_id).map(|s| s.cost);
        let should_update = existing_cost.map(|c| new_cost < c).unwrap_or(true);

        if should_update {
          let had_existing = existing_cost.is_some();

          distances.insert(
            neighbor_id,
            PathState {
              node_id: neighbor_id,
              cost: new_cost,
              depth: current_state.depth + 1,
              parent: Some(current_id),
              edge: Some((edge.src, edge.etype, edge.dst)),
            },
          );

          if had_existing {
            queue.decrease_priority(neighbor_id, new_cost);
          } else {
            queue.insert(neighbor_id, new_cost);
          }
        }
      }
    }
  }

  PathResult::not_found()
}

/// Execute A* shortest path algorithm with heuristic
///
/// # Arguments
/// * `config` - Pathfinding configuration
/// * `get_neighbors` - Function to get neighbors for a node
/// * `get_weight` - Function to get edge weight
/// * `heuristic` - Function estimating distance from node to target
///
/// # Returns
/// PathResult with the shortest path, or not_found() if no path exists
pub fn a_star<F, W, H>(
  config: PathConfig,
  get_neighbors: F,
  get_weight: W,
  heuristic: H,
) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
  H: Fn(NodeId, NodeId) -> f64,
{
  let source_id = config.source;

  // We need at least one target for heuristic
  let primary_target = *config
    .targets
    .iter()
    .next()
    .expect("A* requires at least one target");

  // State map: nodeId -> (g_score, f_score, depth, parent, edge)
  #[derive(Clone)]
  struct AStarState {
    g_score: f64,
    f_score: f64,
    depth: usize,
    parent: Option<NodeId>,
    edge: Option<(NodeId, ETypeId, NodeId)>,
  }

  let mut states: HashMap<NodeId, AStarState> = HashMap::new();
  let mut visited: HashSet<NodeId> = HashSet::new();
  let mut queue = IndexedMinHeap::new();

  // Initialize source
  let h = heuristic(source_id, primary_target);
  states.insert(
    source_id,
    AStarState {
      g_score: 0.0,
      f_score: h,
      depth: 0,
      parent: None,
      edge: None,
    },
  );
  queue.insert(source_id, h);

  while let Some(current_id) = queue.extract_min() {
    if visited.contains(&current_id) {
      continue;
    }
    visited.insert(current_id);

    // Check if we reached a target
    if config.targets.contains(&current_id) {
      // Convert AStarState to PathState for reconstruction
      let path_states: HashMap<NodeId, PathState> = states
        .iter()
        .map(|(&id, state)| {
          (
            id,
            PathState {
              node_id: id,
              cost: state.g_score,
              depth: state.depth,
              parent: state.parent,
              edge: state.edge,
            },
          )
        })
        .collect();
      return reconstruct_path(&path_states, current_id, source_id);
    }

    let current_state = states.get(&current_id).unwrap().clone();
    if current_state.depth >= config.max_depth {
      continue;
    }

    // Get neighbors
    let directions = match config.direction {
      TraversalDirection::Both => vec![TraversalDirection::Out, TraversalDirection::In],
      dir => vec![dir],
    };

    for dir in directions {
      let neighbors = get_neighbors(current_id, dir, None);

      for edge in neighbors {
        // Filter by allowed etypes
        if !config.allowed_etypes.is_empty() && !config.allowed_etypes.contains(&edge.etype) {
          continue;
        }

        let neighbor_id = match dir {
          TraversalDirection::Out => edge.dst,
          TraversalDirection::In => edge.src,
          TraversalDirection::Both => {
            if edge.src == current_id {
              edge.dst
            } else {
              edge.src
            }
          }
        };

        if visited.contains(&neighbor_id) {
          continue;
        }

        let weight = get_weight(edge.src, edge.etype, edge.dst);
        let tentative_g = current_state.g_score + weight;

        // Check if we should update - extract info to avoid borrow issues
        let existing_g_score = states.get(&neighbor_id).map(|s| s.g_score);
        let should_update = existing_g_score.map(|g| tentative_g < g).unwrap_or(true);

        if should_update {
          let had_existing = existing_g_score.is_some();
          let h = heuristic(neighbor_id, primary_target);
          let f = tentative_g + h;

          states.insert(
            neighbor_id,
            AStarState {
              g_score: tentative_g,
              f_score: f,
              depth: current_state.depth + 1,
              parent: Some(current_id),
              edge: Some((edge.src, edge.etype, edge.dst)),
            },
          );

          if had_existing {
            queue.decrease_priority(neighbor_id, f);
          } else {
            queue.insert(neighbor_id, f);
          }
        }
      }
    }
  }

  PathResult::not_found()
}

/// Reconstruct path from parent pointers
fn reconstruct_path(
  states: &HashMap<NodeId, PathState>,
  target_id: NodeId,
  source_id: NodeId,
) -> PathResult {
  let mut path = Vec::new();
  let mut edges = Vec::new();

  let mut current_id = Some(target_id);
  let mut path_states = Vec::new();

  // Walk backwards from target to source
  while let Some(id) = current_id {
    let Some(state) = states.get(&id) else {
      break;
    };

    path_states.push(state.clone());

    if id == source_id {
      break;
    }

    current_id = state.parent;
  }

  // Check if we actually reached the source
  if path_states.is_empty() || path_states.last().unwrap().node_id != source_id {
    return PathResult::not_found();
  }

  // Reverse to get source -> target order
  path_states.reverse();

  // Build path and edges
  for (i, state) in path_states.iter().enumerate() {
    path.push(state.node_id);

    if i > 0 {
      if let Some(edge) = state.edge {
        edges.push(edge);
      }
    }
  }

  // Total weight is the cost to reach the target
  let total_weight = path_states.last().map(|s| s.cost).unwrap_or(0.0);

  PathResult {
    path,
    edges,
    total_weight,
    found: true,
  }
}

// ============================================================================
// Pathfinding Builder
// ============================================================================

/// Builder for configuring pathfinding queries
pub struct PathFindingBuilder<F, W> {
  source: NodeId,
  targets: HashSet<NodeId>,
  allowed_etypes: HashSet<ETypeId>,
  direction: TraversalDirection,
  max_depth: usize,
  get_neighbors: F,
  get_weight: W,
}

impl<F, W> PathFindingBuilder<F, W>
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  /// Create a new pathfinding builder
  pub fn new(source: NodeId, get_neighbors: F, get_weight: W) -> Self {
    Self {
      source,
      targets: HashSet::new(),
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
      get_neighbors,
      get_weight,
    }
  }

  /// Set the target node
  pub fn to(mut self, target: NodeId) -> Self {
    self.targets.clear();
    self.targets.insert(target);
    self
  }

  /// Set multiple target nodes (find path to any)
  pub fn to_any(mut self, targets: impl IntoIterator<Item = NodeId>) -> Self {
    self.targets = targets.into_iter().collect();
    self
  }

  /// Restrict traversal to specific edge type
  pub fn via(mut self, etype: ETypeId) -> Self {
    self.allowed_etypes.insert(etype);
    self
  }

  /// Set maximum search depth
  pub fn max_depth(mut self, depth: usize) -> Self {
    self.max_depth = depth;
    self
  }

  /// Set traversal direction
  pub fn direction(mut self, direction: TraversalDirection) -> Self {
    self.direction = direction;
    self
  }

  /// Execute Dijkstra's algorithm
  pub fn dijkstra(self) -> PathResult {
    if self.targets.is_empty() {
      return PathResult::not_found();
    }

    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    dijkstra(config, self.get_neighbors, self.get_weight)
  }

  /// Execute A* algorithm with heuristic
  pub fn a_star<H>(self, heuristic: H) -> PathResult
  where
    H: Fn(NodeId, NodeId) -> f64,
  {
    if self.targets.is_empty() {
      return PathResult::not_found();
    }

    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    a_star(config, self.get_neighbors, self.get_weight, heuristic)
  }

  /// Find k shortest paths using Yen's algorithm
  ///
  /// Returns up to k different paths sorted by total weight.
  pub fn k_shortest(self, k: usize) -> Vec<PathResult> {
    if self.targets.is_empty() || k == 0 {
      return Vec::new();
    }

    let config = PathConfig {
      source: self.source,
      targets: self.targets,
      allowed_etypes: self.allowed_etypes,
      direction: self.direction,
      max_depth: self.max_depth,
    };

    yen_k_shortest(config, k, self.get_neighbors, self.get_weight)
  }

  /// Find all paths (alias for k_shortest with a large k)
  ///
  /// Note: This limits to 100 paths by default to prevent excessive computation.
  /// Use `k_shortest(n)` if you need a specific number.
  pub fn all_paths(self) -> Vec<PathResult> {
    self.k_shortest(100)
  }
}

// ============================================================================
// BFS (Unweighted Shortest Path)
// ============================================================================

/// Find shortest path using BFS (unweighted)
///
/// This is faster than Dijkstra for unweighted graphs.
pub fn bfs<F>(config: PathConfig, get_neighbors: F) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
{
  dijkstra(config, get_neighbors, |_, _, _| 1.0)
}

// ============================================================================
// Yen's K-Shortest Paths Algorithm
// ============================================================================

/// Find the k shortest paths using Yen's algorithm
///
/// Yen's algorithm finds the k shortest loopless paths in a graph.
/// It works by:
/// 1. Finding the shortest path using Dijkstra
/// 2. For each subsequent path, systematically "spur" from nodes of previous paths
/// 3. Use a priority queue to select the next shortest candidate path
///
/// # Arguments
/// * `config` - Pathfinding configuration (source, target, etc.)
/// * `k` - Maximum number of paths to find
/// * `get_neighbors` - Function to get neighbors for a node
/// * `get_weight` - Function to get edge weight
///
/// # Returns
/// Vector of up to k shortest paths, sorted by total weight
///
/// # Example
/// ```rust,no_run
/// # use raydb::api::pathfinding::{yen_k_shortest, PathConfig};
/// # use raydb::api::traversal::TraversalDirection;
/// # use raydb::types::{Edge, ETypeId, NodeId};
/// # fn get_neighbors(
/// #   _: NodeId,
/// #   _: TraversalDirection,
/// #   _: Option<ETypeId>,
/// # ) -> Vec<Edge> {
/// #   Vec::new()
/// # }
/// # fn main() {
/// # let source_id: NodeId = 1;
/// # let target_id: NodeId = 2;
/// let config = PathConfig::new(source_id, target_id).max_depth(10);
///
/// let paths = yen_k_shortest(
///     config,
///     3,  // Find up to 3 shortest paths
///     |node, dir, etype| get_neighbors(node, dir, etype),
///     |src, etype, dst| 1.0,  // Unweighted
/// );
///
/// for (i, path) in paths.iter().enumerate() {
///     println!("Path {}: {:?} (weight: {})", i + 1, path.path, path.total_weight);
/// }
/// # }
/// ```
pub fn yen_k_shortest<F, W>(
  config: PathConfig,
  k: usize,
  get_neighbors: F,
  get_weight: W,
) -> Vec<PathResult>
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  if k == 0 {
    return Vec::new();
  }

  // We need exactly one target for Yen's algorithm
  let target = match config.targets.iter().next() {
    Some(&t) => t,
    None => return Vec::new(),
  };

  let _source = config.source;

  // Result: the k shortest paths
  let mut result_paths: Vec<PathResult> = Vec::with_capacity(k);

  // Find the first shortest path using Dijkstra
  let first_path = dijkstra(config.clone(), &get_neighbors, &get_weight);
  if !first_path.found {
    return Vec::new();
  }
  result_paths.push(first_path);

  if k == 1 {
    return result_paths;
  }

  // Candidate paths (potential k-shortest paths) stored as (weight, path)
  // Using BinaryHeap would be ideal but we need custom ordering
  let mut candidates: Vec<PathResult> = Vec::new();

  // For each path we've found (except we keep finding more)
  for path_idx in 0..k - 1 {
    if path_idx >= result_paths.len() {
      break;
    }

    let prev_path = &result_paths[path_idx];
    let prev_path_nodes = &prev_path.path;

    // For each node in the previous path (except the last), use it as a spur node
    for spur_idx in 0..prev_path_nodes.len().saturating_sub(1) {
      let spur_node = prev_path_nodes[spur_idx];

      // Root path: path from source to spur node
      let root_path: Vec<NodeId> = prev_path_nodes[..=spur_idx].to_vec();
      let root_edges: Vec<(NodeId, ETypeId, NodeId)> = if spur_idx > 0 {
        prev_path.edges[..spur_idx].to_vec()
      } else {
        Vec::new()
      };

      // Calculate root path weight
      let root_weight: f64 = root_edges
        .iter()
        .map(|(s, e, d)| get_weight(*s, *e, *d))
        .sum();

      // Collect edges to exclude (edges used by paths that share this root)
      let mut excluded_edges: HashSet<(NodeId, ETypeId, NodeId)> = HashSet::new();

      for existing_path in &result_paths {
        // Check if this path shares the same root
        if existing_path.path.len() > spur_idx && existing_path.path[..=spur_idx] == root_path[..] {
          // Exclude the edge that leaves the spur node in this path
          if let Some(&edge) = existing_path.edges.get(spur_idx) {
            excluded_edges.insert(edge);
          }
        }
      }

      // Also exclude edges from candidate paths
      for candidate in &candidates {
        if candidate.path.len() > spur_idx && candidate.path[..=spur_idx] == root_path[..] {
          if let Some(&edge) = candidate.edges.get(spur_idx) {
            excluded_edges.insert(edge);
          }
        }
      }

      // Nodes in root path (except spur node) should be avoided
      let root_nodes: HashSet<NodeId> = root_path[..spur_idx].iter().copied().collect();

      // Create a modified get_neighbors that excludes forbidden edges and nodes
      let filtered_neighbors = |node: NodeId, dir: TraversalDirection, etype: Option<ETypeId>| {
        get_neighbors(node, dir, etype)
          .into_iter()
          .filter(|edge| {
            // Don't use excluded edges from spur node
            if node == spur_node && excluded_edges.contains(&(edge.src, edge.etype, edge.dst)) {
              return false;
            }
            // Don't go to nodes in the root path
            let neighbor = if dir == TraversalDirection::In {
              edge.src
            } else {
              edge.dst
            };
            !root_nodes.contains(&neighbor)
          })
          .collect()
      };

      // Find spur path from spur_node to target
      let spur_config = PathConfig {
        source: spur_node,
        targets: {
          let mut s = HashSet::new();
          s.insert(target);
          s
        },
        allowed_etypes: config.allowed_etypes.clone(),
        direction: config.direction,
        max_depth: config.max_depth.saturating_sub(spur_idx),
      };

      let spur_path = dijkstra(spur_config, filtered_neighbors, &get_weight);

      if spur_path.found {
        // Combine root path + spur path (excluding duplicate spur node)
        let mut combined_path = root_path.clone();
        combined_path.extend(spur_path.path.into_iter().skip(1));

        let mut combined_edges = root_edges.clone();
        combined_edges.extend(spur_path.edges);

        let combined_weight = root_weight + spur_path.total_weight;

        let candidate = PathResult {
          path: combined_path,
          edges: combined_edges,
          total_weight: combined_weight,
          found: true,
        };

        // Only add if we haven't seen this exact path before
        let is_duplicate = result_paths.iter().any(|p| p.path == candidate.path)
          || candidates.iter().any(|p| p.path == candidate.path);

        if !is_duplicate {
          candidates.push(candidate);
        }
      }
    }

    // If we have candidates, add the shortest one to results
    if !candidates.is_empty() {
      // Find the candidate with minimum weight
      candidates.sort_by(|a, b| {
        a.total_weight
          .partial_cmp(&b.total_weight)
          .unwrap_or(std::cmp::Ordering::Equal)
      });

      let best = candidates.remove(0);
      result_paths.push(best);
    } else {
      // No more candidates, we've found all possible paths
      break;
    }
  }

  result_paths
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn mock_graph() -> impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge> {
    // Graph:
    //   1 --1--> 2 --1--> 3
    //   |        |
    //   1        1
    //   v        v
    //   4 --1--> 5
    //
    // Weight on edge from 1->4 is 2, others are 1
    move |node_id: NodeId, direction: TraversalDirection, _etype: Option<ETypeId>| {
      let mut edges = Vec::new();

      match direction {
        TraversalDirection::Out => match node_id {
          1 => {
            edges.push(Edge {
              src: 1,
              etype: 1,
              dst: 2,
            });
            edges.push(Edge {
              src: 1,
              etype: 1,
              dst: 4,
            });
          }
          2 => {
            edges.push(Edge {
              src: 2,
              etype: 1,
              dst: 3,
            });
            edges.push(Edge {
              src: 2,
              etype: 1,
              dst: 5,
            });
          }
          4 => {
            edges.push(Edge {
              src: 4,
              etype: 1,
              dst: 5,
            });
          }
          _ => {}
        },
        TraversalDirection::In => match node_id {
          2 => edges.push(Edge {
            src: 1,
            etype: 1,
            dst: 2,
          }),
          3 => edges.push(Edge {
            src: 2,
            etype: 1,
            dst: 3,
          }),
          4 => edges.push(Edge {
            src: 1,
            etype: 1,
            dst: 4,
          }),
          5 => {
            edges.push(Edge {
              src: 2,
              etype: 1,
              dst: 5,
            });
            edges.push(Edge {
              src: 4,
              etype: 1,
              dst: 5,
            });
          }
          _ => {}
        },
        TraversalDirection::Both => {
          let out = mock_graph()(node_id, TraversalDirection::Out, None);
          let in_edges = mock_graph()(node_id, TraversalDirection::In, None);
          edges.extend(out);
          edges.extend(in_edges);
        }
      }

      edges
    }
  }

  fn weight_fn(src: NodeId, _etype: ETypeId, dst: NodeId) -> f64 {
    // Edge 1->4 has weight 2, others have weight 1
    if src == 1 && dst == 4 {
      2.0
    } else {
      1.0
    }
  }

  #[test]
  fn test_dijkstra_direct_path() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 2).via(1);

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2]);
    assert_eq!(result.total_weight, 1.0);
  }

  #[test]
  fn test_dijkstra_two_hop() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1);

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_dijkstra_weighted() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    // Unweighted: 1->4->5 or 1->2->5 both have 2 hops
    // Weighted: 1->2->5 = 1+1=2, 1->4->5 = 2+1=3
    let result = dijkstra(config, get_neighbors, weight_fn);

    assert!(result.found);
    // Should prefer 1->2->5 (weight 2) over 1->4->5 (weight 3)
    assert_eq!(result.path, vec![1, 2, 5]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_dijkstra_no_path() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(3, 1).via(1); // Can't go backwards

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(!result.found);
    assert!(result.path.is_empty());
  }

  #[test]
  fn test_dijkstra_max_depth() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1).max_depth(1);

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(!result.found); // 3 is 2 hops away
  }

  #[test]
  fn test_dijkstra_multiple_targets() {
    let get_neighbors = mock_graph();
    let config = PathConfig::with_targets(1, vec![3, 4]).via(1);

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(result.found);
    // Should find 4 first (1 hop) not 3 (2 hops)
    assert_eq!(result.path, vec![1, 4]);
  }

  #[test]
  fn test_a_star() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    // Simple heuristic: always returns 0 (degenerates to Dijkstra)
    let result = a_star(config, get_neighbors, weight_fn, |_, _| 0.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 5]);
  }

  #[test]
  fn test_bfs() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let result = bfs(config, get_neighbors);

    assert!(result.found);
    // BFS finds shortest path by hops (either 1->2->5 or 1->4->5, both 2 hops)
    assert_eq!(result.path.len(), 3);
    assert_eq!(result.path[0], 1);
    assert_eq!(result.path[2], 5);
  }

  #[test]
  fn test_builder() {
    let get_neighbors = mock_graph();

    let result = PathFindingBuilder::new(1, get_neighbors, |_, _, _| 1.0)
      .to(3)
      .via(1)
      .max_depth(10)
      .dijkstra();

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
  }

  #[test]
  fn test_builder_no_target() {
    let get_neighbors = mock_graph();

    let result = PathFindingBuilder::new(1, get_neighbors, |_, _, _| 1.0)
      .via(1)
      .dijkstra();

    assert!(!result.found);
  }

  #[test]
  fn test_same_source_target() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 1).via(1);

    let result = dijkstra(config, get_neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1]);
    assert_eq!(result.total_weight, 0.0);
  }

  // ========================================================================
  // Yen's K-Shortest Paths Tests
  // ========================================================================

  #[test]
  fn test_yen_single_path() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1);

    let paths = yen_k_shortest(config, 1, get_neighbors, |_, _, _| 1.0);

    assert_eq!(paths.len(), 1);
    assert!(paths[0].found);
    assert_eq!(paths[0].path, vec![1, 2, 3]);
  }

  #[test]
  fn test_yen_two_paths_to_node_5() {
    // Graph has two paths to node 5:
    // 1->2->5 (weight 2) and 1->4->5 (weight 3)
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 3, get_neighbors, weight_fn);

    assert!(paths.len() >= 2);

    // First path should be the shortest (1->2->5, weight 2)
    assert_eq!(paths[0].path, vec![1, 2, 5]);
    assert_eq!(paths[0].total_weight, 2.0);

    // Second path should be (1->4->5, weight 3)
    assert_eq!(paths[1].path, vec![1, 4, 5]);
    assert_eq!(paths[1].total_weight, 3.0);
  }

  #[test]
  fn test_yen_paths_sorted_by_weight() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 10, get_neighbors, weight_fn);

    // Verify paths are sorted by weight
    for i in 1..paths.len() {
      assert!(
        paths[i].total_weight >= paths[i - 1].total_weight,
        "Paths should be sorted by weight"
      );
    }
  }

  #[test]
  fn test_yen_no_path() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(3, 1).via(1); // Can't go backwards

    let paths = yen_k_shortest(config, 3, get_neighbors, |_, _, _| 1.0);

    assert!(paths.is_empty());
  }

  #[test]
  fn test_yen_k_zero() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 0, get_neighbors, |_, _, _| 1.0);

    assert!(paths.is_empty());
  }

  #[test]
  fn test_yen_no_duplicate_paths() {
    let get_neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 10, get_neighbors, |_, _, _| 1.0);

    // Check no duplicate paths
    for i in 0..paths.len() {
      for j in i + 1..paths.len() {
        assert_ne!(
          paths[i].path, paths[j].path,
          "Should not have duplicate paths"
        );
      }
    }
  }

  #[test]
  fn test_yen_builder() {
    let get_neighbors = mock_graph();

    let paths = PathFindingBuilder::new(1, get_neighbors, weight_fn)
      .to(5)
      .via(1)
      .k_shortest(3);

    assert!(paths.len() >= 2);
    assert_eq!(paths[0].path, vec![1, 2, 5]);
    assert_eq!(paths[1].path, vec![1, 4, 5]);
  }

  #[test]
  fn test_yen_all_paths() {
    let get_neighbors = mock_graph();

    let paths = PathFindingBuilder::new(1, get_neighbors, |_, _, _| 1.0)
      .to(5)
      .via(1)
      .all_paths();

    // Should find at least the two known paths
    assert!(paths.len() >= 2);
  }
}
