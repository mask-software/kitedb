//! raydb-b4 `pathfinding` lane: shortest paths that read about what a bidirectional search
//! reads, with the same answers.
//!
//! - Perf: `bfs` (`Kite::shortest_path(..).find_bfs()`, `has_path`) and `dijkstra` (`find()`,
//!   weight 1 by default) searched from the source alone. On a power-law graph that reads
//!   about a hundred times the neighbor lists a bidirectional BFS reads (the compare bench:
//!   40.9 ms against 60 us at 1M nodes / 10M edges). The counting tests compare the neighbor
//!   lists each entry point reads, and the edges the database examines for it, with what a
//!   bidirectional BFS needs for the same answer.
//! - Correctness: every entry point (`bfs`, `dijkstra`, `a_star`, `yen_k_shortest`, and the
//!   `Kite` builder) against brute-force answers on seeded random graphs: directions out, in
//!   and both, edge-type filters, depth limits that cut off cheaper paths, several targets,
//!   source == target, cycles, self-loops, zero and unusable weights; and the `Kite` searches
//!   inside transactions (pending edges, MVCC snapshots).

use super::*;
use crate::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use crate::api::traversal::edges_in_direction;
use crate::core::single_file::{SyncMode, EDGES_EXAMINED};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use tempfile::tempdir;

// ============================================================================
// Graphs
// ============================================================================

const DIRECTIONS: [TraversalDirection; 3] = [
  TraversalDirection::Out,
  TraversalDirection::In,
  TraversalDirection::Both,
];

type EdgeKey = (NodeId, ETypeId, NodeId);

/// An in-memory graph whose neighbor lists are what the database lists: out-edges in
/// `(etype, dst)` order, in-edges in `(etype, src)` order, `Both` the out-edges, then the
/// in-edges that are not self-loops.
struct Graph {
  nodes: usize,
  edges: Vec<EdgeKey>,
  out: HashMap<NodeId, Vec<(ETypeId, NodeId)>>,
  inc: HashMap<NodeId, Vec<(ETypeId, NodeId)>>,
}

impl Graph {
  /// Nodes `1..=nodes`.
  fn new(nodes: usize, mut edges: Vec<EdgeKey>) -> Self {
    edges.sort_unstable();
    edges.dedup();
    let mut out: HashMap<NodeId, Vec<(ETypeId, NodeId)>> = HashMap::new();
    let mut inc: HashMap<NodeId, Vec<(ETypeId, NodeId)>> = HashMap::new();
    for &(src, etype, dst) in &edges {
      out.entry(src).or_default().push((etype, dst));
      inc.entry(dst).or_default().push((etype, src));
    }
    for list in out.values_mut().chain(inc.values_mut()) {
      list.sort_unstable();
    }
    Self {
      nodes,
      edges,
      out,
      inc,
    }
  }

  fn typed(
    lists: &HashMap<NodeId, Vec<(ETypeId, NodeId)>>,
    node: NodeId,
    etype: Option<ETypeId>,
  ) -> Vec<(ETypeId, NodeId)> {
    lists
      .get(&node)
      .into_iter()
      .flatten()
      .copied()
      .filter(|&(e, _)| etype.is_none_or(|etype| etype == e))
      .collect()
  }

  fn neighbors(&self, node: NodeId, dir: TraversalDirection, etype: Option<ETypeId>) -> Vec<Edge> {
    edges_in_direction(
      node,
      dir,
      || Self::typed(&self.out, node, etype),
      || Self::typed(&self.inc, node, etype),
    )
  }

  fn has_edge(&self, edge: EdgeKey) -> bool {
    self.edges.binary_search(&edge).is_ok()
  }
}

/// Endpoints drawn half the time uniformly and half the time from a Zipf-like law (weight
/// `1 / rank^0.8`, ranks shuffled), so a few hubs hold many edges: what the compare bench's
/// generator makes, at a smaller scale.
fn power_law_graph(seed: u64, nodes: usize, edges: usize) -> Graph {
  let mut rng = StdRng::seed_from_u64(seed);
  let law = |rng: &mut StdRng| -> Vec<f64> {
    let mut ranks: Vec<usize> = (1..=nodes).collect();
    ranks.shuffle(rng);
    let mut total = 0.0;
    ranks
      .iter()
      .map(|&rank| {
        total += 1.0 / (rank as f64).powf(0.8);
        total
      })
      .collect()
  };
  let src_law = law(&mut rng);
  let dst_law = law(&mut rng);
  let pick = |law: &[f64], rng: &mut StdRng| -> NodeId {
    if rng.gen_bool(0.5) {
      rng.gen_range(1..=nodes as NodeId)
    } else {
      let x = rng.gen::<f64>() * law[law.len() - 1];
      (law.partition_point(|&c| c < x).min(nodes - 1) + 1) as NodeId
    }
  };
  let list = (0..edges)
    .map(|_| {
      let src = pick(&src_law, &mut rng);
      let dst = pick(&dst_law, &mut rng);
      (src, rng.gen_range(1..=2), dst)
    })
    .collect();
  Graph::new(nodes, list)
}

/// A small random graph with cycles, self-loops and both edge types between some pairs.
fn small_graph(rng: &mut StdRng) -> Graph {
  let nodes = rng.gen_range(2..=24);
  let edges = rng.gen_range(0..=nodes * 3);
  let list = (0..edges)
    .map(|_| {
      (
        rng.gen_range(1..=nodes as NodeId),
        rng.gen_range(1..=3),
        rng.gen_range(1..=nodes as NodeId),
      )
    })
    .collect();
  Graph::new(nodes, list)
}

/// A random weight for each edge, from a fixed table: multiples of 0.5 (sums are exact), zero,
/// and some that pathfinding must skip (NaN, infinite, negative).
fn weights_for(graph: &Graph, rng: &mut StdRng) -> HashMap<EdgeKey, f64> {
  const TABLE: [f64; 10] = [0.0, 0.5, 1.0, 1.0, 1.5, 2.0, 3.0, 5.0, 8.0, 13.0];
  graph
    .edges
    .iter()
    .map(|&edge| {
      let weight = match rng.gen_range(0..40) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => -1.0,
        _ => TABLE[rng.gen_range(0..TABLE.len())],
      };
      (edge, weight)
    })
    .collect()
}

fn random_config(graph: &Graph, rng: &mut StdRng) -> PathConfig {
  let node = |rng: &mut StdRng| rng.gen_range(1..=graph.nodes as NodeId);
  let source = node(rng);
  let mut config = match rng.gen_range(0..6) {
    0 => PathConfig::new(source, source),
    1 => PathConfig::with_targets(source, (0..rng.gen_range(0..=3)).map(|_| node(rng))),
    _ => PathConfig::new(source, node(rng)),
  };
  config = config.direction(DIRECTIONS[rng.gen_range(0..3)]);
  config = config.max_depth(match rng.gen_range(0..8) {
    0 => 0,
    1 => 100,
    _ => rng.gen_range(1..=6),
  });
  for _ in 0..rng.gen_range(0..=2) {
    config = config.via(rng.gen_range(1..=3));
  }
  config
}

// ============================================================================
// Oracles
// ============================================================================

/// The edges a hop from `node` may take under `config`, as `(edge, next node)`.
fn hops(graph: &Graph, config: &PathConfig, node: NodeId) -> Vec<(EdgeKey, NodeId)> {
  graph
    .neighbors(node, config.direction, None)
    .into_iter()
    .filter(|edge| config.allows(edge.etype))
    .map(|edge| {
      let next = neighbor_id_for_edge(node, config.direction, &edge);
      ((edge.src, edge.etype, edge.dst), next)
    })
    .collect()
}

/// The fewest hops from the source to a target within `max_depth`, by a plain BFS.
fn oracle_hops(graph: &Graph, config: &PathConfig) -> Option<usize> {
  if config.targets.contains(&config.source) {
    return Some(0);
  }
  let mut seen = HashSet::from([config.source]);
  let mut level = vec![config.source];
  for depth in 1..=config.max_depth.min(graph.nodes) {
    let mut next_level = Vec::new();
    for &node in &level {
      for (_, next) in hops(graph, config, node) {
        if config.targets.contains(&next) {
          return Some(depth);
        }
        if seen.insert(next) {
          next_level.push(next);
        }
      }
    }
    level = next_level;
  }
  None
}

/// The cheapest cost from the source to a target within `max_depth` hops (unusable weights
/// skipped), by dynamic programming over the hop count.
fn oracle_cost(
  graph: &Graph,
  config: &PathConfig,
  weight: &dyn Fn(NodeId, ETypeId, NodeId) -> f64,
) -> Option<f64> {
  let mut best: HashMap<NodeId, f64> = HashMap::from([(config.source, 0.0)]);
  let mut answer = config.targets.contains(&config.source).then_some(0.0f64);
  for _ in 0..config.max_depth.min(graph.nodes) {
    let mut next = best.clone();
    for (&node, &cost) in &best {
      for ((src, etype, dst), to) in hops(graph, config, node) {
        let w = weight(src, etype, dst);
        if !usable_weight(w) {
          continue;
        }
        let entry = next.entry(to).or_insert(f64::INFINITY);
        *entry = entry.min(cost + w);
      }
    }
    best = next;
  }
  for target in &config.targets {
    if let Some(&cost) = best.get(target) {
      answer = Some(answer.map_or(cost, |a| a.min(cost)));
    }
  }
  answer
}

/// Every simple path from the source that ends at a target and passes through no other target,
/// within `max_depth` hops: `(cost, nodes)`.
fn all_simple_paths(
  graph: &Graph,
  config: &PathConfig,
  weight: &dyn Fn(NodeId, ETypeId, NodeId) -> f64,
) -> Vec<(f64, Vec<NodeId>)> {
  fn walk(
    graph: &Graph,
    config: &PathConfig,
    weight: &dyn Fn(NodeId, ETypeId, NodeId) -> f64,
    path: &mut Vec<NodeId>,
    cost: f64,
    found: &mut Vec<(f64, Vec<NodeId>)>,
  ) {
    let node = *path.last().unwrap();
    if config.targets.contains(&node) {
      found.push((cost, path.clone()));
      if path.len() > 1 {
        return;
      }
    }
    if path.len() > config.max_depth {
      return;
    }
    for ((src, etype, dst), next) in hops(graph, config, node) {
      let w = weight(src, etype, dst);
      if !usable_weight(w) || path.contains(&next) {
        continue;
      }
      path.push(next);
      walk(graph, config, weight, path, cost + w, found);
      path.pop();
    }
  }
  let mut found = Vec::new();
  walk(
    graph,
    config,
    weight,
    &mut vec![config.source],
    0.0,
    &mut found,
  );
  found
}

/// Checks that `result` is a path the search may return under `config`: from the source to a
/// target, each edge in the graph, of an allowed type, taken in the search direction, no node
/// twice, within `max_depth`. Returns its weight, summed edge by edge from the source.
fn check_path(
  graph: &Graph,
  config: &PathConfig,
  result: &PathResult,
  weight: &dyn Fn(NodeId, ETypeId, NodeId) -> f64,
  what: &str,
) -> f64 {
  assert!(result.found, "{what}: not found");
  assert_eq!(
    result.path.first(),
    Some(&config.source),
    "{what}: {result:?}"
  );
  assert!(
    config.targets.contains(result.path.last().unwrap()),
    "{what}: ends off target: {result:?}"
  );
  assert_eq!(
    result.path.len(),
    result.edges.len() + 1,
    "{what}: {result:?}"
  );
  assert!(
    result.edges.len() <= config.max_depth,
    "{what}: too deep: {result:?}"
  );
  let distinct: HashSet<NodeId> = result.path.iter().copied().collect();
  assert_eq!(
    distinct.len(),
    result.path.len(),
    "{what}: repeats a node: {result:?}"
  );
  let mut total = 0.0;
  for (i, &(src, etype, dst)) in result.edges.iter().enumerate() {
    let (from, to) = (result.path[i], result.path[i + 1]);
    let forward = src == from && dst == to;
    let backward = src == to && dst == from;
    let taken = match config.direction {
      TraversalDirection::Out => forward,
      TraversalDirection::In => backward,
      TraversalDirection::Both => forward || backward,
    };
    assert!(
      taken,
      "{what}: edge {i} is not a hop {from}->{to}: {result:?}"
    );
    assert!(
      graph.has_edge((src, etype, dst)),
      "{what}: no edge {i}: {result:?}"
    );
    assert!(
      config.allows(etype),
      "{what}: edge type {etype} not allowed: {result:?}"
    );
    total += weight(src, etype, dst);
  }
  total
}

fn unit(_: NodeId, _: ETypeId, _: NodeId) -> f64 {
  1.0
}

// ============================================================================
// Correctness: every entry point against brute force
// ============================================================================

#[test]
fn pathfinding_bfs_finds_the_fewest_hops_on_random_graphs() {
  let mut rng = StdRng::seed_from_u64(0xB4_0001);
  for round in 0..3000 {
    let graph = small_graph(&mut rng);
    let config = random_config(&graph, &mut rng);
    let what = format!("round {round}: {config:?}");
    let expected = oracle_hops(&graph, &config);
    let result = bfs(config.clone(), |node, dir, etype| {
      graph.neighbors(node, dir, etype)
    });
    match expected {
      None => assert!(!result.found, "{what}: found {result:?}"),
      Some(hops) => {
        check_path(&graph, &config, &result, &unit, &what);
        assert_eq!(result.edges.len(), hops, "{what}: {result:?}");
        assert_eq!(result.total_weight, hops as f64, "{what}: {result:?}");
      }
    }
  }
}

#[test]
fn pathfinding_dijkstra_and_a_star_find_the_cheapest_path_on_random_graphs() {
  let mut rng = StdRng::seed_from_u64(0xB4_0002);
  for round in 0..3000 {
    let graph = small_graph(&mut rng);
    let mut config = random_config(&graph, &mut rng);
    if rng.gen_bool(0.3) {
      // Depth limits that bind: a cheaper path is often a deeper one.
      config.max_depth = rng.gen_range(1..=3);
    }
    let weights = weights_for(&graph, &mut rng);
    let unit_weights = rng.gen_bool(0.3);
    let weight = |src: NodeId, etype: ETypeId, dst: NodeId| -> f64 {
      if unit_weights {
        1.0
      } else {
        weights[&(src, etype, dst)]
      }
    };
    let what = format!("round {round}: {config:?}, unit weights {unit_weights}");
    let expected = oracle_cost(&graph, &config, &weight);
    let neighbors = |node, dir, etype| graph.neighbors(node, dir, etype);

    let result = dijkstra(config.clone(), neighbors, weight);
    // The cheapest usable edge bounds the cost of any hop: admissible for A*.
    let cheapest = weights
      .values()
      .copied()
      .filter(|&w| usable_weight(w))
      .fold(if unit_weights { 1.0 } else { f64::INFINITY }, f64::min);
    let cheapest = if cheapest.is_finite() { cheapest } else { 0.0 };
    let estimate = move |node: NodeId, target: NodeId| if node == target { 0.0 } else { cheapest };
    let guided = a_star(config.clone(), neighbors, weight, estimate);
    let blind = a_star(config.clone(), neighbors, weight, |_, _| 0.0);
    let k_first = yen_k_shortest(config.clone(), 1, neighbors, weight);

    for (name, result) in [
      ("dijkstra", &result),
      ("a_star", &guided),
      ("a_star(0)", &blind),
    ]
    .into_iter()
    .chain(k_first.first().map(|r| ("yen k=1", r)))
    {
      let what = format!("{what}, {name}");
      match expected {
        None => assert!(!result.found, "{what}: found {result:?}"),
        Some(cost) => {
          let summed = check_path(&graph, &config, result, &weight, &what);
          assert_eq!(result.total_weight, cost, "{what}: {result:?}");
          assert_eq!(summed, cost, "{what}: weights along the path: {result:?}");
        }
      }
    }
    assert_eq!(
      k_first.len(),
      usize::from(expected.is_some()),
      "{what}: yen k=1: {k_first:?}"
    );
  }
}

#[test]
fn pathfinding_yen_finds_the_k_cheapest_simple_paths_on_random_graphs() {
  let mut rng = StdRng::seed_from_u64(0xB4_0003);
  for round in 0..1500 {
    // At most one edge per pair of nodes: Yen tells paths apart by their nodes.
    let nodes = rng.gen_range(2..=9);
    let mut pairs = HashSet::new();
    let edges: Vec<EdgeKey> = (0..rng.gen_range(0..=nodes * 3))
      .filter_map(|_| {
        let (src, dst) = (
          rng.gen_range(1..=nodes as NodeId),
          rng.gen_range(1..=nodes as NodeId),
        );
        (src != dst && pairs.insert((src.min(dst), src.max(dst))))
          .then(|| (src, rng.gen_range(1..=2), dst))
      })
      .collect();
    let graph = Graph::new(nodes, edges);
    let mut config = random_config(&graph, &mut rng);
    config.max_depth = config.max_depth.min(5);
    let weights = weights_for(&graph, &mut rng);
    let weight = |src: NodeId, etype: ETypeId, dst: NodeId| weights[&(src, etype, dst)];
    let k = rng.gen_range(1..=6);
    let what = format!("round {round}: k {k}, {config:?}");

    let paths = yen_k_shortest(
      config.clone(),
      k,
      |node, dir, etype| graph.neighbors(node, dir, etype),
      weight,
    );
    let mut expected: Vec<f64> = all_simple_paths(&graph, &config, &weight)
      .into_iter()
      .map(|(cost, _)| cost)
      .collect();
    expected.sort_by(f64::total_cmp);
    expected.truncate(k);
    let costs: Vec<f64> = paths.iter().map(|p| p.total_weight).collect();
    assert_eq!(costs, expected, "{what}: {paths:?}");
    let mut seen = HashSet::new();
    for path in &paths {
      let summed = check_path(&graph, &config, path, &weight, &what);
      assert_eq!(summed, path.total_weight, "{what}: {path:?}");
      assert!(
        seen.insert(path.path.clone()),
        "{what}: path twice: {paths:?}"
      );
      let inner = &path.path[1..path.path.len().max(2) - 1];
      assert!(
        inner.iter().all(|node| !config.targets.contains(node)),
        "{what}: passes through another target: {path:?}"
      );
    }
  }
}

/// Yen's spur searches exclude the next edge of the paths that share the root, and the root's
/// nodes: also when the spur search reaches the spur node from the target's side.
#[test]
fn pathfinding_yen_spur_exclusions_hold_from_both_ends() {
  // 1 -> 2 -> 4 (cost 2), 1 -> 3 -> 4 (cost 4), 1 -> 4 (cost 10).
  let graph = Graph::new(
    4,
    vec![(1, 1, 2), (2, 1, 4), (1, 1, 3), (3, 1, 4), (1, 1, 4)],
  );
  let weight = |src: NodeId, _: ETypeId, dst: NodeId| match (src, dst) {
    (1, 2) | (2, 4) => 1.0,
    (1, 3) | (3, 4) => 2.0,
    _ => 10.0,
  };
  // A search that also expands from 4 reaches 1 through the excluded edge 1->2 from 2's side,
  // and through the root node 1 from 3's side: the spur paths must still avoid them.
  for direction in [TraversalDirection::Out, TraversalDirection::Both] {
    let paths = yen_k_shortest(
      PathConfig::new(1, 4).direction(direction),
      5,
      |node, dir, etype| graph.neighbors(node, dir, etype),
      weight,
    );
    let found: Vec<(Vec<NodeId>, f64)> = paths
      .iter()
      .map(|p| (p.path.clone(), p.total_weight))
      .collect();
    let expected = vec![
      (vec![1, 2, 4], 2.0),
      (vec![1, 3, 4], 4.0),
      (vec![1, 4], 10.0),
    ];
    assert_eq!(found, expected, "{direction:?}: {paths:?}");
  }
  // Reversed: from 4 against the edges.
  let paths = yen_k_shortest(
    PathConfig::new(4, 1).direction(TraversalDirection::In),
    5,
    |node, dir, etype| graph.neighbors(node, dir, etype),
    weight,
  );
  let found: Vec<Vec<NodeId>> = paths.iter().map(|p| p.path.clone()).collect();
  assert_eq!(found, vec![vec![4, 2, 1], vec![4, 3, 1], vec![4, 1]]);
}

// ============================================================================
// Perf: reads against a bidirectional BFS
// ============================================================================

/// The fewest hops from `a` to `b` within `max_hops`, by a level-at-a-time bidirectional BFS
/// that expands the smaller frontier (what the compare bench's app-level search and Neo4j's
/// shortestPath do). `out(v)` / `inc(v)` list `v`'s out- / in-neighbors.
fn reference_bidirectional_bfs(
  a: NodeId,
  b: NodeId,
  max_hops: usize,
  out: impl Fn(NodeId) -> Vec<NodeId>,
  inc: impl Fn(NodeId) -> Vec<NodeId>,
) -> Option<usize> {
  if a == b {
    return Some(0);
  }
  let mut forward: HashMap<NodeId, usize> = HashMap::from([(a, 0)]);
  let mut backward: HashMap<NodeId, usize> = HashMap::from([(b, 0)]);
  let (mut forward_level, mut backward_level) = (vec![a], vec![b]);
  let (mut forward_depth, mut backward_depth) = (0, 0);
  while forward_depth + backward_depth < max_hops
    && !forward_level.is_empty()
    && !backward_level.is_empty()
  {
    let expand_forward = forward_level.len() <= backward_level.len();
    let (level, seen, other, depth, list): (_, _, _, _, &dyn Fn(NodeId) -> Vec<NodeId>) =
      if expand_forward {
        (
          &mut forward_level,
          &mut forward,
          &backward,
          &mut forward_depth,
          &out,
        )
      } else {
        (
          &mut backward_level,
          &mut backward,
          &forward,
          &mut backward_depth,
          &inc,
        )
      };
    let mut best = None::<usize>;
    let mut next_level = Vec::new();
    for &node in level.iter() {
      for next in list(node) {
        if seen.contains_key(&next) {
          continue;
        }
        seen.insert(next, *depth + 1);
        if let Some(&rest) = other.get(&next) {
          let hops = *depth + 1 + rest;
          best = Some(best.map_or(hops, |best| best.min(hops)));
        }
        next_level.push(next);
      }
    }
    *depth += 1;
    *level = next_level;
    if best.is_some() {
      return best;
    }
  }
  None
}

/// Query pairs over nodes `1..=nodes`: random pairs plus pairs a few hops apart.
fn query_pairs(graph: &Graph, seed: u64, count: usize) -> Vec<(NodeId, NodeId)> {
  let mut rng = StdRng::seed_from_u64(seed);
  (0..count)
    .map(|i| {
      let a = rng.gen_range(1..=graph.nodes as NodeId);
      if i % 2 == 0 {
        return (a, rng.gen_range(1..=graph.nodes as NodeId));
      }
      // A random walk of up to 5 out-hops, so most of these pairs are connected.
      let mut b = a;
      for _ in 0..rng.gen_range(1..=5) {
        match graph.out.get(&b) {
          Some(list) => b = list[rng.gen_range(0..list.len())].1,
          None => break,
        }
      }
      (a, b)
    })
    .collect()
}

#[derive(Default)]
struct Reads {
  lists: Cell<usize>,
}

impl Reads {
  fn take(&self) -> usize {
    self.lists.replace(0)
  }
}

/// `bfs` and `dijkstra` (unit weights) read about the neighbor lists a bidirectional BFS reads,
/// not the whole neighborhood of the source.
#[test]
fn pathfinding_searches_read_about_what_a_bidirectional_bfs_reads() {
  let graph = power_law_graph(0xB4_0004, 20_000, 200_000);
  let pairs = query_pairs(&graph, 0xB4_0005, 120);
  let reads = Reads::default();
  let neighbors = |node: NodeId, dir: TraversalDirection, etype: Option<ETypeId>| {
    reads.lists.set(reads.lists.get() + 1);
    graph.neighbors(node, dir, etype)
  };
  let list = |dir: TraversalDirection| {
    let reads = &reads;
    let graph = &graph;
    move |node: NodeId| {
      reads.lists.set(reads.lists.get() + 1);
      graph
        .neighbors(node, dir, None)
        .into_iter()
        .map(|edge| neighbor_id_for_edge(node, dir, &edge))
        .collect::<Vec<_>>()
    }
  };

  let (mut reference, mut by_bfs, mut by_dijkstra) = (0, 0, 0);
  let mut found = 0;
  for &(a, b) in &pairs {
    let expected = reference_bidirectional_bfs(
      a,
      b,
      6,
      list(TraversalDirection::Out),
      list(TraversalDirection::In),
    );
    reference += reads.take();
    found += usize::from(expected.is_some());

    let result = bfs(PathConfig::new(a, b).max_depth(6), neighbors);
    by_bfs += reads.take();
    assert_eq!(
      result.found.then_some(result.edges.len()),
      expected,
      "bfs {a}->{b}"
    );

    let result = dijkstra(PathConfig::new(a, b).max_depth(6), neighbors, unit);
    by_dijkstra += reads.take();
    assert_eq!(
      result.found.then_some(result.edges.len()),
      expected,
      "dijkstra {a}->{b}"
    );
  }
  assert!(found > pairs.len() / 2, "only {found} pairs connected");
  let queries = pairs.len();
  let over = [("bfs", by_bfs, 2), ("dijkstra", by_dijkstra, 2)]
    .into_iter()
    .filter(|&(_, read, factor)| read > factor * reference + queries)
    .map(|(name, read, factor)| format!("{name} read {read} neighbor lists (bound {factor}x)"))
    .collect::<Vec<_>>();
  assert!(
    over.is_empty(),
    "for {queries} queries a bidirectional BFS reads {reference} neighbor lists; {over:?}"
  );
}

fn reset_edges_examined() {
  EDGES_EXAMINED.with(|count| count.set(0));
}

fn edges_examined() -> usize {
  EDGES_EXAMINED.with(|count| count.get())
}

/// `graph` in a fresh `Kite` (edge types 1 and 2 as `A` and `B`), checkpointed: the database
/// ids of nodes `1..=nodes` (index 0 is unused) and of the two edge types.
fn kite_with(path: &Path, graph: &Graph) -> (Kite, Vec<NodeId>, [ETypeId; 2]) {
  let kite = Kite::open(
    path,
    KiteOptions::new()
      .node(NodeDef::new("N", "n:"))
      .edge(EdgeDef::new("A"))
      .edge(EdgeDef::new("B"))
      .sync_mode(SyncMode::Off),
  )
  .expect("open");
  let db = kite.raw();
  let etypes = [db.etype_id("A").expect("A"), db.etype_id("B").expect("B")];
  db.begin_bulk().expect("begin bulk");
  let keys: Vec<String> = (1..=graph.nodes).map(|i| format!("n:{i}")).collect();
  let keys: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  let mut ids = vec![0];
  ids.extend(db.create_nodes_batch(&keys).expect("nodes"));
  let edges: Vec<EdgeKey> = graph
    .edges
    .iter()
    .map(|&(src, etype, dst)| {
      (
        ids[src as usize],
        etypes[etype as usize - 1],
        ids[dst as usize],
      )
    })
    .collect();
  db.add_edges_batch(&edges).expect("edges");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  (kite, ids, etypes)
}

/// The `Kite` builder's searches examine about the edges a bidirectional BFS over
/// `out_edges` / `in_edges` examines for the same answers.
#[test]
fn pathfinding_kite_searches_examine_about_what_a_bidirectional_bfs_examines() {
  let graph = power_law_graph(0xB4_0006, 4_000, 40_000);
  let dir = tempdir().expect("temp dir");
  let (kite, ids, _) = kite_with(&dir.path().join("db.kitedb"), &graph);
  let db = kite.raw();
  let pairs: Vec<(NodeId, NodeId)> = query_pairs(&graph, 0xB4_0007, 80)
    .into_iter()
    .map(|(a, b)| (ids[a as usize], ids[b as usize]))
    .collect();
  let out =
    |node: NodeId| -> Vec<NodeId> { db.out_edges(node).into_iter().map(|(_, dst)| dst).collect() };
  let inc =
    |node: NodeId| -> Vec<NodeId> { db.in_edges(node).into_iter().map(|(_, src)| src).collect() };
  let hops = |result: PathResult| result.found.then_some(result.edges.len());

  let (mut reference, mut by_bfs, mut by_find, mut reference_100, mut by_has_path) =
    (0, 0, 0, 0, 0);
  for &(a, b) in &pairs {
    reset_edges_examined();
    let expected = reference_bidirectional_bfs(a, b, 6, out, inc);
    reference += edges_examined();

    reset_edges_examined();
    let result = kite.shortest_path(a, b).max_depth(6).find_bfs();
    by_bfs += edges_examined();
    assert_eq!(hops(result), expected, "find_bfs {a}->{b}");

    reset_edges_examined();
    let result = kite.shortest_path(a, b).max_depth(6).find();
    by_find += edges_examined();
    assert_eq!(hops(result), expected, "find {a}->{b}");

    // `has_path` searches up to 100 hops.
    reset_edges_examined();
    let expected = reference_bidirectional_bfs(a, b, 100, out, inc);
    reference_100 += edges_examined();
    reset_edges_examined();
    assert_eq!(
      kite.has_path(a, b, None).expect("has_path"),
      expected.is_some(),
      "has_path {a}->{b}"
    );
    by_has_path += edges_examined();
  }
  let slack = pairs.len() * 16;
  let over = [
    ("find_bfs", by_bfs, 2, reference),
    ("find", by_find, 2, reference),
    ("has_path", by_has_path, 2, reference_100),
  ]
  .into_iter()
  .filter(|&(_, examined, factor, reference)| examined > factor * reference + slack)
  .map(|(name, examined, factor, reference)| {
    format!("{name} examined {examined} edges, a bidirectional BFS {reference} (bound {factor}x)")
  })
  .collect::<Vec<_>>();
  assert!(over.is_empty(), "{over:#?}");
  kite.close().expect("close");
}

// ============================================================================
// Kite: semantics
// ============================================================================

#[test]
fn pathfinding_kite_shortest_path_semantics() {
  // 1 -A-> 2 -A-> 3 -B-> 4,  1 -B-> 5 -B-> 4,  3 -A-> 1 (a cycle),  2 -A-> 2 (a self-loop),
  // 6 alone.
  let graph = Graph::new(
    6,
    vec![
      (1, 1, 2),
      (2, 1, 3),
      (3, 2, 4),
      (1, 2, 5),
      (5, 2, 4),
      (3, 1, 1),
      (2, 1, 2),
    ],
  );
  let dir = tempdir().expect("temp dir");
  let (kite, n, [a, b]) = kite_with(&dir.path().join("db.kitedb"), &graph);
  let path_of = |r: &PathResult| r.found.then(|| r.path.clone());

  // Any type, directed: the B detour is shorter.
  let r = kite.shortest_path(n[1], n[4]).find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[1], n[5], n[4]]));
  assert_eq!(r.edges, vec![(n[1], b, n[5]), (n[5], b, n[4])]);
  assert_eq!(r.total_weight, 2.0);
  let r = kite.shortest_path(n[1], n[4]).find();
  assert_eq!(path_of(&r), Some(vec![n[1], n[5], n[4]]));
  assert_eq!(r.total_weight, 2.0);

  // Type filters.
  let via_a = || kite.shortest_path(n[1], n[4]).via("A").expect("A");
  assert!(!via_a().find_bfs().found, "4 has no A in-edge");
  assert!(!via_a().find().found);
  assert!(via_a().find_k_shortest(3).is_empty());
  let r = kite
    .shortest_path(n[1], n[3])
    .via("A")
    .expect("A")
    .find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[1], n[2], n[3]]));
  assert_eq!(r.edges, vec![(n[1], a, n[2]), (n[2], a, n[3])]);
  let r = kite
    .shortest_path(n[1], n[4])
    .via("A")
    .expect("A")
    .via("B")
    .expect("B")
    .find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[1], n[5], n[4]]));

  // Depth limits.
  assert!(!kite.shortest_path(n[1], n[4]).max_depth(1).find_bfs().found);
  assert!(!kite.shortest_path(n[1], n[4]).max_depth(1).find().found);
  assert!(kite.shortest_path(n[1], n[4]).max_depth(2).find_bfs().found);
  assert!(kite.shortest_path(n[1], n[4]).max_depth(2).find().found);
  let via_a_to_3 = |depth| {
    kite
      .shortest_path(n[1], n[3])
      .via("A")
      .expect("A")
      .max_depth(depth)
  };
  assert!(!via_a_to_3(1).find_bfs().found);
  assert!(via_a_to_3(2).find_bfs().found);

  // Source == target, at any depth.
  for depth in [0, 1, 6] {
    let r = kite.shortest_path(n[3], n[3]).max_depth(depth).find_bfs();
    assert_eq!(path_of(&r), Some(vec![n[3]]));
    assert!(r.edges.is_empty());
    assert_eq!(r.total_weight, 0.0);
    let r = kite.shortest_path(n[3], n[3]).max_depth(depth).find();
    assert_eq!(path_of(&r), Some(vec![n[3]]));
    assert_eq!(r.total_weight, 0.0);
  }

  // No path: 4 has no out-edges, 6 no edges at all.
  assert!(!kite.shortest_path(n[4], n[1]).find_bfs().found);
  assert!(!kite.shortest_path(n[4], n[1]).find().found);
  assert!(!kite.has_path(n[4], n[1], None).expect("has_path"));
  for direction in DIRECTIONS {
    assert!(
      !kite
        .shortest_path(n[1], n[6])
        .direction(direction)
        .find_bfs()
        .found
    );
    assert!(
      !kite
        .shortest_path(n[6], n[1])
        .direction(direction)
        .find()
        .found
    );
  }

  // Against the edges: In walks them backwards, Both either way.
  let r = kite
    .shortest_path(n[4], n[1])
    .direction(TraversalDirection::In)
    .find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[4], n[5], n[1]]));
  assert_eq!(r.edges, vec![(n[5], b, n[4]), (n[1], b, n[5])]);
  let r = kite.shortest_path(n[4], n[2]).bidirectional().find_bfs();
  assert_eq!(r.edges.len(), 2, "{r:?}");
  let r = kite
    .shortest_path(n[4], n[1])
    .direction(TraversalDirection::In)
    .find();
  assert_eq!(path_of(&r), Some(vec![n[4], n[5], n[1]]));

  // Cycles: 3 reaches 2 through 1.
  let r = kite.shortest_path(n[3], n[2]).find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[3], n[1], n[2]]));
  assert!(kite.has_path(n[3], n[2], Some("A")).expect("has_path"));
  assert!(!kite.has_path(n[3], n[2], Some("B")).expect("has_path"));

  // Several targets: the nearest.
  let r = kite
    .shortest_path_to_any(n[1], vec![n[3], n[4], n[6]])
    .via("A")
    .expect("A")
    .find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[1], n[2], n[3]]));
  let r = kite.shortest_path_to_any(n[1], vec![n[2], n[4]]).find_bfs();
  assert_eq!(path_of(&r), Some(vec![n[1], n[2]]));

  // k shortest: 1-5-4, then 1-2-3-4.
  let paths = kite.shortest_path(n[1], n[4]).find_k_shortest(3);
  let found: Vec<Vec<NodeId>> = paths.iter().map(|p| p.path.clone()).collect();
  assert_eq!(
    found,
    vec![vec![n[1], n[5], n[4]], vec![n[1], n[2], n[3], n[4]]]
  );
  kite.close().expect("close");
}

/// The searches see their transaction: its pending edges and deletes on its thread, nothing
/// of it on another; and an MVCC read transaction keeps its snapshot.
#[test]
fn pathfinding_kite_searches_see_their_transaction() {
  // 1 -A-> 2 -A-> 3.
  let graph = Graph::new(3, vec![(1, 1, 2), (2, 1, 3)]);
  let dir = tempdir().expect("temp dir");
  let (kite, n, [a, _]) = kite_with(&dir.path().join("db.kitedb"), &graph);
  let db = kite.raw();
  let hops = |r: PathResult| r.found.then_some(r.edges.len());
  let searches = |kite: &Kite| {
    [
      hops(kite.shortest_path(n[1], n[3]).find_bfs()),
      hops(kite.shortest_path(n[1], n[3]).find()),
      kite
        .shortest_path(n[1], n[3])
        .find_k_shortest(1)
        .pop()
        .and_then(hops),
      hops(
        kite
          .shortest_path(n[3], n[1])
          .direction(TraversalDirection::In)
          .find_bfs(),
      ),
    ]
  };
  assert_eq!(searches(&kite), [Some(2); 4]);

  db.begin(false).expect("begin");
  db.add_edge(n[1], a, n[3]).expect("add");
  assert_eq!(searches(&kite), [Some(1); 4], "the pending edge 1->3");
  std::thread::scope(|scope| {
    let other = scope.spawn(|| searches(&kite)).join().expect("thread");
    assert_eq!(other, [Some(2); 4], "another thread saw the pending edge");
  });
  db.delete_edge(n[1], a, n[3]).expect("delete");
  db.delete_edge(n[2], a, n[3]).expect("delete");
  assert_eq!(searches(&kite), [None; 4], "the pending delete of 2->3");
  db.rollback().expect("rollback");
  assert_eq!(searches(&kite), [Some(2); 4]);

  // A read transaction that began before a commit keeps its snapshot.
  let (began_tx, began) = mpsc::channel();
  let (committed_tx, committed) = mpsc::channel::<()>();
  std::thread::scope(|scope| {
    let (searches, kite) = (&searches, &kite);
    let reader = scope.spawn(move || {
      db.begin(true).expect("begin read");
      began_tx.send(()).expect("send");
      committed.recv().expect("recv");
      let seen = searches(kite);
      db.rollback().expect("end read");
      seen
    });
    began.recv().expect("recv");
    db.begin(false).expect("begin");
    db.add_edge(n[1], a, n[3]).expect("add");
    db.commit().expect("commit");
    committed_tx.send(()).expect("send");
    assert_eq!(
      reader.join().expect("reader"),
      [Some(2); 4],
      "a snapshot reader saw a later commit"
    );
  });
  assert_eq!(searches(&kite), [Some(1); 4]);
  kite.close().expect("close");
}

/// Two halves of a meeting that share a node (a zero-weight cycle): the loop between its
/// occurrences is cut out of the path.
#[test]
fn pathfinding_meeting_path_cuts_out_a_loop() {
  let label = |node, parent, edge, weight| Label {
    node,
    cost: 0.0,
    depth: 0,
    parent,
    edge,
    weight,
    alive: true,
  };
  // Forward: 1 -> 2 -> 3. Backward: 4 -> 2 -> 5 (5 is the target).
  let forward = [
    label(1, None, None, 0.0),
    label(2, Some(0), Some((1, 1, 2)), 1.0),
    label(3, Some(1), Some((2, 1, 3)), 0.0),
  ];
  let backward = [
    label(5, None, None, 0.0),
    label(2, Some(0), Some((2, 1, 5)), 2.0),
    label(4, Some(1), Some((4, 1, 2)), 0.0),
  ];
  let result = meeting_path(
    &forward,
    &backward,
    Meeting {
      forward: 2,
      edge: (3, 1, 4),
      weight: 0.0,
      backward: 2,
    },
  );
  assert_eq!(result.path, vec![1, 2, 5]);
  assert_eq!(result.edges, vec![(1, 1, 2), (2, 1, 5)]);
  assert_eq!(result.total_weight, 3.0);
}
