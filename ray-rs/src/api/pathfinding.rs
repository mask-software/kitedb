//! Pathfinding Algorithms
//!
//! Dijkstra and A* shortest path algorithms for graph traversal.
//! Supports weighted edges via custom weight functions.
//!
//! # Edge weights
//!
//! Dijkstra, A* and Yen's algorithm need finite, non-negative weights. An edge whose weight is
//! NaN, infinite or negative is skipped, as if it were absent.
//!
//! # Depth limit
//!
//! `max_depth` bounds the number of hops. The search returns the cheapest path within that bound,
//! even when a cheaper but deeper path reaches the same intermediate node.
//!
//! Ported from src/api/pathfinding.ts

use super::traversal::TraversalDirection;
use crate::types::{ETypeId, Edge, NodeId};
use hashbrown::{HashMap as FastMap, HashSet as FastSet};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet, VecDeque};

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

  /// The etype to pass to the neighbors function: the only allowed one, else `None` (and
  /// edges are filtered with [`Self::allows`]).
  fn neighbors_etype(&self) -> Option<ETypeId> {
    if self.allowed_etypes.len() == 1 {
      self.allowed_etypes.iter().next().copied()
    } else {
      None
    }
  }

  fn allows(&self, etype: ETypeId) -> bool {
    self.allowed_etypes.is_empty() || self.allowed_etypes.contains(&etype)
  }
}

/// Whether Dijkstra/A* can use an edge weight: finite and non-negative.
fn usable_weight(weight: f64) -> bool {
  weight.is_finite() && weight >= 0.0
}

/// The directions a search expands: `Both` is `Out`, then `In`.
fn expand_directions(direction: TraversalDirection) -> &'static [TraversalDirection] {
  match direction {
    TraversalDirection::Out => &[TraversalDirection::Out],
    TraversalDirection::In => &[TraversalDirection::In],
    TraversalDirection::Both => &[TraversalDirection::Out, TraversalDirection::In],
  }
}

fn neighbor_id_for_edge(current_id: NodeId, dir: TraversalDirection, edge: &Edge) -> NodeId {
  match dir {
    TraversalDirection::Out => edge.dst,
    TraversalDirection::In => edge.src,
    TraversalDirection::Both => {
      if edge.src == current_id {
        edge.dst
      } else {
        edge.src
      }
    }
  }
}

// ============================================================================
// Label-setting search (Dijkstra / A*)
// ============================================================================

/// One way of reaching a node.
struct Label {
  node: NodeId,
  cost: f64,
  depth: usize,
  parent: Option<usize>,
  edge: Option<(NodeId, ETypeId, NodeId)>,
  /// False once a label that reaches the node at least as cheaply and as shallowly replaced
  /// it; its queue entry is then skipped.
  alive: bool,
}

/// The labels of one node that no other label of it dominates.
#[derive(Default)]
struct Frontier {
  first: Option<usize>,
  rest: Vec<usize>,
}

impl Frontier {
  fn labels(&self) -> impl Iterator<Item = usize> + '_ {
    self.first.into_iter().chain(self.rest.iter().copied())
  }

  fn push(&mut self, label: usize) {
    if self.first.is_none() {
      self.first = Some(label);
    } else {
      self.rest.push(label);
    }
  }

  fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
    if self.first.is_some_and(|label| !keep(label)) {
      self.first = None;
    }
    self.rest.retain(|&label| keep(label));
  }
}

/// A queued label, popped by lowest priority, then lowest depth.
struct Queued {
  priority: f64,
  depth: usize,
  label: usize,
}

impl PartialEq for Queued {
  fn eq(&self, other: &Self) -> bool {
    self.cmp(other) == Ordering::Equal
  }
}

impl Eq for Queued {}

impl Ord for Queued {
  fn cmp(&self, other: &Self) -> Ordering {
    // Reversed: `BinaryHeap` is a max-heap.
    other
      .priority
      .total_cmp(&self.priority)
      .then_with(|| other.depth.cmp(&self.depth))
  }
}

impl PartialOrd for Queued {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

struct Search {
  result: PathResult,
  /// Whether a node was left unexpanded because it was at `max_depth`.
  depth_cut: bool,
}

/// Best-first search from `config.source` to the first target popped, by `cost + heuristic`.
///
/// With `track_depth`, a node is reached once per (cost, depth) trade-off: a costlier but
/// shallower label survives next to a cheaper, deeper one, so the depth limit cannot hide a
/// path. Without it, a node keeps only its cheapest label (classic Dijkstra/A*), which is exact
/// as long as the depth limit cut nothing off.
fn label_search<F, W, H>(
  config: &PathConfig,
  neighbors: &F,
  edge_weight: &W,
  heuristic: &H,
  track_depth: bool,
) -> Search
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
  H: Fn(NodeId) -> f64,
{
  // Labels compare on depth only when tracking it.
  let key = |depth: usize| if track_depth { depth } else { 0 };
  let etype_filter = config.neighbors_etype();

  let mut labels = vec![Label {
    node: config.source,
    cost: 0.0,
    depth: 0,
    parent: None,
    edge: None,
    alive: true,
  }];
  let mut frontiers: FastMap<NodeId, Frontier> = FastMap::new();
  frontiers.entry(config.source).or_default().push(0);
  // The smallest depth key a popped label of each node had.
  let mut settled: FastMap<NodeId, usize> = FastMap::new();
  let mut queue = BinaryHeap::new();
  queue.push(Queued {
    priority: heuristic(config.source),
    depth: 0,
    label: 0,
  });
  let mut depth_cut = false;

  while let Some(Queued { label: index, .. }) = queue.pop() {
    let Label {
      node,
      cost,
      depth,
      alive,
      ..
    } = labels[index];
    if !alive || settled.get(&node).is_some_and(|&seen| seen <= key(depth)) {
      continue;
    }
    settled.insert(node, key(depth));

    if config.targets.contains(&node) {
      return Search {
        result: path_from_label(&labels, index),
        depth_cut,
      };
    }
    if depth >= config.max_depth {
      depth_cut = true;
      continue;
    }

    let next_depth = depth + 1;
    for &dir in expand_directions(config.direction) {
      for edge in neighbors(node, dir, etype_filter) {
        if !config.allows(edge.etype) {
          continue;
        }
        let next = neighbor_id_for_edge(node, dir, &edge);
        if settled
          .get(&next)
          .is_some_and(|&seen| seen <= key(next_depth))
        {
          continue;
        }
        let weight = edge_weight(edge.src, edge.etype, edge.dst);
        if !usable_weight(weight) {
          continue;
        }
        let next_cost = cost + weight;

        let frontier = frontiers.entry(next).or_default();
        if frontier.labels().any(|other| {
          labels[other].cost <= next_cost && key(labels[other].depth) <= key(next_depth)
        }) {
          continue;
        }
        frontier.retain(|other| {
          let other = &mut labels[other];
          let dominated = next_cost <= other.cost && key(next_depth) <= key(other.depth);
          if dominated {
            other.alive = false;
          }
          !dominated
        });
        let next_index = labels.len();
        frontier.push(next_index);
        labels.push(Label {
          node: next,
          cost: next_cost,
          depth: next_depth,
          parent: Some(index),
          edge: Some((edge.src, edge.etype, edge.dst)),
          alive: true,
        });
        queue.push(Queued {
          priority: next_cost + heuristic(next),
          depth: next_depth,
          label: next_index,
        });
      }
    }
  }

  Search {
    result: PathResult::not_found(),
    depth_cut,
  }
}

/// The path that reached `index`, following parent labels back to the source.
fn path_from_label(labels: &[Label], index: usize) -> PathResult {
  let mut path = Vec::new();
  let mut edges = Vec::new();
  let mut current = Some(index);
  while let Some(label) = current.map(|index| &labels[index]) {
    path.push(label.node);
    edges.extend(label.edge);
    current = label.parent;
  }
  path.reverse();
  edges.reverse();

  PathResult {
    path,
    edges,
    total_weight: labels[index].cost,
    found: true,
  }
}

/// Cheapest path within `config.max_depth`: classic search first, and the depth-tracking
/// search only if the depth limit cut something off.
fn shortest_path_search<F, W, H>(
  config: &PathConfig,
  neighbors: &F,
  edge_weight: &W,
  heuristic: &H,
) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
  H: Fn(NodeId) -> f64,
{
  let classic = label_search(config, neighbors, edge_weight, heuristic, false);
  if !classic.depth_cut {
    return classic.result;
  }
  label_search(config, neighbors, edge_weight, heuristic, true).result
}

// ============================================================================
// Dijkstra's Algorithm
// ============================================================================

/// Execute Dijkstra's shortest path algorithm
///
/// # Arguments
/// * `config` - Pathfinding configuration
/// * `neighbors` - Function to get neighbors for a node
/// * `edge_weight` - Function to get edge weight (default: 1.0 for all edges). Edges with a NaN,
///   infinite or negative weight are skipped.
///
/// # Returns
/// PathResult with the cheapest path within `config.max_depth` hops to any target, or
/// not_found() if no path exists
///
/// # Example
/// ```rust,no_run
/// # use kitedb::api::pathfinding::{dijkstra, PathConfig};
/// # use kitedb::api::traversal::TraversalDirection;
/// # use kitedb::types::{Edge, ETypeId, NodeId};
/// # fn neighbors(
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
///     |node, dir, etype| neighbors(node, dir, etype),
///     |src, etype, dst| 1.0,  // Unweighted
/// );
///
/// if result.found {
///     println!("Path length: {}", result.path.len());
/// }
/// # }
/// ```
pub fn dijkstra<F, W>(config: PathConfig, neighbors: F, edge_weight: W) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  shortest_path_search(&config, &neighbors, &edge_weight, &|_| 0.0)
}

/// Execute A* shortest path algorithm with heuristic
///
/// # Arguments
/// * `config` - Pathfinding configuration
/// * `neighbors` - Function to get neighbors for a node
/// * `edge_weight` - Function to get edge weight. Edges with a NaN, infinite or negative weight
///   are skipped.
/// * `heuristic` - `heuristic(node, target)` estimates the cost from `node` to `target`. It must
///   never overestimate (admissible) for the result to be the cheapest path. With several
///   targets, the search uses the smallest estimate over all of them.
///
/// # Returns
/// PathResult with the shortest path, or not_found() if no path exists
pub fn a_star<F, W, H>(config: PathConfig, neighbors: F, edge_weight: W, heuristic: H) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
  H: Fn(NodeId, NodeId) -> f64,
{
  if config.targets.is_empty() {
    return PathResult::not_found();
  }
  // Admissible for every target if each per-target estimate is.
  let targets: Vec<NodeId> = config.targets.iter().copied().collect();
  let nearest_target_estimate = |node: NodeId| {
    targets
      .iter()
      .map(|&target| heuristic(node, target))
      .fold(f64::INFINITY, f64::min)
  };
  shortest_path_search(&config, &neighbors, &edge_weight, &nearest_target_estimate)
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
  neighbors: F,
  edge_weight: W,
}

impl<F, W> PathFindingBuilder<F, W>
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  /// Create a new pathfinding builder
  pub fn new(source: NodeId, neighbors: F, edge_weight: W) -> Self {
    Self {
      source,
      targets: HashSet::new(),
      allowed_etypes: HashSet::new(),
      direction: TraversalDirection::Out,
      max_depth: 100,
      neighbors,
      edge_weight,
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

    dijkstra(config, self.neighbors, self.edge_weight)
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

    a_star(config, self.neighbors, self.edge_weight, heuristic)
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

    yen_k_shortest(config, k, self.neighbors, self.edge_weight)
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
/// Finds the path with the fewest hops (within `config.max_depth`); `total_weight` is the hop
/// count. This is faster than Dijkstra for unweighted graphs.
pub fn bfs<F>(config: PathConfig, neighbors: F) -> PathResult
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
{
  let source = config.source;
  if config.targets.contains(&source) {
    return PathResult {
      path: vec![source],
      edges: Vec::new(),
      total_weight: 0.0,
      found: true,
    };
  }

  let etype_filter = config.neighbors_etype();
  // Node -> (parent, edge used to reach it).
  let mut parents: FastMap<NodeId, (NodeId, (NodeId, ETypeId, NodeId))> = FastMap::new();
  let mut visited: FastSet<NodeId> = FastSet::new();
  visited.insert(source);
  let mut queue = VecDeque::new();
  queue.push_back((source, 0usize));

  while let Some((node, depth)) = queue.pop_front() {
    if depth >= config.max_depth {
      continue;
    }
    for &dir in expand_directions(config.direction) {
      for edge in neighbors(node, dir, etype_filter) {
        if !config.allows(edge.etype) {
          continue;
        }
        let next = neighbor_id_for_edge(node, dir, &edge);
        if !visited.insert(next) {
          continue;
        }
        parents.insert(next, (node, (edge.src, edge.etype, edge.dst)));
        if config.targets.contains(&next) {
          return bfs_path(&parents, source, next);
        }
        queue.push_back((next, depth + 1));
      }
    }
  }

  PathResult::not_found()
}

fn bfs_path(
  parents: &FastMap<NodeId, (NodeId, (NodeId, ETypeId, NodeId))>,
  source: NodeId,
  target: NodeId,
) -> PathResult {
  let mut path = vec![target];
  let mut edges = Vec::new();
  let mut current = target;
  while current != source {
    let (parent, edge) = parents[&current];
    edges.push(edge);
    path.push(parent);
    current = parent;
  }
  path.reverse();
  edges.reverse();
  let total_weight = edges.len() as f64;

  PathResult {
    path,
    edges,
    total_weight,
    found: true,
  }
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
/// With several targets, the result is the k cheapest paths that end at a target without
/// passing through another one.
///
/// # Arguments
/// * `config` - Pathfinding configuration (source, target, etc.)
/// * `k` - Maximum number of paths to find
/// * `neighbors` - Function to get neighbors for a node
/// * `edge_weight` - Function to get edge weight. Edges with a NaN, infinite or negative weight
///   are skipped.
///
/// # Returns
/// Vector of up to k shortest paths, sorted by total weight
///
/// # Example
/// ```rust,no_run
/// # use kitedb::api::pathfinding::{yen_k_shortest, PathConfig};
/// # use kitedb::api::traversal::TraversalDirection;
/// # use kitedb::types::{Edge, ETypeId, NodeId};
/// # fn neighbors(
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
///     |node, dir, etype| neighbors(node, dir, etype),
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
  neighbors: F,
  edge_weight: W,
) -> Vec<PathResult>
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  if k == 0 {
    return Vec::new();
  }

  let mut targets: Vec<NodeId> = config.targets.iter().copied().collect();
  targets.sort_unstable();
  let [target] = targets[..] else {
    // Rank the k cheapest paths to each target (avoiding the other targets) together.
    let mut paths = Vec::new();
    for &target in &targets {
      let others: FastSet<NodeId> = targets.iter().copied().filter(|&t| t != target).collect();
      let avoid_others = |node: NodeId, dir: TraversalDirection, etype: Option<ETypeId>| {
        neighbors(node, dir, etype)
          .into_iter()
          .filter(|edge| !others.contains(&neighbor_id_for_edge(node, dir, edge)))
          .collect::<Vec<_>>()
      };
      paths.extend(yen_single_target(
        &config,
        target,
        k,
        &avoid_others,
        &edge_weight,
      ));
    }
    paths.sort_by(|a, b| {
      a.total_weight
        .total_cmp(&b.total_weight)
        .then_with(|| a.path.len().cmp(&b.path.len()))
        .then_with(|| a.path.cmp(&b.path))
    });
    paths.truncate(k);
    return paths;
  };
  yen_single_target(&config, target, k, &neighbors, &edge_weight)
}

fn yen_single_target<F, W>(
  config: &PathConfig,
  target: NodeId,
  k: usize,
  neighbors: &F,
  edge_weight: &W,
) -> Vec<PathResult>
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  // Result: the k shortest paths
  let mut result_paths: Vec<PathResult> = Vec::with_capacity(k);

  // Find the first shortest path using Dijkstra
  let first_path = dijkstra(
    build_spur_config(config, config.source, target, 0),
    neighbors,
    edge_weight,
  );
  if !first_path.found {
    return Vec::new();
  }
  result_paths.push(first_path);

  if k == 1 {
    return result_paths;
  }

  // Candidate paths (potential k-shortest paths)
  let mut candidates: Vec<PathResult> = Vec::new();

  // For each path we've found (except we keep finding more)
  for path_idx in 0..k - 1 {
    if path_idx >= result_paths.len() {
      break;
    }

    let prev_path = &result_paths[path_idx];
    let prev_path_nodes = &prev_path.path;

    // For each node in the previous path (except the last), use it as a spur node
    let max_spur_idx = prev_path_nodes.len().saturating_sub(1);
    for (spur_idx, &spur_node) in prev_path_nodes.iter().enumerate().take(max_spur_idx) {
      // Root path: path from source to spur node
      let (root_path, root_edges) = root_segments(prev_path, spur_idx);
      let root_weight = root_weight(&root_edges, edge_weight);

      // Collect edges to exclude (edges used by paths that share this root)
      let mut excluded_edges = HashSet::new();
      extend_excluded_edges(&mut excluded_edges, &result_paths, &root_path, spur_idx);
      extend_excluded_edges(&mut excluded_edges, &candidates, &root_path, spur_idx);

      // Nodes in root path (except spur node) should be avoided
      let root_nodes = root_nodes(&root_path, spur_idx);

      // Create a modified neighbors that excludes forbidden edges and nodes
      let filtered_neighbors = |node: NodeId, dir: TraversalDirection, etype: Option<ETypeId>| {
        neighbors(node, dir, etype)
          .into_iter()
          .filter(|edge| {
            // Don't use excluded edges from spur node
            if node == spur_node && excluded_edges.contains(&(edge.src, edge.etype, edge.dst)) {
              return false;
            }
            // Don't go to nodes in the root path
            !root_nodes.contains(&neighbor_id_for_edge(node, dir, edge))
          })
          .collect()
      };

      // Find spur path from spur_node to target
      let spur_config = build_spur_config(config, spur_node, target, spur_idx);

      let spur_path = dijkstra(spur_config, filtered_neighbors, edge_weight);

      if spur_path.found {
        let candidate = combine_paths(root_path, root_edges, root_weight, spur_path);
        if !is_duplicate_path(&candidate, &result_paths)
          && !is_duplicate_path(&candidate, &candidates)
        {
          candidates.push(candidate);
        }
      }
    }

    // If we have candidates, add the shortest one to results
    match pop_best_candidate(&mut candidates) {
      Some(best) => result_paths.push(best),
      // No more candidates, we've found all possible paths
      None => break,
    }
  }

  result_paths
}

fn root_segments(
  prev_path: &PathResult,
  spur_idx: usize,
) -> (Vec<NodeId>, Vec<(NodeId, ETypeId, NodeId)>) {
  let root_path: Vec<NodeId> = prev_path.path[..=spur_idx].to_vec();
  let root_edges: Vec<(NodeId, ETypeId, NodeId)> = prev_path.edges[..spur_idx].to_vec();
  (root_path, root_edges)
}

fn root_weight<W>(root_edges: &[(NodeId, ETypeId, NodeId)], edge_weight: &W) -> f64
where
  W: Fn(NodeId, ETypeId, NodeId) -> f64,
{
  root_edges
    .iter()
    .map(|(s, e, d)| edge_weight(*s, *e, *d))
    .sum()
}

fn extend_excluded_edges(
  excluded_edges: &mut HashSet<(NodeId, ETypeId, NodeId)>,
  paths: &[PathResult],
  root_path: &[NodeId],
  spur_idx: usize,
) {
  for path in paths {
    if path.path.len() > spur_idx && path.path[..=spur_idx] == root_path[..] {
      if let Some(&edge) = path.edges.get(spur_idx) {
        excluded_edges.insert(edge);
      }
    }
  }
}

fn root_nodes(root_path: &[NodeId], spur_idx: usize) -> HashSet<NodeId> {
  root_path[..spur_idx].iter().copied().collect()
}

/// `config` searching from `spur_node` (reached in `spur_idx` hops) to `target` alone, with the
/// hops it has left.
fn build_spur_config(
  config: &PathConfig,
  spur_node: NodeId,
  target: NodeId,
  spur_idx: usize,
) -> PathConfig {
  let mut targets = HashSet::new();
  targets.insert(target);

  PathConfig {
    source: spur_node,
    targets,
    allowed_etypes: config.allowed_etypes.clone(),
    direction: config.direction,
    max_depth: config.max_depth.saturating_sub(spur_idx),
  }
}

fn combine_paths(
  mut root_path: Vec<NodeId>,
  mut root_edges: Vec<(NodeId, ETypeId, NodeId)>,
  root_weight: f64,
  spur_path: PathResult,
) -> PathResult {
  root_path.extend(spur_path.path.into_iter().skip(1));
  root_edges.extend(spur_path.edges);

  PathResult {
    path: root_path,
    edges: root_edges,
    total_weight: root_weight + spur_path.total_weight,
    found: true,
  }
}

fn is_duplicate_path(candidate: &PathResult, paths: &[PathResult]) -> bool {
  paths.iter().any(|p| p.path == candidate.path)
}

fn pop_best_candidate(candidates: &mut Vec<PathResult>) -> Option<PathResult> {
  let best = candidates
    .iter()
    .enumerate()
    .min_by(|(_, a), (_, b)| a.total_weight.total_cmp(&b.total_weight))
    .map(|(index, _)| index)?;
  Some(candidates.remove(best))
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
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 2).via(1);

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2]);
    assert_eq!(result.total_weight, 1.0);
  }

  #[test]
  fn test_dijkstra_two_hop() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1);

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_dijkstra_weighted() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    // Unweighted: 1->4->5 or 1->2->5 both have 2 hops
    // Weighted: 1->2->5 = 1+1=2, 1->4->5 = 2+1=3
    let result = dijkstra(config, neighbors, weight_fn);

    assert!(result.found);
    // Should prefer 1->2->5 (weight 2) over 1->4->5 (weight 3)
    assert_eq!(result.path, vec![1, 2, 5]);
    assert_eq!(result.total_weight, 2.0);
  }

  #[test]
  fn test_dijkstra_no_path() {
    let neighbors = mock_graph();
    let config = PathConfig::new(3, 1).via(1); // Can't go backwards

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(!result.found);
    assert!(result.path.is_empty());
  }

  #[test]
  fn test_dijkstra_max_depth() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1).max_depth(1);

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(!result.found); // 3 is 2 hops away
  }

  #[test]
  fn test_dijkstra_multiple_targets() {
    let neighbors = mock_graph();
    let config = PathConfig::with_targets(1, vec![3, 4]).via(1);

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(result.found);
    // Should find 4 first (1 hop) not 3 (2 hops)
    assert_eq!(result.path, vec![1, 4]);
  }

  #[test]
  fn test_a_star() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    // Simple heuristic: always returns 0 (degenerates to Dijkstra)
    let result = a_star(config, neighbors, weight_fn, |_, _| 0.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 5]);
  }

  #[test]
  fn test_bfs() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let result = bfs(config, neighbors);

    assert!(result.found);
    // BFS finds shortest path by hops (either 1->2->5 or 1->4->5, both 2 hops)
    assert_eq!(result.path.len(), 3);
    assert_eq!(result.path[0], 1);
    assert_eq!(result.path[2], 5);
  }

  #[test]
  fn test_builder() {
    let neighbors = mock_graph();

    let result = PathFindingBuilder::new(1, neighbors, |_, _, _| 1.0)
      .to(3)
      .via(1)
      .max_depth(10)
      .dijkstra();

    assert!(result.found);
    assert_eq!(result.path, vec![1, 2, 3]);
  }

  #[test]
  fn test_builder_no_target() {
    let neighbors = mock_graph();

    let result = PathFindingBuilder::new(1, neighbors, |_, _, _| 1.0)
      .via(1)
      .dijkstra();

    assert!(!result.found);
  }

  #[test]
  fn test_same_source_target() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 1).via(1);

    let result = dijkstra(config, neighbors, |_, _, _| 1.0);

    assert!(result.found);
    assert_eq!(result.path, vec![1]);
    assert_eq!(result.total_weight, 0.0);
  }

  // ========================================================================
  // Yen's K-Shortest Paths Tests
  // ========================================================================

  #[test]
  fn test_yen_single_path() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 3).via(1);

    let paths = yen_k_shortest(config, 1, neighbors, |_, _, _| 1.0);

    assert_eq!(paths.len(), 1);
    assert!(paths[0].found);
    assert_eq!(paths[0].path, vec![1, 2, 3]);
  }

  #[test]
  fn test_yen_two_paths_to_node_5() {
    // Graph has two paths to node 5:
    // 1->2->5 (weight 2) and 1->4->5 (weight 3)
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 3, neighbors, weight_fn);

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
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 10, neighbors, weight_fn);

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
    let neighbors = mock_graph();
    let config = PathConfig::new(3, 1).via(1); // Can't go backwards

    let paths = yen_k_shortest(config, 3, neighbors, |_, _, _| 1.0);

    assert!(paths.is_empty());
  }

  #[test]
  fn test_yen_k_zero() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 0, neighbors, |_, _, _| 1.0);

    assert!(paths.is_empty());
  }

  #[test]
  fn test_yen_no_duplicate_paths() {
    let neighbors = mock_graph();
    let config = PathConfig::new(1, 5).via(1);

    let paths = yen_k_shortest(config, 10, neighbors, |_, _, _| 1.0);

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
    let neighbors = mock_graph();

    let paths = PathFindingBuilder::new(1, neighbors, weight_fn)
      .to(5)
      .via(1)
      .k_shortest(3);

    assert!(paths.len() >= 2);
    assert_eq!(paths[0].path, vec![1, 2, 5]);
    assert_eq!(paths[1].path, vec![1, 4, 5]);
  }

  #[test]
  fn test_yen_all_paths() {
    let neighbors = mock_graph();

    let paths = PathFindingBuilder::new(1, neighbors, |_, _, _| 1.0)
      .to(5)
      .via(1)
      .all_paths();

    // Should find at least the two known paths
    assert!(paths.len() >= 2);
  }
}
