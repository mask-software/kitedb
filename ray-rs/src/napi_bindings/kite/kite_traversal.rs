//! Traversal builder for fluent graph traversal API
//!
//! `whereEdge`/`whereNode` predicates are JS functions. They cannot run inside the core
//! traversal iterator, whose filters are `Send + Sync` Rust closures, and JS must not be called
//! while the Kite lock is held (a predicate may call back into the database). So a traversal
//! without predicates runs on the core `TraversalBuilder`, and one with predicates runs here,
//! step by step:
//!
//! - each predicate filters the step it follows (one added before the first step filters the
//!   start nodes), and several predicates on a step must all pass;
//! - a node rejected after a single hop is not marked visited, so another edge of the same hop
//!   may still reach it (as with the core's step filters);
//! - after `traverse()`, the predicates select which of the reached nodes are kept and continue;
//!   they do not prune the search itself;
//! - `take()` applies to the filtered results.

#![allow(clippy::arc_with_non_send_sync)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::api::kite::{Kite as RustKite, KiteTraversalProps};
use crate::api::traversal::{
  NoProps, TraversalBuilder, TraversalDirection, TraversalStep, TraverseOptions,
};
use crate::types::{ETypeId, Edge, NodeId};

use super::helpers::{
  call_filter, edge_filter_arg, edge_filter_data, filter_fn, node_filter_arg, node_filter_data,
  node_to_js, FilterFn,
};
use crate::napi_bindings::database::JsFullEdge;
use crate::napi_bindings::traversal::JsTraverseOptions;
use crate::napi_bindings::validation;

// =============================================================================
// Steps and predicates
// =============================================================================

/// A `whereEdge`/`whereNode` predicate, attached to the step it follows.
#[derive(Clone)]
enum StepFilter {
  Edge(Arc<FilterFn>),
  Node(Arc<FilterFn>),
}

/// The steps of a traversal as a persistent list, so forking a traversal shares its prefix.
#[derive(Clone, Default)]
struct StepChain {
  head: Option<Arc<StepNode>>,
  len: usize,
}

struct StepNode {
  step: TraversalStep,
  filters: Vec<StepFilter>,
  prev: Option<Arc<StepNode>>,
}

impl StepChain {
  fn push(&self, step: TraversalStep) -> Self {
    Self {
      head: Some(Arc::new(StepNode {
        step,
        filters: Vec::new(),
        prev: self.head.clone(),
      })),
      len: self.len + 1,
    }
  }

  /// This chain with `filter` added to its last step, or `None` if it has no step.
  fn with_filter(&self, filter: StepFilter) -> Option<Self> {
    let head = self.head.as_ref()?;
    let mut filters = head.filters.clone();
    filters.push(filter);
    Some(Self {
      head: Some(Arc::new(StepNode {
        step: head.step.clone(),
        filters,
        prev: head.prev.clone(),
      })),
      len: self.len,
    })
  }

  fn to_vec(&self) -> Vec<(TraversalStep, Vec<StepFilter>)> {
    let mut steps = Vec::with_capacity(self.len);
    let mut current = self.head.as_deref();
    while let Some(node) = current {
      steps.push((node.step.clone(), node.filters.clone()));
      current = node.prev.as_deref();
    }
    steps.reverse();
    steps
  }
}

// =============================================================================
// Traversal Builder
// =============================================================================

#[napi]
pub struct KiteTraversal {
  ray: Arc<RwLock<Option<RustKite>>>,
  start_nodes: Vec<NodeId>,
  /// Predicates added before the first step; they filter the start nodes.
  start_filters: Vec<StepFilter>,
  steps: StepChain,
  limit: Option<usize>,
  /// The node props `select()` limits loading to.
  selected_props: Option<Arc<[String]>>,
}

impl KiteTraversal {
  pub(crate) fn new(ray: Arc<RwLock<Option<RustKite>>>, start_nodes: Vec<NodeId>) -> Self {
    Self {
      ray,
      start_nodes,
      start_filters: Vec::new(),
      steps: StepChain::default(),
      limit: None,
      selected_props: None,
    }
  }

  fn fork(&self) -> KiteTraversal {
    KiteTraversal {
      ray: self.ray.clone(),
      start_nodes: self.start_nodes.clone(),
      start_filters: self.start_filters.clone(),
      steps: self.steps.clone(),
      limit: self.limit,
      selected_props: self.selected_props.clone(),
    }
  }

  fn with_filter(&self, filter: StepFilter) -> KiteTraversal {
    let mut next = self.fork();
    match self.steps.with_filter(filter.clone()) {
      Some(steps) => next.steps = steps,
      None => next.start_filters.push(filter),
    }
    next
  }

  fn with_step(&self, step: TraversalStep) -> KiteTraversal {
    let mut next = self.fork();
    next.steps = self.steps.push(step);
    next
  }

  fn single_hop(&self, direction: TraversalDirection, edge_type: Option<String>) -> Result<Self> {
    let etype = self.resolve_etype(edge_type)?;
    Ok(self.with_step(TraversalStep::SingleHop {
      direction,
      etype,
      edge_filter: None,
      node_filter: None,
    }))
  }

  fn with_ray<R>(&self, f: impl FnOnce(&RustKite) -> Result<R>) -> Result<R> {
    let guard = self.ray.read();
    let ray = guard
      .as_ref()
      .ok_or_else(|| Error::from_reason("Kite is closed"))?;
    f(ray)
  }

  fn has_filters(steps: &[(TraversalStep, Vec<StepFilter>)], start: &[StepFilter]) -> bool {
    !start.is_empty() || steps.iter().any(|(_, filters)| !filters.is_empty())
  }

  /// The core builder for a traversal without predicates.
  fn core_builder(&self, steps: Vec<(TraversalStep, Vec<StepFilter>)>) -> TraversalBuilder {
    let mut builder = TraversalBuilder::new(self.start_nodes.clone());
    for (step, _) in steps {
      builder.push_step(step);
    }
    if let Some(limit) = self.limit {
      builder = builder.take(limit);
    }
    builder
  }

  /// Run the traversal: each result is a node and the edge of the hop that reached it.
  fn run(&self, env: &Env) -> Result<Vec<Hit>> {
    let steps = self.steps.to_vec();
    if !Self::has_filters(&steps, &self.start_filters) {
      let builder = self.core_builder(steps);
      return self.with_ray(|ray| {
        Ok(
          builder
            .execute_source(ray.neighbor_source(), NoProps)
            .map(|result| Hit {
              node_id: result.node_id,
              edge: result.edge.map(|edge| Edge {
                src: edge.src,
                etype: edge.etype,
                dst: edge.dst,
              }),
            })
            .collect(),
        )
      });
    }

    let plan = Plan {
      start_nodes: &self.start_nodes,
      start_filters: &self.start_filters,
      steps: &steps,
      limit: self.limit,
    };
    // The lock is taken per expansion and per predicate input, never across a JS call.
    run_plan(
      &plan,
      |node_id, dir, etype| self.with_ray(|ray| Ok(ray.neighbors(node_id, dir, etype))),
      |filters, hit| self.passes(env, filters, hit),
    )
  }

  /// Whether `hit` passes every predicate in `filters`. A predicate on edges passes the start
  /// nodes, which were not reached over an edge.
  fn passes(&self, env: &Env, filters: &[StepFilter], hit: &Hit) -> Result<bool> {
    let needs_node = filters.iter().any(|f| matches!(f, StepFilter::Node(_)));
    let needs_edge = filters.iter().any(|f| matches!(f, StepFilter::Edge(_)));
    let (node, edge) = self.with_ray(|ray| {
      let props = KiteTraversalProps::new(ray.raw(), self.selected_props.as_deref());
      let node = needs_node.then(|| node_filter_data(ray, &props, hit.node_id));
      let edge = hit
        .edge
        .filter(|_| needs_edge)
        .map(|edge| edge_filter_data(&props, &edge));
      Ok((node, edge))
    })?;

    for filter in filters {
      let keep = match (filter, &node, &edge) {
        (StepFilter::Node(predicate), Some(node), _) => {
          call_filter(env, predicate, node_filter_arg(env, node)?)?
        }
        (StepFilter::Edge(predicate), _, Some(edge)) => {
          call_filter(env, predicate, edge_filter_arg(env, edge)?)?
        }
        _ => true,
      };
      if !keep {
        return Ok(false);
      }
    }
    Ok(true)
  }

  fn resolve_etype(&self, edge_type: Option<String>) -> Result<Option<ETypeId>> {
    let edge_type = match edge_type {
      Some(edge_type) => edge_type,
      None => return Ok(None),
    };
    self.with_ray(|ray| {
      let edge_def = ray
        .edge_def(&edge_type)
        .ok_or_else(|| Error::from_reason(format!("Unknown edge type: {edge_type}")))?;
      let etype_id = edge_def
        .etype_id
        .ok_or_else(|| Error::from_reason("Edge type not initialized"))?;
      Ok(Some(etype_id))
    })
  }
}

#[napi]
impl KiteTraversal {
  /// Keep only results whose edge passes `func`. Applies to the step it follows.
  #[napi(js_name = "whereEdge")]
  pub fn where_edge(&self, func: Unknown) -> Result<KiteTraversal> {
    Ok(self.with_filter(StepFilter::Edge(filter_fn(func, "whereEdge")?)))
  }

  /// Keep only results whose node passes `func`. Applies to the step it follows, or to the
  /// start nodes before the first step.
  #[napi(js_name = "whereNode")]
  pub fn where_node(&self, func: Unknown) -> Result<KiteTraversal> {
    Ok(self.with_filter(StepFilter::Node(filter_fn(func, "whereNode")?)))
  }

  #[napi]
  pub fn out(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    self.single_hop(TraversalDirection::Out, edge_type)
  }

  #[napi(js_name = "in")]
  pub fn in_(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    self.single_hop(TraversalDirection::In, edge_type)
  }

  #[napi]
  pub fn both(&self, edge_type: Option<String>) -> Result<KiteTraversal> {
    self.single_hop(TraversalDirection::Both, edge_type)
  }

  #[napi]
  pub fn traverse(
    &self,
    edge_type: Option<String>,
    options: JsTraverseOptions,
  ) -> Result<KiteTraversal> {
    let etype = self.resolve_etype(edge_type)?;
    let options = options.to_rust()?;
    Ok(self.with_step(TraversalStep::Traverse { etype, options }))
  }

  /// Limit the number of results, counted after `whereEdge`/`whereNode` filtering.
  #[napi]
  pub fn take(&self, limit: i64) -> Result<KiteTraversal> {
    let mut next = self.fork();
    next.limit = Some(validation::non_negative_usize(
      "limit",
      limit,
      validation::MAX_COUNT,
    )?);
    Ok(next)
  }

  #[napi]
  pub fn select(&self, props: Vec<String>) -> Result<KiteTraversal> {
    let mut next = self.fork();
    next.selected_props = Some(Arc::from(props));
    Ok(next)
  }

  #[napi]
  pub fn nodes(&self, env: Env) -> Result<Vec<i64>> {
    Ok(
      self
        .run(&env)?
        .into_iter()
        .map(|hit| hit.node_id as i64)
        .collect(),
    )
  }

  #[napi(js_name = "nodesWithProps")]
  pub fn nodes_with_props(&self, env: Env) -> Result<Vec<Object<'_>>> {
    let hits = self.run(&env)?;
    let nodes = self.with_ray(|ray| {
      let props = KiteTraversalProps::new(ray.raw(), self.selected_props.as_deref());
      Ok(
        hits
          .iter()
          .map(|hit| node_filter_data(ray, &props, hit.node_id))
          .collect::<Vec<_>>(),
      )
    })?;
    nodes
      .into_iter()
      .map(|node| node_to_js(&env, node.id, Some(node.key), &node.node_type, node.props))
      .collect()
  }

  #[napi]
  pub fn edges(&self, env: Env) -> Result<Vec<JsFullEdge>> {
    Ok(
      self
        .run(&env)?
        .into_iter()
        .filter_map(|hit| hit.edge)
        .map(|edge| JsFullEdge {
          src: edge.src as f64,
          etype: edge.etype,
          dst: edge.dst as f64,
        })
        .collect(),
    )
  }

  #[napi]
  pub fn count(&self, env: Env) -> Result<i64> {
    let steps = self.steps.to_vec();
    if Self::has_filters(&steps, &self.start_filters) {
      return Ok(self.run(&env)?.len() as i64);
    }
    let builder = self.core_builder(steps);
    self.with_ray(|ray| Ok(builder.count_source(ray.neighbor_source(), NoProps) as i64))
  }
}

// =============================================================================
// Filtered execution
// =============================================================================

/// A traversal result: the node reached and the edge of the hop that reached it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hit {
  node_id: NodeId,
  edge: Option<Edge>,
}

/// What a filtered traversal runs; `F` is the predicate type.
struct Plan<'a, F> {
  start_nodes: &'a [NodeId],
  start_filters: &'a [F],
  steps: &'a [(TraversalStep, Vec<F>)],
  limit: Option<usize>,
}

/// Run `plan` step by step with the core traversal's semantics (nodes are unique across the
/// whole traversal), calling `passes` for each candidate of a step that has predicates. The
/// limit stops the last step early, so no predicate runs for results past it.
fn run_plan<F>(
  plan: &Plan<'_, F>,
  neighbors: impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Result<Vec<Edge>>,
  mut passes: impl FnMut(&[F], &Hit) -> Result<bool>,
) -> Result<Vec<Hit>> {
  let last = plan.steps.len();
  let cap_at = |step: usize| if step == last { plan.limit } else { None };

  let mut visited: HashSet<NodeId> = plan.start_nodes.iter().copied().collect();
  let start = plan
    .start_nodes
    .iter()
    .map(|&node_id| Hit {
      node_id,
      edge: None,
    })
    .collect();
  let mut frontier = select(start, plan.start_filters, None, cap_at(0), &mut passes)?;

  for (index, (step, filters)) in plan.steps.iter().enumerate() {
    if frontier.is_empty() {
      break;
    }
    let cap = cap_at(index + 1);
    frontier = match step {
      TraversalStep::SingleHop {
        direction, etype, ..
      } => {
        let candidates = expand_hop(&neighbors, &frontier, *direction, *etype, &visited)?;
        select(candidates, filters, Some(&mut visited), cap, &mut passes)?
      }
      TraversalStep::Traverse { etype, options } => {
        let reached = expand_traverse(&neighbors, &frontier, *etype, options, &mut visited)?;
        select(reached, filters, None, cap, &mut passes)?
      }
    };
  }

  if let Some(limit) = plan.limit {
    frontier.truncate(limit);
  }
  Ok(frontier)
}

/// The candidates that pass `filters`, in order, at most `cap` of them. With `visited`, a node
/// already in it is skipped, and a kept node is added to it.
fn select<F>(
  candidates: Vec<Hit>,
  filters: &[F],
  mut visited: Option<&mut HashSet<NodeId>>,
  cap: Option<usize>,
  passes: &mut impl FnMut(&[F], &Hit) -> Result<bool>,
) -> Result<Vec<Hit>> {
  let mut kept = Vec::new();
  for hit in candidates {
    if cap.is_some_and(|cap| kept.len() >= cap) {
      break;
    }
    if visited.as_ref().is_some_and(|v| v.contains(&hit.node_id)) {
      continue;
    }
    if !filters.is_empty() && !passes(filters, &hit)? {
      continue;
    }
    if let Some(visited) = visited.as_mut() {
      visited.insert(hit.node_id);
    }
    kept.push(hit);
  }
  Ok(kept)
}

/// The node a hop in `direction` reaches from `node_id` over `edge`.
fn neighbor_of(edge: &Edge, node_id: NodeId, direction: TraversalDirection) -> NodeId {
  match direction {
    TraversalDirection::Out => edge.dst,
    TraversalDirection::In => edge.src,
    TraversalDirection::Both => {
      if edge.src == node_id {
        edge.dst
      } else {
        edge.src
      }
    }
  }
}

/// Every edge of a single hop from `frontier` to a node not visited before the hop.
fn expand_hop(
  neighbors: &impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Result<Vec<Edge>>,
  frontier: &[Hit],
  direction: TraversalDirection,
  etype: Option<ETypeId>,
  visited: &HashSet<NodeId>,
) -> Result<Vec<Hit>> {
  let mut candidates = Vec::new();
  for hit in frontier {
    for edge in neighbors(hit.node_id, direction, etype)? {
      let node_id = neighbor_of(&edge, hit.node_id, direction);
      if !visited.contains(&node_id) {
        candidates.push(Hit {
          node_id,
          edge: Some(edge),
        });
      }
    }
  }
  Ok(candidates)
}

/// The nodes a `traverse()` step reaches from `frontier`: a breadth-first search, as in the
/// core iterator, adding every node it reaches to `visited`.
fn expand_traverse(
  neighbors: &impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Result<Vec<Edge>>,
  frontier: &[Hit],
  etype: Option<ETypeId>,
  options: &TraverseOptions,
  visited: &mut HashSet<NodeId>,
) -> Result<Vec<Hit>> {
  let directions: &[TraversalDirection] = match options.direction {
    TraversalDirection::Both => &[TraversalDirection::Out, TraversalDirection::In],
    TraversalDirection::Out => &[TraversalDirection::Out],
    TraversalDirection::In => &[TraversalDirection::In],
  };
  let mut seen_in_step: HashSet<NodeId> = if options.unique {
    frontier.iter().map(|hit| hit.node_id).collect()
  } else {
    HashSet::new()
  };
  let mut queue: VecDeque<(NodeId, usize)> = frontier.iter().map(|hit| (hit.node_id, 0)).collect();
  let mut reached = Vec::new();

  while let Some((node_id, depth)) = queue.pop_front() {
    if depth >= options.max_depth {
      continue;
    }
    for &direction in directions {
      for edge in neighbors(node_id, direction, etype)? {
        let next = neighbor_of(&edge, node_id, direction);
        if options.unique && !seen_in_step.insert(next) {
          continue;
        }
        if !visited.insert(next) {
          continue;
        }
        let next_depth = depth + 1;
        if next_depth >= options.min_depth {
          reached.push(Hit {
            node_id: next,
            edge: Some(edge),
          });
        }
        if next_depth < options.max_depth {
          queue.push_back((next, next_depth));
        }
      }
    }
  }
  Ok(reached)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::api::traversal::NodeInfo;
  use rand::rngs::StdRng;
  use rand::{Rng, SeedableRng};

  /// A random multigraph as an edge list; neighbor order follows insertion order.
  struct Graph {
    edges: Vec<Edge>,
  }

  impl Graph {
    fn random(rng: &mut StdRng, nodes: u64, edges: usize) -> Self {
      let edges = (0..edges)
        .map(|_| Edge {
          src: rng.gen_range(1..=nodes),
          etype: rng.gen_range(1..=2),
          dst: rng.gen_range(1..=nodes),
        })
        .collect();
      Self { edges }
    }

    /// Same contract as `Kite::neighbors`, which the traversal expands its hops with.
    fn neighbors(
      &self,
      node_id: NodeId,
      dir: TraversalDirection,
      etype: Option<ETypeId>,
    ) -> Vec<Edge> {
      let matches = |edge: &&Edge| etype.is_none_or(|etype| edge.etype == etype);
      match dir {
        TraversalDirection::Out => self
          .edges
          .iter()
          .filter(|e| e.src == node_id)
          .filter(matches)
          .copied()
          .collect(),
        TraversalDirection::In => self
          .edges
          .iter()
          .filter(|e| e.dst == node_id)
          .filter(matches)
          .copied()
          .collect(),
        TraversalDirection::Both => {
          let mut out = self.neighbors(node_id, TraversalDirection::Out, etype);
          // A self-loop is also an out-edge: list it once.
          out.extend(
            self
              .neighbors(node_id, TraversalDirection::In, etype)
              .into_iter()
              .filter(|edge| edge.src != edge.dst),
          );
          out
        }
      }
    }
  }

  fn random_step(rng: &mut StdRng) -> TraversalStep {
    let direction = match rng.gen_range(0..3) {
      0 => TraversalDirection::Out,
      1 => TraversalDirection::In,
      _ => TraversalDirection::Both,
    };
    let etype = rng.gen_bool(0.5).then(|| rng.gen_range(1..=2));
    if rng.gen_bool(0.3) {
      let max_depth = rng.gen_range(0..4);
      let options = TraverseOptions::new(direction, max_depth)
        .with_min_depth(rng.gen_range(0..=max_depth))
        .with_unique(rng.gen_bool(0.7));
      TraversalStep::Traverse { etype, options }
    } else {
      TraversalStep::SingleHop {
        direction,
        etype,
        edge_filter: None,
        node_filter: None,
      }
    }
  }

  fn core_hits(builder: TraversalBuilder, graph: &Graph) -> Vec<Hit> {
    builder
      .execute(|node_id, dir, etype| graph.neighbors(node_id, dir, etype))
      .map(|result| Hit {
        node_id: result.node_id,
        edge: result.edge.map(|edge| Edge {
          src: edge.src,
          etype: edge.etype,
          dst: edge.dst,
        }),
      })
      .collect()
  }

  /// Without predicates, the step-by-step runner yields exactly what the core iterator yields:
  /// a filtered traversal reaches the same nodes as an unfiltered one before filtering.
  #[test]
  fn run_plan_matches_the_core_iterator_without_predicates() {
    let mut rng = StdRng::seed_from_u64(0x6b69_7465);
    for _ in 0..500 {
      let graph = Graph::random(&mut rng, 12, 30);
      let start_nodes: Vec<NodeId> = (0..rng.gen_range(1..3))
        .map(|_| rng.gen_range(1..=12))
        .collect();
      let steps: Vec<(TraversalStep, Vec<()>)> = (0..rng.gen_range(0..4))
        .map(|_| (random_step(&mut rng), Vec::new()))
        .collect();
      let limit = rng.gen_bool(0.3).then(|| rng.gen_range(0..6));

      let mut builder = TraversalBuilder::new(start_nodes.clone());
      for (step, _) in &steps {
        builder.push_step(step.clone());
      }
      if let Some(limit) = limit {
        builder = builder.take(limit);
      }
      let expected = core_hits(builder, &graph);

      let plan = Plan {
        start_nodes: &start_nodes,
        start_filters: &[],
        steps: &steps,
        limit,
      };
      let actual = run_plan(
        &plan,
        |node_id, dir, etype| Ok(graph.neighbors(node_id, dir, etype)),
        |_, _| -> Result<bool> { unreachable!("no predicates") },
      )
      .expect("run_plan");
      assert_eq!(actual, expected, "steps {:?}, start {start_nodes:?}", steps);
    }
  }

  /// A node predicate on a single hop behaves like the core's step-level node filter: it runs
  /// before the node is marked visited, so a rejected node does not block later steps.
  #[test]
  fn hop_predicates_match_core_step_filters() {
    let mut rng = StdRng::seed_from_u64(0x7374_6570);
    for _ in 0..500 {
      let graph = Graph::random(&mut rng, 10, 25);
      let start_nodes = vec![rng.gen_range(1..=10)];
      // Each hop rejects nodes whose id is divisible by its modulus.
      let moduli: Vec<u64> = (0..rng.gen_range(1..4))
        .map(|_| rng.gen_range(2..5))
        .collect();
      let directions: Vec<TraversalDirection> = moduli
        .iter()
        .map(|_| match rng.gen_range(0..3) {
          0 => TraversalDirection::Out,
          1 => TraversalDirection::In,
          _ => TraversalDirection::Both,
        })
        .collect();

      let mut builder = TraversalBuilder::new(start_nodes.clone());
      for (&modulus, &direction) in moduli.iter().zip(&directions) {
        builder.push_step(TraversalStep::SingleHop {
          direction,
          etype: None,
          edge_filter: None,
          node_filter: Some(Arc::new(move |node: &NodeInfo| {
            !node.id.is_multiple_of(modulus)
          })),
        });
      }
      let expected = core_hits(builder, &graph);

      let steps: Vec<(TraversalStep, Vec<u64>)> = moduli
        .iter()
        .zip(&directions)
        .map(|(&modulus, &direction)| {
          let step = TraversalStep::SingleHop {
            direction,
            etype: None,
            edge_filter: None,
            node_filter: None,
          };
          (step, vec![modulus])
        })
        .collect();
      let plan = Plan {
        start_nodes: &start_nodes,
        start_filters: &[],
        steps: &steps,
        limit: None,
      };
      let actual = run_plan(
        &plan,
        |node_id, dir, etype| Ok(graph.neighbors(node_id, dir, etype)),
        |moduli, hit| {
          Ok(
            moduli
              .iter()
              .all(|&modulus| !hit.node_id.is_multiple_of(modulus)),
          )
        },
      )
      .expect("run_plan");
      assert_eq!(
        actual, expected,
        "moduli {moduli:?}, directions {directions:?}"
      );
    }
  }

  /// The edges a hop expands list a self-loop once in `Both`, as `Kite::neighbors` does (A13):
  /// it is one edge, both an out- and an in-edge.
  #[test]
  fn hop_neighbors_list_a_self_loop_once_in_both() {
    use crate::api::kite::{EdgeDef, KiteOptions, NodeDef};
    let dir = tempfile::tempdir().expect("tempdir");
    let options = KiteOptions::new()
      .node(NodeDef::new("User", "user:"))
      .edge(EdgeDef::new("FOLLOWS"));
    let mut ray = RustKite::open(dir.path().join("self-loop.kitedb"), options).expect("open");
    let a = ray
      .create_node("User", "a", std::collections::HashMap::new())
      .expect("create a")
      .id();
    ray.link(a, "FOLLOWS", a).expect("self-loop");
    let etype = ray
      .edge_def("FOLLOWS")
      .and_then(|def| def.etype_id)
      .expect("etype");

    let edges = ray.neighbors(a, TraversalDirection::Both, None);
    assert_eq!(
      edges,
      vec![Edge {
        src: a,
        etype,
        dst: a
      }],
      "one self-loop is one edge in both directions"
    );
    ray.close().expect("close");
  }

  #[test]
  fn limit_counts_filtered_results_and_stops_calling_predicates() {
    // 1 -> 2..=11; only even nodes pass.
    let graph = Graph {
      edges: (2..=11)
        .map(|dst| Edge {
          src: 1,
          etype: 1,
          dst,
        })
        .collect(),
    };
    let steps = vec![(
      TraversalStep::SingleHop {
        direction: TraversalDirection::Out,
        etype: None,
        edge_filter: None,
        node_filter: None,
      },
      vec![()],
    )];
    let plan = Plan {
      start_nodes: &[1],
      start_filters: &[],
      steps: &steps,
      limit: Some(3),
    };
    let mut calls = 0;
    let hits = run_plan(
      &plan,
      |node_id, dir, etype| Ok(graph.neighbors(node_id, dir, etype)),
      |_, hit| {
        calls += 1;
        Ok(hit.node_id.is_multiple_of(2))
      },
    )
    .expect("run_plan");
    let ids: Vec<NodeId> = hits.iter().map(|hit| hit.node_id).collect();
    assert_eq!(ids, vec![2, 4, 6]);
    assert_eq!(
      calls, 5,
      "nodes 2..=6 are checked; nothing after the third match"
    );
  }
}
