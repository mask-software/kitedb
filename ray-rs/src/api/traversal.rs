//! Traversal API
//!
//! Fluent API for graph traversal with lazy iterator results.
//!
//! Ported from src/api/traversal.ts

use crate::core::single_file::SingleFileDB;
use crate::types::{ETypeId, Edge, NodeId, PropValue};
use hashbrown::{HashMap as FastMap, HashSet as FastSet};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Type alias for edge filter predicates
pub type EdgeFilter = Arc<dyn Fn(&EdgeInfo) -> bool + Send + Sync>;

/// Type alias for node filter predicates  
pub type NodeFilter = Arc<dyn Fn(&NodeInfo) -> bool + Send + Sync>;

// ============================================================================
// Traversal Types
// ============================================================================

/// Direction for traversal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalDirection {
  Out,
  In,
  Both,
}

/// Options for variable-depth traversal
#[derive(Clone)]
pub struct TraverseOptions {
  /// Direction of traversal
  pub direction: TraversalDirection,
  /// Minimum depth (default: 1)
  pub min_depth: usize,
  /// Maximum depth
  pub max_depth: usize,
  /// Whether to only visit unique nodes (default: true)
  pub unique: bool,
  /// Edge filter predicate for variable-depth traversal
  pub where_edge: Option<EdgeFilter>,
  /// Node filter predicate for variable-depth traversal
  pub where_node: Option<NodeFilter>,
}

impl std::fmt::Debug for TraverseOptions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TraverseOptions")
      .field("direction", &self.direction)
      .field("min_depth", &self.min_depth)
      .field("max_depth", &self.max_depth)
      .field("unique", &self.unique)
      .field("where_edge", &self.where_edge.as_ref().map(|_| "<fn>"))
      .field("where_node", &self.where_node.as_ref().map(|_| "<fn>"))
      .finish()
  }
}

impl Default for TraverseOptions {
  fn default() -> Self {
    Self {
      direction: TraversalDirection::Out,
      min_depth: 1,
      max_depth: 1,
      unique: true,
      where_edge: None,
      where_node: None,
    }
  }
}

impl TraverseOptions {
  pub fn new(direction: TraversalDirection, max_depth: usize) -> Self {
    Self {
      direction,
      min_depth: 1,
      max_depth,
      unique: true,
      where_edge: None,
      where_node: None,
    }
  }

  pub fn with_min_depth(mut self, min_depth: usize) -> Self {
    self.min_depth = min_depth;
    self
  }

  pub fn with_unique(mut self, unique: bool) -> Self {
    self.unique = unique;
    self
  }

  /// Add an edge filter predicate for variable-depth traversal
  ///
  /// Applied at every hop: the traversal does not follow an edge the predicate rejects.
  pub fn with_edge_filter<F>(mut self, predicate: F) -> Self
  where
    F: Fn(&EdgeInfo) -> bool + Send + Sync + 'static,
  {
    self.where_edge = Some(Arc::new(predicate));
    self
  }

  /// Add a node filter predicate for variable-depth traversal
  ///
  /// Applied at every hop: the traversal neither yields nor expands a node the predicate
  /// rejects.
  pub fn with_node_filter<F>(mut self, predicate: F) -> Self
  where
    F: Fn(&NodeInfo) -> bool + Send + Sync + 'static,
  {
    self.where_node = Some(Arc::new(predicate));
    self
  }
}

/// Raw edge data without any property loading
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEdge {
  pub src: NodeId,
  pub dst: NodeId,
  pub etype: ETypeId,
}

impl From<Edge> for RawEdge {
  fn from(edge: Edge) -> Self {
    Self {
      src: edge.src,
      dst: edge.dst,
      etype: edge.etype,
    }
  }
}

/// Edge result with properties
#[derive(Debug, Clone)]
pub struct EdgeResult {
  pub src: NodeId,
  pub dst: NodeId,
  pub etype: ETypeId,
  pub props: Vec<(String, PropValue)>,
}

/// Traversal result with node and edge
#[derive(Debug, Clone)]
pub struct TraversalResult {
  pub node_id: NodeId,
  pub edge: Option<RawEdge>,
  pub depth: usize,
}

/// Edge info for filter predicates
#[derive(Debug, Clone)]
pub struct EdgeInfo {
  pub src: NodeId,
  pub dst: NodeId,
  pub etype: ETypeId,
  /// The edge's props, as loaded by the traversal's [`TraversalProps`] (empty with
  /// [`NoProps`], i.e. for [`TraversalBuilder::execute`]).
  pub props: HashMap<String, PropValue>,
}

impl From<RawEdge> for EdgeInfo {
  fn from(edge: RawEdge) -> Self {
    Self {
      src: edge.src,
      dst: edge.dst,
      etype: edge.etype,
      props: HashMap::new(),
    }
  }
}

/// Node info for filter predicates
#[derive(Debug, Clone)]
pub struct NodeInfo {
  pub id: NodeId,
  /// The node's props, as loaded by the traversal's [`TraversalProps`] (empty with
  /// [`NoProps`], i.e. for [`TraversalBuilder::execute`]).
  pub props: HashMap<String, PropValue>,
}

/// Supplies the props that filter predicates see in [`EdgeInfo::props`] and
/// [`NodeInfo::props`].
///
/// Props are loaded only to evaluate a filter. Traversals started from `Kite` load them from the
/// database; [`TraversalBuilder::execute`] uses [`NoProps`].
pub trait TraversalProps {
  /// Props of a node
  fn node_props(&self, node_id: NodeId) -> HashMap<String, PropValue>;
  /// Props of an edge
  fn edge_props(&self, edge: &RawEdge) -> HashMap<String, PropValue>;
}

/// [`TraversalProps`] that loads nothing: filters see empty prop maps.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProps;

impl TraversalProps for NoProps {
  fn node_props(&self, _node_id: NodeId) -> HashMap<String, PropValue> {
    HashMap::new()
  }

  fn edge_props(&self, _edge: &RawEdge) -> HashMap<String, PropValue> {
    HashMap::new()
  }
}

fn edge_info<P: TraversalProps>(props: &P, edge: RawEdge) -> EdgeInfo {
  EdgeInfo {
    src: edge.src,
    dst: edge.dst,
    etype: edge.etype,
    props: props.edge_props(&edge),
  }
}

fn node_info<P: TraversalProps>(props: &P, node_id: NodeId) -> NodeInfo {
  NodeInfo {
    id: node_id,
    props: props.node_props(node_id),
  }
}

// ============================================================================
// Neighbor Sources
// ============================================================================

/// Where a traversal reads each node's edges from.
///
/// [`TraversalBuilder::execute`] takes a function `(node, direction, etype) -> Vec<Edge>`,
/// which is a `NeighborSource` too. A source may yield edges lazily: the last step of a
/// traversal reads only as many as it needs, so `take(n)` from a node with many edges reads
/// about `n` of them ([`DbNeighbors`] does).
pub trait NeighborSource {
  /// The edges of one node
  type Edges: Iterator<Item = Edge>;

  /// The edges of `node_id` in `direction`, of type `etype` (every type for `None`). `Both`
  /// lists the out-edges, then the in-edges that are not self-loops (a self-loop is an out-edge
  /// too), so each edge once.
  fn edges(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Self::Edges;

  /// Every edge [`Self::edges`] yields, at once: for a step that reads them all.
  fn all_edges(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    self.edges(node_id, direction, etype).collect()
  }
}

impl<F> NeighborSource for F
where
  F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
{
  type Edges = std::vec::IntoIter<Edge>;

  fn edges(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Self::Edges {
    self(node_id, direction, etype).into_iter()
  }
}

/// The edges of `node_id` in `direction`, from its out-edges `(etype, dst)` and in-edges
/// `(etype, src)`, each read only if `direction` needs it: `Both` lists the out-edges, then the
/// in-edges that are not self-loops (a self-loop is an out-edge too), as
/// [`NeighborSource::edges`] does.
pub fn edges_in_direction<O, I>(
  node_id: NodeId,
  direction: TraversalDirection,
  out_edges: impl FnOnce() -> O,
  in_edges: impl FnOnce() -> I,
) -> Vec<Edge>
where
  O: IntoIterator<Item = (ETypeId, NodeId)>,
  I: IntoIterator<Item = (ETypeId, NodeId)>,
{
  let out = || {
    out_edges().into_iter().map(move |(etype, dst)| Edge {
      src: node_id,
      etype,
      dst,
    })
  };
  let incoming = || {
    in_edges().into_iter().map(move |(etype, src)| Edge {
      src,
      etype,
      dst: node_id,
    })
  };
  match direction {
    TraversalDirection::Out => out().collect(),
    TraversalDirection::In => incoming().collect(),
    TraversalDirection::Both => out()
      .chain(incoming().filter(|edge| edge.src != edge.dst))
      .collect(),
  }
}

/// A database's edges for traversal and pathfinding: what `Kite` and the bindings expand hops
/// with. [`Self::neighbors`] lists a node's edges; as a [`NeighborSource`] it reads them lazily,
/// in slices.
#[derive(Clone, Copy)]
pub struct DbNeighbors<'a> {
  db: &'a SingleFileDB,
}

impl<'a> DbNeighbors<'a> {
  pub fn new(db: &'a SingleFileDB) -> Self {
    Self { db }
  }

  /// The edges of `node_id` in `direction`, of type `etype` (every type for `None`), as
  /// [`NeighborSource::edges`] lists them: out-edges in `(etype, dst)` order, in-edges in
  /// `(etype, src)` order.
  pub fn neighbors(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    edges_in_direction(
      node_id,
      direction,
      || self.db.out_edges_after(node_id, etype, None, usize::MAX),
      || self.db.in_edges_after(node_id, etype, None, usize::MAX),
    )
  }
}

impl<'a> NeighborSource for DbNeighbors<'a> {
  type Edges = DbEdges<'a>;

  fn edges(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> DbEdges<'a> {
    DbEdges {
      db: self.db,
      node_id,
      etype,
      direction,
      incoming: direction == TraversalDirection::In,
      after: None,
      slice: Vec::new().into_iter(),
      slice_incoming: false,
      slice_len: DbEdges::FIRST_SLICE,
      done: false,
    }
  }

  fn all_edges(
    &self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: Option<ETypeId>,
  ) -> Vec<Edge> {
    self.neighbors(node_id, direction, etype)
  }
}

/// One node's edges, read from the database in slices that double in size, each a seek to
/// where the previous one ended ([`SingleFileDB::out_edges_after`]). No database lock is held
/// between slices, so outside a transaction a slice reflects the commits made before it.
pub struct DbEdges<'a> {
  db: &'a SingleFileDB,
  node_id: NodeId,
  etype: Option<ETypeId>,
  direction: TraversalDirection,
  /// Whether the in-edges are being read (else the out-edges)
  incoming: bool,
  /// The key `(etype, other endpoint)` the last slice ended at
  after: Option<(ETypeId, NodeId)>,
  /// The slice being yielded, and whether it holds in-edges
  slice: std::vec::IntoIter<(ETypeId, NodeId)>,
  slice_incoming: bool,
  slice_len: usize,
  done: bool,
}

impl DbEdges<'_> {
  // Tests use small slices, so neighbor reads cross many slice boundaries.
  const FIRST_SLICE: usize = if cfg!(test) { 2 } else { 16 };
  const MAX_SLICE: usize = if cfg!(test) { 8 } else { 4096 };

  /// Read the next non-empty slice; `false` when no edges remain.
  fn read_slice(&mut self) -> bool {
    while !self.done {
      let incoming = self.incoming;
      let slice = if incoming {
        self
          .db
          .in_edges_after(self.node_id, self.etype, self.after, self.slice_len)
      } else {
        self
          .db
          .out_edges_after(self.node_id, self.etype, self.after, self.slice_len)
      };
      if slice.len() < self.slice_len {
        // This side is exhausted: `Both` reads the in-edges next.
        if self.direction == TraversalDirection::Both && !self.incoming {
          self.incoming = true;
          self.after = None;
          self.slice_len = Self::FIRST_SLICE;
        } else {
          self.done = true;
        }
      } else {
        self.after = slice.last().copied();
        self.slice_len = (self.slice_len * 2).min(Self::MAX_SLICE);
      }
      if !slice.is_empty() {
        self.slice = slice.into_iter();
        self.slice_incoming = incoming;
        return true;
      }
    }
    false
  }
}

impl Iterator for DbEdges<'_> {
  type Item = Edge;

  fn next(&mut self) -> Option<Edge> {
    loop {
      for (etype, other) in self.slice.by_ref() {
        if !self.slice_incoming {
          return Some(Edge {
            src: self.node_id,
            etype,
            dst: other,
          });
        }
        // In `Both`, a self-loop was listed with the out-edges.
        if other != self.node_id || self.direction != TraversalDirection::Both {
          return Some(Edge {
            src: other,
            etype,
            dst: self.node_id,
          });
        }
      }
      if !self.read_slice() {
        return None;
      }
    }
  }
}

// ============================================================================
// Traversal Step
// ============================================================================

/// A single step in a traversal query
#[derive(Clone)]
pub enum TraversalStep {
  /// Single-hop traversal (out, in, or both)
  SingleHop {
    direction: TraversalDirection,
    etype: Option<ETypeId>,
    /// Edge filter for this step
    edge_filter: Option<EdgeFilter>,
    /// Node filter for this step
    node_filter: Option<NodeFilter>,
  },
  /// Variable-depth traversal
  Traverse {
    etype: Option<ETypeId>,
    options: TraverseOptions,
  },
}

impl std::fmt::Debug for TraversalStep {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::SingleHop {
        direction,
        etype,
        edge_filter,
        node_filter,
      } => f
        .debug_struct("SingleHop")
        .field("direction", direction)
        .field("etype", etype)
        .field("edge_filter", &edge_filter.as_ref().map(|_| "<fn>"))
        .field("node_filter", &node_filter.as_ref().map(|_| "<fn>"))
        .finish(),
      Self::Traverse { etype, options } => f
        .debug_struct("Traverse")
        .field("etype", etype)
        .field("options", options)
        .finish(),
    }
  }
}

/// The directions a variable-depth step expands: `Both` is `Out`, then `In`.
fn expand_directions(direction: TraversalDirection) -> &'static [TraversalDirection] {
  match direction {
    TraversalDirection::Out => &[TraversalDirection::Out],
    TraversalDirection::In => &[TraversalDirection::In],
    TraversalDirection::Both => &[TraversalDirection::Out, TraversalDirection::In],
  }
}

/// The node a single-hop step reaches from `node_id` over `edge`.
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

// ============================================================================
// Traversal Builder
// ============================================================================

/// Builder for constructing traversal queries
///
/// # Example
/// ```rust,no_run
/// # use kitedb::api::traversal::{TraversalBuilder, TraversalDirection};
/// # use kitedb::types::{Edge, ETypeId, NodeId};
/// # fn neighbors_fn(
/// #   _: NodeId,
/// #   _: TraversalDirection,
/// #   _: Option<ETypeId>,
/// # ) -> Vec<Edge> {
/// #   Vec::new()
/// # }
/// # fn main() {
/// # let start_node_id: NodeId = 1;
/// # let follows_etype: ETypeId = 1;
/// # let knows_etype: ETypeId = 2;
/// let builder = TraversalBuilder::new(vec![start_node_id])
///     .out(Some(follows_etype))
///     .out(Some(knows_etype))
///     .where_edge(|e| e.etype == 1)
///     .take(10);
///
/// for result in builder.execute(&neighbors_fn) {
///     println!("Found node: {}", result.node_id);
/// }
/// # }
/// ```
#[derive(Clone)]
pub struct TraversalBuilder {
  /// Starting node IDs
  start_nodes: Vec<NodeId>,
  /// Traversal steps to execute
  steps: Vec<TraversalStep>,
  /// Maximum number of results (None = unlimited)
  limit: Option<usize>,
  /// Whether to skip visited nodes across all steps
  unique_nodes: bool,
  /// Global edge filter applied to all results
  edge_filter: Option<EdgeFilter>,
  /// Global node filter applied to all results
  node_filter: Option<NodeFilter>,
  /// Selected properties for node projection (None = load all)
  selected_props: Option<Vec<String>>,
}

impl std::fmt::Debug for TraversalBuilder {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TraversalBuilder")
      .field("start_nodes", &self.start_nodes)
      .field("steps", &self.steps)
      .field("limit", &self.limit)
      .field("unique_nodes", &self.unique_nodes)
      .field("edge_filter", &self.edge_filter.as_ref().map(|_| "<fn>"))
      .field("node_filter", &self.node_filter.as_ref().map(|_| "<fn>"))
      .field("selected_props", &self.selected_props)
      .finish()
  }
}

impl TraversalBuilder {
  /// Create a new traversal builder starting from the given nodes
  pub fn new(start_nodes: Vec<NodeId>) -> Self {
    Self {
      start_nodes,
      steps: Vec::new(),
      limit: None,
      unique_nodes: true,
      edge_filter: None,
      node_filter: None,
      selected_props: None,
    }
  }

  /// Create a new traversal builder starting from a single node
  pub fn from_node(node_id: NodeId) -> Self {
    Self::new(vec![node_id])
  }

  pub(crate) fn push_step(&mut self, step: TraversalStep) {
    self.steps.push(step);
  }

  /// Add an outgoing edge traversal step
  pub fn out(mut self, etype: Option<ETypeId>) -> Self {
    self.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::Out,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    self
  }

  /// Add an incoming edge traversal step
  pub fn r#in(mut self, etype: Option<ETypeId>) -> Self {
    self.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::In,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    self
  }

  /// Add a bidirectional edge traversal step
  pub fn both(mut self, etype: Option<ETypeId>) -> Self {
    self.steps.push(TraversalStep::SingleHop {
      direction: TraversalDirection::Both,
      etype,
      edge_filter: None,
      node_filter: None,
    });
    self
  }

  /// Add a variable-depth traversal step
  pub fn traverse(mut self, etype: Option<ETypeId>, options: TraverseOptions) -> Self {
    self.steps.push(TraversalStep::Traverse { etype, options });
    self
  }

  /// Limit the number of results
  pub fn take(mut self, limit: usize) -> Self {
    self.limit = Some(limit);
    self
  }

  /// Set whether to only visit unique nodes
  pub fn unique(mut self, unique: bool) -> Self {
    self.unique_nodes = unique;
    self
  }

  /// Add a global edge filter predicate
  ///
  /// Filters the results: a result is kept only if the edge that reached it passes. It does not
  /// prune the traversal, so the edges of earlier hops are not checked; use
  /// [`TraverseOptions::with_edge_filter`] to filter every hop. Start nodes (which have no edge)
  /// always pass.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use kitedb::api::traversal::TraversalBuilder;
  /// # use kitedb::types::ETypeId;
  /// # fn main() {
  /// # let knows_etype: ETypeId = 1;
  /// let builder = TraversalBuilder::from_node(1)
  ///     .out(Some(knows_etype))
  ///     .where_edge(|edge| edge.etype == 1);
  /// # }
  /// ```
  pub fn where_edge<F>(mut self, predicate: F) -> Self
  where
    F: Fn(&EdgeInfo) -> bool + Send + Sync + 'static,
  {
    self.edge_filter = Some(Arc::new(predicate));
    self
  }

  /// Add a global node filter predicate
  ///
  /// Filters the results: only result nodes where the predicate returns `true` are kept. It
  /// does not prune the traversal, so nodes of earlier hops are not checked; use
  /// [`TraverseOptions::with_node_filter`] to filter every hop.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use kitedb::api::traversal::TraversalBuilder;
  /// # use kitedb::types::ETypeId;
  /// # fn main() {
  /// # let knows_etype: ETypeId = 1;
  /// let builder = TraversalBuilder::from_node(1)
  ///     .out(Some(knows_etype))
  ///     .where_node(|node| node.id > 5);
  /// # }
  /// ```
  pub fn where_node<F>(mut self, predicate: F) -> Self
  where
    F: Fn(&NodeInfo) -> bool + Send + Sync + 'static,
  {
    self.node_filter = Some(Arc::new(predicate));
    self
  }

  /// Select the node properties to load
  ///
  /// Records which node props a caller that loads props (a [`TraversalProps`], or the
  /// bindings when they materialize results) should load, instead of all of them. Traversals
  /// started from `Kite` pass only these props to node filters.
  ///
  /// # Example
  /// ```rust,no_run
  /// # use kitedb::api::traversal::TraversalBuilder;
  /// # use kitedb::types::ETypeId;
  /// # fn main() {
  /// # let knows_etype: ETypeId = 1;
  /// let builder = TraversalBuilder::from_node(1)
  ///     .out(Some(knows_etype))
  ///     .select(vec!["name".to_string(), "age".to_string()]);
  /// # }
  /// ```
  pub fn select(mut self, props: Vec<String>) -> Self {
    self.selected_props = Some(props);
    self
  }

  /// Select specific properties to load using string slices
  ///
  /// Convenience method that accepts &str instead of String.
  pub fn select_props(mut self, props: &[&str]) -> Self {
    self.selected_props = Some(props.iter().map(|s| s.to_string()).collect());
    self
  }

  /// Get the selected properties (if any)
  pub fn selected_properties(&self) -> Option<&[String]> {
    self.selected_props.as_deref()
  }

  /// Check if the builder has any filters set
  pub fn has_filters(&self) -> bool {
    if self.edge_filter.is_some() || self.node_filter.is_some() {
      return true;
    }
    for step in &self.steps {
      match step {
        TraversalStep::SingleHop {
          edge_filter,
          node_filter,
          ..
        } => {
          if edge_filter.is_some() || node_filter.is_some() {
            return true;
          }
        }
        TraversalStep::Traverse { options, .. } => {
          if options.where_edge.is_some() || options.where_node.is_some() {
            return true;
          }
        }
      }
    }
    false
  }

  /// Build CollectOptions from the current builder configuration
  ///
  /// This creates CollectOptions with the selected properties for use
  /// when materializing results with properties.
  pub fn collect_options(&self) -> CollectOptions {
    let mut opts = CollectOptions::new();
    if let Some(ref props) = self.selected_props {
      opts = opts.select_node_props(props.clone());
    }
    opts
  }

  /// Execute the traversal and return an iterator of results
  ///
  /// The `neighbors` function should return neighbors for a given node and direction. Filters
  /// see empty props ([`NoProps`]); see [`Self::execute_with_props`].
  pub fn execute<F>(self, neighbors: F) -> TraversalIterator<F>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.execute_with_props(neighbors, NoProps)
  }

  /// Execute the traversal, with filters seeing the props `props` loads
  ///
  /// The iterator is lazy in the last step: with `take(n)`, it stops expanding once it has
  /// yielded `n` results.
  pub fn execute_with_props<F, P>(self, neighbors: F, props: P) -> TraversalIterator<F, P>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
    P: TraversalProps,
  {
    self.execute_source(neighbors, props)
  }

  /// Execute the traversal over the edges `source` yields, with filters seeing the props
  /// `props` loads
  ///
  /// The iterator is lazy in the last step, down to the edges of each node: with `take(n)`, it
  /// stops reading edges once it has yielded `n` results, so a source that yields edges lazily
  /// (like [`DbNeighbors`]) reads about `n` edges even from a node with many.
  pub fn execute_source<S, P>(self, source: S, props: P) -> TraversalIterator<S, P>
  where
    S: NeighborSource,
    P: TraversalProps,
  {
    TraversalIterator::new(self, source, props)
  }

  /// Execute the traversal and collect all node IDs
  pub fn collect_node_ids<F>(self, neighbors: F) -> Vec<NodeId>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.execute(neighbors).map(|r| r.node_id).collect()
  }

  /// Execute the traversal and count results (optimized path)
  pub fn count<F>(self, neighbors: F) -> usize
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.count_with_props(neighbors, NoProps)
  }

  /// Count results, with filters seeing the props `props` loads
  pub fn count_with_props<F, P>(self, neighbors: F, props: P) -> usize
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
    P: TraversalProps,
  {
    self.count_source(neighbors, props)
  }

  /// Count the results of a traversal over the edges `source` yields, with filters seeing the
  /// props `props` loads
  pub fn count_source<S, P>(self, source: S, props: P) -> usize
  where
    S: NeighborSource,
    P: TraversalProps,
  {
    // For simple traversals without variable-depth, use fast counting
    if self.can_use_fast_count() {
      return self.count_fast(&source);
    }

    // Fall back to full iteration
    self.execute_source(source, props).count()
  }

  /// Check if we can use the fast count path
  fn can_use_fast_count(&self) -> bool {
    // Cannot use fast path if any filters are set
    if self.has_filters() {
      return false;
    }

    // Can only use fast path for simple single-hop traversals
    for step in &self.steps {
      if matches!(step, TraversalStep::Traverse { .. }) {
        return false;
      }
    }
    true
  }

  /// Fast count for simple traversals.
  ///
  /// Counts exactly what `TraversalIterator` yields, without materializing results: with
  /// `unique`, the start nodes seed `visited` and each node is counted once across all steps;
  /// without it, every traversed edge yields a result, so the frontier tracks how many times
  /// each node was reached.
  fn count_fast<S: NeighborSource>(&self, source: &S) -> usize {
    let mut frontier: FastMap<NodeId, usize> = FastMap::new();
    for &node_id in &self.start_nodes {
      *frontier.entry(node_id).or_insert(0) += 1;
    }
    let mut visited: FastSet<NodeId> = if self.unique_nodes {
      frontier.keys().copied().collect()
    } else {
      FastSet::new()
    };

    for step in &self.steps {
      let TraversalStep::SingleHop {
        direction, etype, ..
      } = step
      else {
        unreachable!()
      };

      let mut next: FastMap<NodeId, usize> = FastMap::new();
      for (&node_id, &times_reached) in &frontier {
        for edge in source.all_edges(node_id, *direction, *etype) {
          let neighbor = neighbor_of(&edge, node_id, *direction);
          if self.unique_nodes {
            if visited.insert(neighbor) {
              next.insert(neighbor, 1);
            }
          } else {
            let count = next.entry(neighbor).or_insert(0);
            *count = count.saturating_add(times_reached);
          }
        }
      }
      frontier = next;
    }

    let total = frontier
      .values()
      .fold(0usize, |total, &count| total.saturating_add(count));
    self.limit.map_or(total, |limit| total.min(limit))
  }

  /// Get the edges that reached each result (no property loading)
  ///
  /// Yields exactly the `edge` of each result [`Self::execute`] yields, honoring every step,
  /// `take`, `unique` and filter. Start nodes (no edge) yield nothing.
  pub fn raw_edges<F>(self, neighbors: F) -> RawEdgeIterator<F>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    RawEdgeIterator {
      inner: self.execute(neighbors),
    }
  }
}

// ============================================================================
// Traversal Iterator
// ============================================================================

/// The work of the step being run.
struct StepRun<E> {
  /// Nodes to expand: (node, depth of the result that entered the step, hops taken within the
  /// step).
  queue: VecDeque<(NodeId, usize, usize)>,
  /// The node being expanded, with the edges of it not read yet.
  expanding: Option<Expansion<E>>,
  /// Nodes a `traverse()` step with `unique` has reached.
  local_visited: FastSet<NodeId>,
  /// Results produced and not consumed yet.
  results: VecDeque<TraversalResult>,
}

impl<E> Default for StepRun<E> {
  fn default() -> Self {
    Self {
      queue: VecDeque::new(),
      expanding: None,
      local_visited: FastSet::new(),
      results: VecDeque::new(),
    }
  }
}

/// A node being expanded: its edges in the direction being read, read one at a time.
struct Expansion<E> {
  node_id: NodeId,
  /// Depth of the result that entered the step
  base_depth: usize,
  /// Hops taken within the step
  hops: usize,
  /// Index into the step's directions (`Both` in a `traverse()` step reads `Out`, then `In`)
  direction: usize,
  edges: StepEdges<E>,
}

/// The edges of a node being expanded: read lazily by the last step of a traversal with a
/// limit (it may stop early), read at once otherwise (every edge will be read).
enum StepEdges<E> {
  Lazy(E),
  All(std::vec::IntoIter<Edge>),
}

impl<E: Iterator<Item = Edge>> Iterator for StepEdges<E> {
  type Item = Edge;

  fn next(&mut self) -> Option<Edge> {
    match self {
      Self::Lazy(edges) => edges.next(),
      Self::All(edges) => edges.next(),
    }
  }
}

/// The directions a step reads each node's edges in, one after the other.
fn step_directions(step: &TraversalStep) -> &'static [TraversalDirection] {
  match step {
    TraversalStep::SingleHop { direction, .. } => match direction {
      TraversalDirection::Out => &[TraversalDirection::Out],
      TraversalDirection::In => &[TraversalDirection::In],
      TraversalDirection::Both => &[TraversalDirection::Both],
    },
    TraversalStep::Traverse { options, .. } => expand_directions(options.direction),
  }
}

fn step_etype(step: &TraversalStep) -> Option<ETypeId> {
  match step {
    TraversalStep::SingleHop { etype, .. } | TraversalStep::Traverse { etype, .. } => *etype,
  }
}

/// Iterator for traversal results
///
/// Every step but the last runs to completion when the iterator reaches it, since the next
/// step expands its whole result set. The last step runs lazily, one edge at a time, so the
/// iterator stops reading edges once `take(n)` has `n` results.
pub struct TraversalIterator<F: NeighborSource, P = NoProps> {
  /// Where edges are read from
  neighbors: F,
  /// Loads the props filters see
  props: P,
  /// Steps to execute
  steps: Vec<TraversalStep>,
  /// Number of steps started (the last started one is being run)
  steps_started: usize,
  /// Work of the step being run; before the first step, its results are the start nodes
  run: StepRun<F::Edges>,
  /// Visited nodes (for uniqueness)
  visited: FastSet<NodeId>,
  /// Whether to track unique nodes
  unique_nodes: bool,
  /// Maximum results
  limit: Option<usize>,
  /// Results yielded so far
  yielded: usize,
  /// Whether we're done
  done: bool,
  /// Global edge filter
  edge_filter: Option<EdgeFilter>,
  /// Global node filter
  node_filter: Option<NodeFilter>,
}

impl<F, P> TraversalIterator<F, P>
where
  F: NeighborSource,
  P: TraversalProps,
{
  fn new(builder: TraversalBuilder, neighbors: F, props: P) -> Self {
    let mut run = StepRun::default();
    let mut visited = FastSet::new();

    // The start nodes are the results of "no step yet".
    for node_id in builder.start_nodes {
      if builder.unique_nodes {
        visited.insert(node_id);
      }
      run.results.push_back(TraversalResult {
        node_id,
        edge: None,
        depth: 0,
      });
    }

    Self {
      neighbors,
      props,
      steps: builder.steps,
      steps_started: 0,
      run,
      visited,
      unique_nodes: builder.unique_nodes,
      limit: builder.limit,
      yielded: 0,
      done: false,
      edge_filter: builder.edge_filter,
      node_filter: builder.node_filter,
    }
  }

  /// Check if a result passes the global filters
  fn passes_filters(&self, result: &TraversalResult) -> bool {
    if let (Some(edge_filter), Some(edge)) = (&self.edge_filter, result.edge) {
      if !edge_filter(&edge_info(&self.props, edge)) {
        return false;
      }
    }
    if let Some(node_filter) = &self.node_filter {
      if !node_filter(&node_info(&self.props, result.node_id)) {
        return false;
      }
    }
    true
  }

  /// Start the next step: its input is every result of the previous one.
  fn start_step(&mut self) {
    let step = &self.steps[self.steps_started];
    self.steps_started += 1;
    let inputs = std::mem::take(&mut self.run.results);
    self.run.queue = inputs
      .into_iter()
      .map(|result| (result.node_id, result.depth, 0))
      .collect();
    self.run.expanding = None;
    self.run.local_visited = match step {
      TraversalStep::Traverse { options, .. } if options.unique => self
        .run
        .queue
        .iter()
        .map(|&(node_id, ..)| node_id)
        .collect(),
      _ => FastSet::new(),
    };
  }

  /// Read edges of the step being run until one produces a result in `run.results`, or the
  /// node being expanded has none left. Returns false if the step has nothing left to expand.
  fn expand_next(&mut self) -> bool {
    let Self {
      neighbors,
      props,
      steps,
      steps_started,
      run,
      visited,
      unique_nodes,
      limit,
      ..
    } = self;
    let Some(step) = steps_started.checked_sub(1).map(|index| &steps[index]) else {
      return false;
    };
    let directions = step_directions(step);
    let etype = step_etype(step);
    let lazy = limit.is_some() && *steps_started == steps.len();
    let local_unique = matches!(step, TraversalStep::Traverse { options, .. } if options.unique);
    // A node's edges read in whole: room in the visited sets for all of them, so a hub's
    // neighbors grow the sets once, not a doubling at a time.
    let unique_nodes = *unique_nodes;
    let read =
      |node_id, direction, visited: &mut FastSet<NodeId>, local_visited: &mut FastSet<NodeId>| {
        if lazy {
          return StepEdges::Lazy(neighbors.edges(node_id, direction, etype));
        }
        let edges = neighbors.all_edges(node_id, direction, etype);
        if unique_nodes {
          visited.reserve(edges.len());
        }
        if local_unique {
          local_visited.reserve(edges.len());
        }
        StepEdges::All(edges.into_iter())
      };

    loop {
      let expansion = match run.expanding.as_mut() {
        Some(expansion) => expansion,
        None => {
          let Some((node_id, base_depth, hops)) = run.queue.pop_front() else {
            return false;
          };
          if let TraversalStep::Traverse { options, .. } = step {
            if hops >= options.max_depth {
              continue;
            }
          }
          let edges = read(node_id, directions[0], visited, &mut run.local_visited);
          run.expanding.insert(Expansion {
            node_id,
            base_depth,
            hops,
            direction: 0,
            edges,
          })
        }
      };
      let Some(edge) = expansion.edges.next() else {
        // The edges in this direction are done: read the next direction, or the next node.
        expansion.direction += 1;
        match directions.get(expansion.direction) {
          Some(&direction) => {
            expansion.edges = read(
              expansion.node_id,
              direction,
              visited,
              &mut run.local_visited,
            );
            continue;
          }
          None => {
            run.expanding = None;
            return true;
          }
        }
      };
      let (node_id, base_depth, hops) = (expansion.node_id, expansion.base_depth, expansion.hops);
      let dir = directions[expansion.direction];

      match step {
        TraversalStep::SingleHop {
          edge_filter,
          node_filter,
          ..
        } => {
          let neighbor_id = neighbor_of(&edge, node_id, dir);
          if unique_nodes && visited.contains(&neighbor_id) {
            continue;
          }
          let raw_edge = RawEdge::from(edge);
          if edge_filter
            .as_ref()
            .is_some_and(|filter| !filter(&edge_info(props, raw_edge)))
          {
            continue;
          }
          if node_filter
            .as_ref()
            .is_some_and(|filter| !filter(&node_info(props, neighbor_id)))
          {
            continue;
          }
          if unique_nodes {
            visited.insert(neighbor_id);
          }
          run.results.push_back(TraversalResult {
            node_id: neighbor_id,
            edge: Some(raw_edge),
            depth: base_depth + 1,
          });
          return true;
        }
        TraversalStep::Traverse { options, .. } => {
          // A self-loop is both an out- and an in-edge: follow it once.
          if options.direction == TraversalDirection::Both
            && dir == TraversalDirection::In
            && edge.src == edge.dst
          {
            continue;
          }
          let neighbor_id = neighbor_of(&edge, node_id, dir);
          if options.unique && run.local_visited.contains(&neighbor_id) {
            continue;
          }
          let raw_edge = RawEdge::from(edge);
          if options
            .where_edge
            .as_ref()
            .is_some_and(|filter| !filter(&edge_info(props, raw_edge)))
          {
            continue;
          }
          if options
            .where_node
            .as_ref()
            .is_some_and(|filter| !filter(&node_info(props, neighbor_id)))
          {
            continue;
          }
          if options.unique {
            run.local_visited.insert(neighbor_id);
          }
          if unique_nodes && !visited.insert(neighbor_id) {
            continue;
          }

          let next_hops = hops + 1;
          if next_hops < options.max_depth {
            run.queue.push_back((neighbor_id, base_depth, next_hops));
          }
          if next_hops >= options.min_depth {
            run.results.push_back(TraversalResult {
              node_id: neighbor_id,
              edge: Some(raw_edge),
              depth: base_depth + next_hops,
            });
            return true;
          }
        }
      }
    }
  }
}

impl<F, P> Iterator for TraversalIterator<F, P>
where
  F: NeighborSource,
  P: TraversalProps,
{
  type Item = TraversalResult;

  fn next(&mut self) -> Option<Self::Item> {
    if self.done {
      return None;
    }

    // Run every step but the last to completion.
    while self.steps_started < self.steps.len() {
      if self.steps_started > 0 {
        while self.expand_next() {}
      }
      self.start_step();
    }

    loop {
      if self.limit.is_some_and(|limit| self.yielded >= limit) {
        self.done = true;
        return None;
      }
      match self.run.results.pop_front() {
        Some(result) => {
          if self.passes_filters(&result) {
            self.yielded += 1;
            return Some(result);
          }
        }
        None => {
          if !self.expand_next() {
            self.done = true;
            return None;
          }
        }
      }
    }
  }
}

// ============================================================================
// Raw Edge Iterator
// ============================================================================

/// Iterator over the edges that reached each traversal result (no property loading)
pub struct RawEdgeIterator<F: NeighborSource> {
  inner: TraversalIterator<F>,
}

impl<F: NeighborSource> Iterator for RawEdgeIterator<F> {
  type Item = RawEdge;

  fn next(&mut self) -> Option<Self::Item> {
    loop {
      if let Some(edge) = self.inner.next()?.edge {
        return Some(edge);
      }
    }
  }
}

// ============================================================================
// Result Accessors
// ============================================================================

/// Result of a traversal query with accessor methods.
///
/// Provides fluent methods to access traversal results:
/// - `.nodes()` - Get an iterator over node IDs
/// - `.edges()` - Get an iterator over edges (with traversal info)
/// - `.to_vec()` - Collect all results into a Vec
/// - `.first()` - Get the first result
/// - `.count()` - Count all results
///
/// # Example
///
/// ```rust,no_run
/// # use kitedb::api::traversal::{TraversalBuilder, TraversalDirection};
/// # use kitedb::types::{Edge, ETypeId, NodeId};
/// # fn main() {
/// # let knows_etype: ETypeId = 1;
/// # let neighbors = |_: NodeId, _: TraversalDirection, _: Option<ETypeId>| -> Vec<Edge> {
/// #   Vec::new()
/// # };
/// // Get first node
/// let first = TraversalBuilder::from_node(1)
///     .out(Some(knows_etype))
///     .results(&neighbors)
///     .first();
///
/// // Collect all node IDs
/// let node_ids = TraversalBuilder::from_node(1)
///     .out(Some(knows_etype))
///     .results(&neighbors)
///     .nodes()
///     .collect::<Vec<_>>();
///
/// // Collect all edges
/// let edges = TraversalBuilder::from_node(1)
///     .out(Some(knows_etype))
///     .results(&neighbors)
///     .edges()
///     .collect::<Vec<_>>();
/// # }
/// ```
pub struct TraversalResults<I> {
  iter: I,
}

impl<I> TraversalResults<I>
where
  I: Iterator<Item = TraversalResult>,
{
  /// Create a new TraversalResults wrapper
  pub fn new(iter: I) -> Self {
    Self { iter }
  }

  /// Get an iterator over node IDs only
  ///
  /// This is useful when you only need the node IDs and don't care about
  /// the edges or depth information.
  pub fn nodes(self) -> NodeIdIterator<I> {
    NodeIdIterator { inner: self.iter }
  }

  /// Get an iterator over edges only
  ///
  /// Returns edges that were traversed to reach each node.
  /// The first result (start nodes) will have `None` for the edge.
  pub fn edges(self) -> EdgeIterator<I> {
    EdgeIterator { inner: self.iter }
  }

  /// Get an iterator over full traversal results
  ///
  /// Each result contains the node ID, the edge used to reach it,
  /// and the depth in the traversal.
  pub fn full(self) -> I {
    self.iter
  }

  /// Collect all node IDs into a Vec
  pub fn to_vec(self) -> Vec<NodeId> {
    self.iter.map(|r| r.node_id).collect()
  }

  /// Get the first result, or None if empty
  pub fn first(mut self) -> Option<TraversalResult> {
    self.iter.next()
  }

  /// Get the first node ID, or None if empty
  pub fn first_node(mut self) -> Option<NodeId> {
    self.iter.next().map(|r| r.node_id)
  }

  /// Count the number of results
  ///
  /// Note: This consumes the iterator.
  pub fn count(self) -> usize {
    self.iter.count()
  }
}

/// Iterator adapter that yields only node IDs from traversal results
pub struct NodeIdIterator<I> {
  inner: I,
}

impl<I> Iterator for NodeIdIterator<I>
where
  I: Iterator<Item = TraversalResult>,
{
  type Item = NodeId;

  fn next(&mut self) -> Option<Self::Item> {
    self.inner.next().map(|r| r.node_id)
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    self.inner.size_hint()
  }
}

/// Iterator adapter that yields only edges from traversal results
pub struct EdgeIterator<I> {
  inner: I,
}

impl<I> Iterator for EdgeIterator<I>
where
  I: Iterator<Item = TraversalResult>,
{
  type Item = RawEdge;

  fn next(&mut self) -> Option<Self::Item> {
    loop {
      {
        let result = self.inner.next()?;
        if let Some(edge) = result.edge {
          return Some(edge);
        }
        // Skip results without edges (start nodes)
      }
    }
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    let (_, upper) = self.inner.size_hint();
    (0, upper) // Lower bound is 0 because start nodes have no edges
  }
}

// ============================================================================
// TraversalBuilder Result Methods
// ============================================================================

impl TraversalBuilder {
  /// Execute the traversal and return results with accessor methods
  ///
  /// This is the recommended way to execute traversals when you need
  /// flexible access to the results.
  ///
  /// # Example
  ///
  /// ```rust,no_run
  /// # use kitedb::api::traversal::{TraversalBuilder, TraversalDirection};
  /// # use kitedb::types::{Edge, ETypeId, NodeId};
  /// # fn main() {
  /// # let knows_etype: ETypeId = 1;
  /// # let neighbors = |_: NodeId, _: TraversalDirection, _: Option<ETypeId>| -> Vec<Edge> {
  /// #   Vec::new()
  /// # };
  /// // Get first node ID
  /// let first = TraversalBuilder::from_node(1)
  ///     .out(Some(knows_etype))
  ///     .results(&neighbors)
  ///     .first_node();
  ///
  /// // Or collect all nodes
  /// let all_nodes = TraversalBuilder::from_node(1)
  ///     .out(Some(knows_etype))
  ///     .results(&neighbors)
  ///     .to_vec();
  /// # }
  /// ```
  pub fn results<F>(self, neighbors: F) -> TraversalResults<TraversalIterator<F>>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    TraversalResults::new(self.execute(neighbors))
  }

  /// Execute and get the first result
  ///
  /// Convenience method equivalent to `.results(f).first()`.
  pub fn first<F>(self, neighbors: F) -> Option<TraversalResult>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.execute(neighbors).next()
  }

  /// Execute and get the first node ID
  ///
  /// Convenience method equivalent to `.results(f).first_node()`.
  pub fn first_node<F>(self, neighbors: F) -> Option<NodeId>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.execute(neighbors).next().map(|r| r.node_id)
  }

  /// Execute and collect all node IDs into a Vec
  ///
  /// Convenience method equivalent to `.results(f).to_vec()`.
  pub fn to_vec<F>(self, neighbors: F) -> Vec<NodeId>
  where
    F: Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge>,
  {
    self.collect_node_ids(neighbors)
  }
}

// ============================================================================
// Extended Result Types with Properties
// ============================================================================

/// A node result with loaded properties
#[derive(Debug, Clone)]
pub struct NodeResult {
  /// Node ID
  pub id: NodeId,
  /// Node key (if available)
  pub key: Option<String>,
  /// Node properties
  pub props: HashMap<String, PropValue>,
}

impl NodeResult {
  /// Create a new NodeResult
  pub fn new(id: NodeId) -> Self {
    Self {
      id,
      key: None,
      props: HashMap::new(),
    }
  }

  /// Set the node key
  pub fn with_key(mut self, key: String) -> Self {
    self.key = Some(key);
    self
  }

  /// Set the node properties
  pub fn with_props(mut self, props: HashMap<String, PropValue>) -> Self {
    self.props = props;
    self
  }

  /// Get a property value by name
  pub fn get(&self, name: &str) -> Option<&PropValue> {
    self.props.get(name)
  }

  /// Get a string property
  pub fn string(&self, name: &str) -> Option<&str> {
    match self.props.get(name) {
      Some(PropValue::String(s)) => Some(s),
      _ => None,
    }
  }

  /// Get an integer property
  pub fn int(&self, name: &str) -> Option<i64> {
    match self.props.get(name) {
      Some(PropValue::I64(v)) => Some(*v),
      _ => None,
    }
  }

  /// Get a float property
  pub fn float(&self, name: &str) -> Option<f64> {
    match self.props.get(name) {
      Some(PropValue::F64(v)) => Some(*v),
      _ => None,
    }
  }

  /// Get a boolean property
  pub fn bool(&self, name: &str) -> Option<bool> {
    match self.props.get(name) {
      Some(PropValue::Bool(v)) => Some(*v),
      _ => None,
    }
  }
}

/// An edge result with loaded properties
#[derive(Debug, Clone)]
pub struct FullEdgeResult {
  /// Source node ID
  pub src: NodeId,
  /// Destination node ID
  pub dst: NodeId,
  /// Edge type ID
  pub etype: ETypeId,
  /// Edge properties
  pub props: HashMap<String, PropValue>,
}

impl FullEdgeResult {
  /// Create from a RawEdge
  pub fn from_raw(edge: RawEdge) -> Self {
    Self {
      src: edge.src,
      dst: edge.dst,
      etype: edge.etype,
      props: HashMap::new(),
    }
  }

  /// Set the edge properties
  pub fn with_props(mut self, props: HashMap<String, PropValue>) -> Self {
    self.props = props;
    self
  }

  /// Get a property value by name
  pub fn get(&self, name: &str) -> Option<&PropValue> {
    self.props.get(name)
  }
}

// ============================================================================
// Collecting Results with Properties
// ============================================================================

/// Options for collecting results with properties
#[derive(Debug, Clone, Default)]
pub struct CollectOptions {
  /// Property names to load for nodes (None = load all)
  pub node_props: Option<Vec<String>>,
  /// Property names to load for edges (None = load all)
  pub edge_props: Option<Vec<String>>,
  /// Whether to load node keys
  pub load_keys: bool,
}

impl CollectOptions {
  /// Create new options
  pub fn new() -> Self {
    Self::default()
  }

  /// Specify which node properties to load
  pub fn select_node_props(mut self, props: Vec<String>) -> Self {
    self.node_props = Some(props);
    self
  }

  /// Specify which edge properties to load
  pub fn select_edge_props(mut self, props: Vec<String>) -> Self {
    self.edge_props = Some(props);
    self
  }

  /// Enable loading node keys
  pub fn with_keys(mut self) -> Self {
    self.load_keys = true;
    self
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::HashSet;

  fn mock_graph() -> impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge> {
    // Create a simple graph:
    // 1 --knows--> 2 --knows--> 3
    // 1 --follows--> 4
    // 2 --follows--> 5
    const GRAPH: [Edge; 4] = [
      Edge {
        src: 1,
        etype: 1,
        dst: 2,
      },
      Edge {
        src: 1,
        etype: 2,
        dst: 4,
      },
      Edge {
        src: 2,
        etype: 1,
        dst: 3,
      },
      Edge {
        src: 2,
        etype: 2,
        dst: 5,
      },
    ];
    move |node_id: NodeId, direction: TraversalDirection, etype: Option<ETypeId>| {
      let edges = GRAPH
        .into_iter()
        .filter(move |edge| etype.is_none_or(|etype| edge.etype == etype));
      let out = edges.clone().filter(|edge| edge.src == node_id);
      let incoming = edges.filter(|edge| edge.dst == node_id);
      match direction {
        TraversalDirection::Out => out.collect(),
        TraversalDirection::In => incoming.collect(),
        TraversalDirection::Both => out.chain(incoming).collect(),
      }
    }
  }

  #[test]
  fn test_single_hop_out() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(Some(1)) // knows
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_single_hop_all_etypes() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None) // all edge types
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 2);
    let node_ids: HashSet<_> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&4));
  }

  #[test]
  fn test_two_hops() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(Some(1)) // 1 -> 2
      .out(Some(1)) // 2 -> 3
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_incoming() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(3)
      .r#in(Some(1)) // 3 <- 2
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_take_limit() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .take(1)
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn test_count() {
    let neighbors = mock_graph();

    let count = TraversalBuilder::from_node(1).out(None).count(&neighbors);

    assert_eq!(count, 2);
  }

  #[test]
  fn test_count_with_limit() {
    let neighbors = mock_graph();

    let count = TraversalBuilder::from_node(1)
      .out(None)
      .take(1)
      .count(&neighbors);

    assert_eq!(count, 1);
  }

  #[test]
  fn test_traverse_variable_depth() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(Some(1), TraverseOptions::new(TraversalDirection::Out, 2))
      .execute(&neighbors)
      .collect();

    // Should find: 2 (depth 1), 3 (depth 2)
    assert_eq!(results.len(), 2);
    let node_ids: HashSet<_> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_traverse_min_depth() {
    let neighbors = mock_graph();

    let options = TraverseOptions::new(TraversalDirection::Out, 2).with_min_depth(2);

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(Some(1), options)
      .execute(&neighbors)
      .collect();

    // Should only find: 3 (depth 2, skipping depth 1)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_multiple_start_nodes() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::new(vec![1, 2])
      .out(Some(1))
      .execute(&neighbors)
      .collect();

    // From 1: finds 2
    // From 2: finds 3
    // But 2 is already visited, so only 3 is new
    // Wait - start nodes are marked visited, so 2 from node 1 won't be yielded
    // Actually the implementation marks start nodes as visited
    // Let me check... yes, start nodes are visited, so 2 won't be yielded from 1
    // Result should be: 2 (from node 1), 3 (from node 2)
    // Hmm, but 2 is a start node so it's visited... Let me re-check
    // Actually start nodes 1 and 2 are visited, then:
    // - From 1, we find 2, but 2 is already visited, skip
    // - From 2, we find 3, which is not visited, yield
    // So only 1 result
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_unique_false() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::new(vec![1, 2])
      .unique(false)
      .out(Some(1))
      .execute(&neighbors)
      .collect();

    // Without uniqueness, we get all results:
    // From 1: 2
    // From 2: 3
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn test_collect_node_ids() {
    let neighbors = mock_graph();

    let node_ids = TraversalBuilder::from_node(1)
      .out(Some(1))
      .out(Some(1))
      .collect_node_ids(&neighbors);

    assert_eq!(node_ids, vec![3]);
  }

  #[test]
  fn test_empty_result() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(999)
      .out(None)
      .execute(&neighbors)
      .collect();

    assert!(results.is_empty());
  }

  #[test]
  fn test_no_steps() {
    let neighbors = mock_graph();

    // With no steps, should just yield start nodes
    // But wait, the implementation doesn't yield start nodes unless there are steps
    // Actually looking at the iterator, if step_index >= steps.len() and frontier not empty,
    // it yields from frontier. So start nodes should be yielded.
    let results: Vec<_> = TraversalBuilder::from_node(1).execute(&neighbors).collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 1);
  }

  // ============================================================================
  // Filter Predicate Tests
  // ============================================================================

  #[test]
  fn test_where_edge_filter_by_etype() {
    let neighbors = mock_graph();

    // Filter to only include edges with etype == 1 (knows)
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None) // all edge types
      .where_edge(|edge| edge.etype == 1)
      .execute(&neighbors)
      .collect();

    // Should only find node 2 (via knows edge), not node 4 (via follows edge)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_where_edge_filter_by_dst() {
    let neighbors = mock_graph();

    // Filter to only include edges where dst > 3
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .where_edge(|edge| edge.dst > 3)
      .execute(&neighbors)
      .collect();

    // Should only find node 4 (dst=4)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 4);
  }

  #[test]
  fn test_where_node_filter() {
    let neighbors = mock_graph();

    // Filter to only include nodes with id > 3
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .where_node(|node| node.id > 3)
      .execute(&neighbors)
      .collect();

    // Should only find node 4
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 4);
  }

  #[test]
  fn test_where_node_filter_excludes_all() {
    let neighbors = mock_graph();

    // Filter that excludes all nodes
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .where_node(|node| node.id > 100)
      .execute(&neighbors)
      .collect();

    assert!(results.is_empty());
  }

  #[test]
  fn test_combined_edge_and_node_filters() {
    let neighbors = mock_graph();

    // Combine edge and node filters
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .where_edge(|edge| edge.etype == 2) // follows only
      .where_node(|node| node.id >= 4)
      .execute(&neighbors)
      .collect();

    // Should find node 4 (follows edge with dst >= 4)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 4);
  }

  #[test]
  fn test_filter_with_limit() {
    let neighbors = mock_graph();

    // From node 2, we can reach nodes 3 and 5 (knows->3, follows->5)
    // Filter to etype 1 only, but also with limit
    let results: Vec<_> = TraversalBuilder::from_node(2)
      .out(None)
      .where_edge(|edge| edge.etype == 1)
      .take(10)
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
  }

  #[test]
  fn test_has_filters_detects_global_edge_filter() {
    let builder = TraversalBuilder::from_node(1)
      .out(None)
      .where_edge(|_| true);

    assert!(builder.has_filters());
  }

  #[test]
  fn test_has_filters_detects_global_node_filter() {
    let builder = TraversalBuilder::from_node(1)
      .out(None)
      .where_node(|_| true);

    assert!(builder.has_filters());
  }

  #[test]
  fn test_has_filters_false_when_no_filters() {
    let builder = TraversalBuilder::from_node(1).out(None);

    assert!(!builder.has_filters());
  }

  #[test]
  fn test_count_falls_back_to_iteration_with_filters() {
    let neighbors = mock_graph();

    // With filters, count should use iteration (slow path)
    let builder = TraversalBuilder::from_node(1)
      .out(None)
      .where_edge(|edge| edge.etype == 1);

    // Can't use fast count
    assert!(!builder.can_use_fast_count());

    // But count still works
    let count = builder.count(&neighbors);
    assert_eq!(count, 1);
  }

  #[test]
  fn test_fast_count_matches_iteration_for_every_single_hop_config() {
    // Cycle 1<->2, parallel 1->2 edges of two types, 2->3 and a self-loop on 3.
    let graph = [
      Edge {
        src: 1,
        etype: 1,
        dst: 2,
      },
      Edge {
        src: 1,
        etype: 2,
        dst: 2,
      },
      Edge {
        src: 2,
        etype: 1,
        dst: 1,
      },
      Edge {
        src: 2,
        etype: 1,
        dst: 3,
      },
      Edge {
        src: 3,
        etype: 1,
        dst: 3,
      },
    ];
    let neighbors = |node_id: NodeId, direction: TraversalDirection, etype: Option<ETypeId>| {
      graph
        .iter()
        .copied()
        .filter(|edge| etype.is_none_or(|etype| edge.etype == etype))
        .filter(|edge| match direction {
          TraversalDirection::Out => edge.src == node_id,
          TraversalDirection::In => edge.dst == node_id,
          TraversalDirection::Both => edge.src == node_id || edge.dst == node_id,
        })
        .collect::<Vec<_>>()
    };
    let directions = [
      TraversalDirection::Out,
      TraversalDirection::In,
      TraversalDirection::Both,
    ];

    for start in [vec![1], vec![1, 2], vec![2, 2], vec![3, 1, 3]] {
      for unique in [true, false] {
        for limit in [None, Some(1), Some(3)] {
          for etype in [None, Some(1)] {
            for step_count in 0..=3u32 {
              // Every direction sequence of this length.
              for sequence in 0..directions.len().pow(step_count) {
                let mut builder = TraversalBuilder::new(start.clone()).unique(unique);
                let mut path = Vec::new();
                let mut digits = sequence;
                for _ in 0..step_count {
                  let direction = directions[digits % directions.len()];
                  digits /= directions.len();
                  path.push(direction);
                  builder = match direction {
                    TraversalDirection::Out => builder.out(etype),
                    TraversalDirection::In => builder.r#in(etype),
                    TraversalDirection::Both => builder.both(etype),
                  };
                }
                if let Some(limit) = limit {
                  builder = builder.take(limit);
                }

                assert!(builder.can_use_fast_count());
                let expected = builder.clone().collect_node_ids(neighbors);
                assert_eq!(
                  builder.count(neighbors),
                  expected.len(),
                  "start={start:?} unique={unique} limit={limit:?} etype={etype:?} \
                   steps={path:?}: iterator yields {expected:?}"
                );
              }
            }
          }
        }
      }
    }
  }

  #[test]
  fn test_traverse_with_edge_filter() {
    let neighbors = mock_graph();

    // Variable-depth traversal with edge filter in options
    let options =
      TraverseOptions::new(TraversalDirection::Out, 2).with_edge_filter(|edge| edge.etype == 1);

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(Some(1), options)
      .execute(&neighbors)
      .collect();

    // Should find: 2 (depth 1), 3 (depth 2) - all via knows edges
    assert_eq!(results.len(), 2);
    let node_ids: HashSet<_> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_traverse_with_node_filter() {
    let neighbors = mock_graph();

    // Variable-depth traversal with node filter in options
    // Filter only yields nodes with id >= 2 (which is all reachable nodes)
    let options =
      TraverseOptions::new(TraversalDirection::Out, 2).with_node_filter(|node| node.id >= 2);

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(Some(1), options)
      .execute(&neighbors)
      .collect();

    // Should find: 2 (depth 1), 3 (depth 2)
    assert_eq!(results.len(), 2);
    let node_ids: HashSet<_> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_traverse_with_node_filter_excludes_intermediate() {
    let neighbors = mock_graph();

    // Filter for id >= 3 - this will filter out node 2, so we can't reach node 3
    // because the traversal stops at filtered nodes
    let options =
      TraverseOptions::new(TraversalDirection::Out, 3).with_node_filter(|node| node.id >= 3);

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(Some(1), options)
      .execute(&neighbors)
      .collect();

    // Node 2 is filtered out, so we can't traverse through it to reach node 3
    // This demonstrates that node filters affect traversal continuation
    assert!(results.is_empty());
  }

  #[test]
  fn test_traverse_options_with_combined_filters() {
    let neighbors = mock_graph();

    // Variable-depth traversal with both filters
    let options = TraverseOptions::new(TraversalDirection::Out, 3)
      .with_edge_filter(|edge| edge.etype == 1)
      .with_node_filter(|node| node.id >= 2);

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .traverse(None, options)
      .execute(&neighbors)
      .collect();

    // Should find nodes 2 and 3 via knows edges (etype=1)
    let node_ids: HashSet<_> = results.iter().map(|r| r.node_id).collect();
    assert!(node_ids.contains(&2));
    assert!(node_ids.contains(&3));
  }

  #[test]
  fn test_edge_info_from_raw_edge() {
    let raw_edge = RawEdge {
      src: 1,
      dst: 2,
      etype: 3,
    };

    let edge_info = EdgeInfo::from(raw_edge);

    assert_eq!(edge_info.src, 1);
    assert_eq!(edge_info.dst, 2);
    assert_eq!(edge_info.etype, 3);
    assert!(edge_info.props.is_empty());
  }

  // ============================================================================
  // Result Accessor Tests
  // ============================================================================

  #[test]
  fn test_results_to_vec() {
    let neighbors = mock_graph();

    let nodes = TraversalBuilder::from_node(1)
      .out(Some(1))
      .results(&neighbors)
      .to_vec();

    assert_eq!(nodes, vec![2]);
  }

  #[test]
  fn test_results_first() {
    let neighbors = mock_graph();

    let first = TraversalBuilder::from_node(1)
      .out(Some(1))
      .results(&neighbors)
      .first();

    assert!(first.is_some());
    assert_eq!(first.expect("expected value").node_id, 2);
  }

  #[test]
  fn test_results_first_node() {
    let neighbors = mock_graph();

    let first = TraversalBuilder::from_node(1)
      .out(Some(1))
      .results(&neighbors)
      .first_node();

    assert_eq!(first, Some(2));
  }

  #[test]
  fn test_results_first_empty() {
    let neighbors = mock_graph();

    let first = TraversalBuilder::from_node(999)
      .out(None)
      .results(&neighbors)
      .first();

    assert!(first.is_none());
  }

  #[test]
  fn test_results_count() {
    let neighbors = mock_graph();

    let count = TraversalBuilder::from_node(1)
      .out(None)
      .results(&neighbors)
      .count();

    assert_eq!(count, 2);
  }

  #[test]
  fn test_results_nodes_iterator() {
    let neighbors = mock_graph();

    let nodes: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .results(&neighbors)
      .nodes()
      .collect();

    assert_eq!(nodes.len(), 2);
    assert!(nodes.contains(&2));
    assert!(nodes.contains(&4));
  }

  #[test]
  fn test_results_edges_iterator() {
    let neighbors = mock_graph();

    let edges: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .results(&neighbors)
      .edges()
      .collect();

    assert_eq!(edges.len(), 2);
    // All edges should have src=1
    for edge in &edges {
      assert_eq!(edge.src, 1);
    }
  }

  #[test]
  fn test_results_full_iterator() {
    let neighbors = mock_graph();

    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(Some(1))
      .results(&neighbors)
      .full()
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
    assert!(results[0].edge.is_some());
    assert_eq!(results[0].depth, 1);
  }

  #[test]
  fn test_builder_first_method() {
    let neighbors = mock_graph();

    let first = TraversalBuilder::from_node(1)
      .out(Some(1))
      .first(&neighbors);

    assert!(first.is_some());
    assert_eq!(first.expect("expected value").node_id, 2);
  }

  #[test]
  fn test_builder_first_node_method() {
    let neighbors = mock_graph();

    let first = TraversalBuilder::from_node(1)
      .out(Some(1))
      .first_node(&neighbors);

    assert_eq!(first, Some(2));
  }

  #[test]
  fn test_builder_to_vec_method() {
    let neighbors = mock_graph();

    let nodes = TraversalBuilder::from_node(1).out(None).to_vec(&neighbors);

    assert_eq!(nodes.len(), 2);
    assert!(nodes.contains(&2));
    assert!(nodes.contains(&4));
  }

  #[test]
  fn test_node_result_accessors() {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String("Alice".to_string()));
    props.insert("age".to_string(), PropValue::I64(30));
    props.insert("score".to_string(), PropValue::F64(95.5));
    props.insert("active".to_string(), PropValue::Bool(true));

    let result = NodeResult::new(1)
      .with_key("user:alice".to_string())
      .with_props(props);

    assert_eq!(result.id, 1);
    assert_eq!(result.key, Some("user:alice".to_string()));
    assert_eq!(result.string("name"), Some("Alice"));
    assert_eq!(result.int("age"), Some(30));
    assert_eq!(result.float("score"), Some(95.5));
    assert_eq!(result.bool("active"), Some(true));
    assert_eq!(result.string("missing"), None);
  }

  #[test]
  fn test_full_edge_result() {
    let raw = RawEdge {
      src: 1,
      dst: 2,
      etype: 3,
    };

    let mut props = HashMap::new();
    props.insert("weight".to_string(), PropValue::F64(0.5));

    let edge = FullEdgeResult::from_raw(raw).with_props(props);

    assert_eq!(edge.src, 1);
    assert_eq!(edge.dst, 2);
    assert_eq!(edge.etype, 3);
    assert!(edge.get("weight").is_some());
  }

  #[test]
  fn test_two_hop_results() {
    let neighbors = mock_graph();

    // 1 -> 2 -> 3
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(Some(1))
      .out(Some(1))
      .results(&neighbors)
      .full()
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 3);
    assert_eq!(results[0].depth, 2);
  }

  #[test]
  fn test_collect_options() {
    let opts = CollectOptions::new()
      .select_node_props(vec!["name".to_string(), "age".to_string()])
      .select_edge_props(vec!["weight".to_string()])
      .with_keys();

    assert!(opts.node_props.is_some());
    assert_eq!(opts.node_props.as_ref().expect("expected value").len(), 2);
    assert!(opts.edge_props.is_some());
    assert!(opts.load_keys);
  }

  // ============================================================================
  // Select Property Tests
  // ============================================================================

  #[test]
  fn test_select_stores_properties() {
    let builder = TraversalBuilder::from_node(1)
      .out(Some(1))
      .select(vec!["name".to_string(), "age".to_string()]);

    let selected = builder.selected_properties();
    assert!(selected.is_some());
    let props = selected.expect("expected value");
    assert_eq!(props.len(), 2);
    assert!(props.contains(&"name".to_string()));
    assert!(props.contains(&"age".to_string()));
  }

  #[test]
  fn test_select_props_with_str_slices() {
    let builder = TraversalBuilder::from_node(1)
      .out(Some(1))
      .select_props(&["name", "email"]);

    let selected = builder.selected_properties();
    assert!(selected.is_some());
    let props = selected.expect("expected value");
    assert_eq!(props.len(), 2);
    assert!(props.contains(&"name".to_string()));
    assert!(props.contains(&"email".to_string()));
  }

  #[test]
  fn test_select_no_properties_by_default() {
    let builder = TraversalBuilder::from_node(1).out(Some(1));

    assert!(builder.selected_properties().is_none());
  }

  #[test]
  fn test_collect_options_from_builder() {
    let builder = TraversalBuilder::from_node(1)
      .out(Some(1))
      .select_props(&["name", "age"]);

    let opts = builder.collect_options();

    assert!(opts.node_props.is_some());
    let props = opts.node_props.expect("expected value");
    assert_eq!(props.len(), 2);
    assert!(props.contains(&"name".to_string()));
    assert!(props.contains(&"age".to_string()));
  }

  #[test]
  fn test_collect_options_empty_without_select() {
    let builder = TraversalBuilder::from_node(1).out(Some(1));

    let opts = builder.collect_options();

    // No props selected means load all (None)
    assert!(opts.node_props.is_none());
  }

  #[test]
  fn test_select_does_not_affect_execution() {
    let neighbors = mock_graph();

    // Select should not affect traversal execution, only property loading
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(Some(1))
      .select_props(&["name", "age"])
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }

  #[test]
  fn test_select_with_take() {
    let neighbors = mock_graph();

    // Select combined with take should work
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .select_props(&["name"])
      .take(1)
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
  }

  #[test]
  fn test_select_with_filters() {
    let neighbors = mock_graph();

    // Select combined with filters
    let results: Vec<_> = TraversalBuilder::from_node(1)
      .out(None)
      .select_props(&["name"])
      .where_edge(|e| e.etype == 1)
      .execute(&neighbors)
      .collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, 2);
  }
}
