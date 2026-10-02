//! Reproductions for the wave-3 API findings (`api/kite.rs`, `api/traversal.rs`,
//! `api/pathfinding.rs`, `util/heap.rs`). Each test states the contract its fix must meet.
//! The perf findings (A1's fsyncs, A7, A8, A16) are measured by the ignored `perf_*` tests at the
//! end; the hygiene finding (A15) has no test.
//!
//! Unit tests that need private access live in `src/api/kite.rs` (`w3_a13_*`, `w3_a14_*`).

use kitedb::api::kite::{BatchOp, EdgeDef, Kite, KiteOptions, NodeDef, NodeRef, PropDef};
use kitedb::api::pathfinding::{a_star, dijkstra, yen_k_shortest, PathConfig, PathResult};
use kitedb::api::traversal::{RawEdge, TraversalBuilder, TraversalDirection, TraverseOptions};
use kitedb::types::{ETypeId, Edge, NodeId, PropValue};
use kitedb::util::heap::IndexedMinHeap;
use kitedb::KiteError;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use tempfile::TempDir;

type Props = HashMap<String, PropValue>;

const MISSING_NODE: NodeId = 999_999;

fn open(options: KiteOptions) -> (TempDir, Kite) {
  let dir = tempfile::tempdir().expect("tempdir");
  let kite = Kite::open(dir.path().join("w3-api.kitedb"), options).expect("open kite");
  (dir, kite)
}

fn props(entries: &[(&str, PropValue)]) -> Props {
  entries
    .iter()
    .map(|(name, value)| (name.to_string(), value.clone()))
    .collect()
}

fn s(value: &str) -> PropValue {
  PropValue::String(value.to_string())
}

/// `User` nodes with a `name` prop; `F` edges with a `weight` prop; `RATED` edges with an int
/// `stars` prop.
fn schema() -> KiteOptions {
  KiteOptions::new()
    .node(NodeDef::new("User", "user:").prop(PropDef::string("name")))
    .edge(EdgeDef::new("F").prop(PropDef::float("weight")))
    .edge(EdgeDef::new("RATED").prop(PropDef::int("stars")))
}

fn user(kite: &mut Kite, key: &str) -> NodeId {
  kite
    .create_node("User", key, Props::new())
    .expect("create user")
    .id()
}

fn assert_no_failures(finding: &str, failures: &[String]) {
  assert!(
    failures.is_empty(),
    "{finding}: {} case(s) violate the contract:\n  {}",
    failures.len(),
    failures.join("\n  ")
  );
}

/// `(path, total_weight)` of a found path, `None` if not found.
fn found_path(result: &PathResult) -> Option<(Vec<NodeId>, f64)> {
  result
    .found
    .then(|| (result.path.clone(), result.total_weight))
}

// Node ids for the pure pathfinding/traversal graphs.
const S: NodeId = 1;
const A: NodeId = 2;
const B: NodeId = 3;
const C: NodeId = 4;
const T: NodeId = 5;
const T2: NodeId = 6;
const T3: NodeId = 7;

/// Edge type of every edge built by [`Graph::weighted`].
const E: ETypeId = 1;

/// A static graph for the pure traversal and pathfinding functions.
struct Graph {
  edges: Vec<(Edge, f64)>,
}

impl Graph {
  /// Edges `(src, dst, weight)`, all of type [`E`].
  fn weighted(edges: &[(NodeId, NodeId, f64)]) -> Self {
    let edges = edges
      .iter()
      .map(|&(src, dst, weight)| (Edge { src, etype: E, dst }, weight))
      .collect();
    Self { edges }
  }

  /// Edges `(src, etype, dst)`, all of weight 1.
  fn typed(edges: &[(NodeId, ETypeId, NodeId)]) -> Self {
    let edges = edges
      .iter()
      .map(|&(src, etype, dst)| (Edge { src, etype, dst }, 1.0))
      .collect();
    Self { edges }
  }

  /// Neighbors like `Kite`'s: `Both` is the out edges followed by the in edges.
  fn neighbors(&self) -> impl Fn(NodeId, TraversalDirection, Option<ETypeId>) -> Vec<Edge> + '_ {
    move |node_id, direction, etype| {
      let edges = self
        .edges
        .iter()
        .map(|(edge, _)| *edge)
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

  fn weight(&self) -> impl Fn(NodeId, ETypeId, NodeId) -> f64 + '_ {
    move |src, etype, dst| {
      self
        .edges
        .iter()
        .find(|(edge, _)| (edge.src, edge.etype, edge.dst) == (src, etype, dst))
        .map(|&(_, weight)| weight)
        .expect("weight of a graph edge")
    }
  }
}

// ============================================================================
// A1: update builders must not commit a write tx just to check existence
// ============================================================================

#[test]
fn a1_update_builders_write_nothing_before_execute() {
  let (_dir, mut kite) = open(schema());
  let alice = kite
    .create_node("User", "alice", Props::new())
    .expect("create alice");
  let mut failures = Vec::new();

  let mut check = |label: &str, kite: &mut Kite, start: fn(&mut Kite, &NodeRef)| {
    let before = kite.stats().wal_bytes;
    start(kite, &alice);
    let after = kite.stats().wal_bytes;
    if after != before {
      failures.push(format!(
        "{label}: building (not executing) an update grew the WAL by {} bytes",
        after.saturating_sub(before)
      ));
    }
  };
  check("update(&node_ref)", &mut kite, |kite, node| {
    drop(kite.update(node).expect("update"));
  });
  check("update_by_id", &mut kite, |kite, node| {
    drop(kite.update_by_id(node.id()).expect("update_by_id"));
  });
  check("update_by_key", &mut kite, |kite, _| {
    drop(kite.update_by_key("User", "alice").expect("update_by_key"));
  });

  assert_no_failures("A1", &failures);
}

// ============================================================================
// A2: overlapping or empty key prefixes must not make node types ambiguous
// ============================================================================

/// Open `options`; `None` if open rejected the schema (an accepted fix for A2).
fn open_or_rejected(options: KiteOptions) -> Option<(TempDir, Kite)> {
  let dir = tempfile::tempdir().expect("tempdir");
  match Kite::open(dir.path().join("w3-api.kitedb"), options) {
    Ok(kite) => Some((dir, kite)),
    Err(KiteError::InvalidSchema(_) | KiteError::SchemaViolation(_)) => None,
    Err(err) => panic!("open failed for a reason other than the schema: {err}"),
  }
}

/// Every node in `typed` must be listed, counted and identified as its own type only.
fn check_node_types(kite: &Kite, typed: &[(&str, NodeId)], failures: &mut Vec<String>) {
  for &(node_type, _) in typed {
    let mut expected: Vec<NodeId> = typed
      .iter()
      .filter(|(other, _)| *other == node_type)
      .map(|&(_, id)| id)
      .collect();
    expected.sort_unstable();
    let mut listed: Vec<NodeId> = kite
      .all(node_type)
      .expect("all")
      .map(|node| node.id())
      .collect();
    listed.sort_unstable();
    if listed != expected {
      failures.push(format!(
        "all({node_type:?}) = {listed:?}, expected {expected:?}"
      ));
    }
    let counted = kite.count_nodes_by_type(node_type).expect("count");
    if counted != expected.len() as u64 {
      failures.push(format!(
        "count_nodes_by_type({node_type:?}) = {counted}, expected {}",
        expected.len()
      ));
    }
  }
  for &(node_type, id) in typed {
    let node = kite
      .node_by_id(id)
      .expect("node_by_id")
      .expect("node exists");
    if node.node_type() != node_type {
      failures.push(format!(
        "node_by_id({id}) (key {:?}) has type {:?}, expected {node_type:?}",
        node.key(),
        node.node_type()
      ));
    }
  }
}

#[test]
fn a2_overlapping_key_prefixes_rejected_or_disambiguated() {
  let options = KiteOptions::new()
    .node(NodeDef::new("User", "user:"))
    .node(NodeDef::new("Admin", "user:admin:"));
  let Some((_dir, mut kite)) = open_or_rejected(options) else {
    return;
  };
  let alice = user(&mut kite, "alice");
  let root = kite
    .create_node("Admin", "root", Props::new())
    .expect("create admin")
    .id();

  let mut failures = Vec::new();
  check_node_types(&kite, &[("User", alice), ("Admin", root)], &mut failures);
  assert_no_failures("A2 (prefixes \"user:\" and \"user:admin:\")", &failures);
}

#[test]
fn a2_type_cannot_create_or_get_a_key_a_longer_prefix_owns() {
  let options = KiteOptions::new()
    .node(NodeDef::new("User", "user:"))
    .node(NodeDef::new("Admin", "user:admin:"));
  let (_dir, mut kite) = open(options);
  let root = kite
    .create_node("Admin", "root", Props::new())
    .expect("create admin")
    .id();

  // "user:" + "admin:x" is in Admin's key space: as a User it would resolve to Admin.
  let created = kite.create_node("User", "admin:x", Props::new());
  assert!(
    matches!(created, Err(KiteError::InvalidSchema(_))),
    "a User keyed into Admin's key space must be refused, got {created:?}"
  );
  assert!(kite.get("User", "admin:root").expect("get").is_none());
  assert_eq!(
    kite
      .get("Admin", "root")
      .expect("get")
      .map(|node| node.id()),
    Some(root)
  );
}

#[test]
fn a2_empty_key_prefix_rejected_or_disambiguated() {
  let options = KiteOptions::new()
    .node(NodeDef::new("User", "user:"))
    .node(NodeDef::new("Thing", ""));
  let Some((_dir, mut kite)) = open_or_rejected(options) else {
    return;
  };
  let alice = user(&mut kite, "alice");
  let thing = kite
    .create_node("Thing", "t1", Props::new())
    .expect("create thing")
    .id();

  let mut failures = Vec::new();
  check_node_types(&kite, &[("User", alice), ("Thing", thing)], &mut failures);
  assert_no_failures("A2 (prefixes \"user:\" and \"\")", &failures);
}

// ============================================================================
// A3: a max_depth search must not lose a shallower, costlier path
// ============================================================================

/// S->A costs 10 in one hop; S->B->C->A costs 3 in three. Under max_depth 3 only S->A->T
/// (cost 11, depth 2) reaches T.
const DEPTH_TRAP: [(NodeId, NodeId, f64); 5] = [
  (S, A, 10.0),
  (S, B, 1.0),
  (B, C, 1.0),
  (C, A, 1.0),
  (A, T, 1.0),
];

#[test]
fn a3_dijkstra_max_depth_finds_shallower_costlier_path() {
  let graph = Graph::weighted(&DEPTH_TRAP);
  let result = dijkstra(
    PathConfig::new(S, T).max_depth(3),
    graph.neighbors(),
    graph.weight(),
  );
  assert_eq!(
    found_path(&result),
    Some((vec![S, A, T], 11.0)),
    "A3: dijkstra(max_depth 3) must find S->A->T; reaching A cheaply at depth 3 must not hide A at depth 1"
  );
}

#[test]
fn a3_a_star_max_depth_finds_shallower_costlier_path() {
  let graph = Graph::weighted(&DEPTH_TRAP);
  let result = a_star(
    PathConfig::new(S, T).max_depth(3),
    graph.neighbors(),
    graph.weight(),
    |_, _| 0.0,
  );
  assert_eq!(
    found_path(&result),
    Some((vec![S, A, T], 11.0)),
    "A3: a_star(max_depth 3) must find S->A->T"
  );
}

#[test]
fn a3_k_shortest_spur_search_honors_max_depth() {
  // S->T is the shortest path; the second one comes from the spur search at S, which must avoid
  // the depth trap.
  let mut edges = DEPTH_TRAP.to_vec();
  edges.push((S, T, 0.5));
  let graph = Graph::weighted(&edges);

  let paths = yen_k_shortest(
    PathConfig::new(S, T).max_depth(3),
    2,
    graph.neighbors(),
    graph.weight(),
  );
  let found: Vec<_> = paths
    .iter()
    .map(|path| (path.path.clone(), path.total_weight))
    .collect();
  assert_eq!(
    found,
    vec![(vec![S, T], 0.5), (vec![S, A, T], 11.0)],
    "A3: yen_k_shortest(k 2, max_depth 3) must find S->A->T as the second path"
  );
}

// ============================================================================
// A4: invalid edge weights are skipped, and the heap is NaN-safe
// ============================================================================

#[test]
fn a4_indexed_heap_pops_every_key_in_total_order() {
  let mut heap = IndexedMinHeap::new();
  heap.insert(1u64, 3.0);
  heap.insert(2, f64::NAN);
  heap.insert(3, 1.0);
  heap.insert(4, f64::INFINITY);
  heap.insert(5, 2.0);

  let popped: Vec<u64> = std::iter::from_fn(|| heap.extract_min()).collect();
  assert_eq!(
    popped,
    vec![3, 5, 1, 4, 2],
    "A4: extract_min must pop every key in f64::total_cmp order (inf, then NaN, last)"
  );
}

#[test]
fn a4_dijkstra_skips_nan_weight_edge() {
  // S->A has a NaN weight; S->B->A is a valid detour.
  let graph = Graph::weighted(&[(S, A, f64::NAN), (S, B, 1.0), (B, A, 1.0), (A, T, 1.0)]);
  let result = dijkstra(PathConfig::new(S, T), graph.neighbors(), graph.weight());
  assert_eq!(
    found_path(&result),
    Some((vec![S, B, A, T], 3.0)),
    "A4: a NaN edge must be skipped, not block the detour through B"
  );
}

#[test]
fn a4_a_star_skips_nan_weight_edge() {
  let graph = Graph::weighted(&[(S, A, f64::NAN), (S, B, 1.0), (B, A, 1.0), (A, T, 1.0)]);
  let result = a_star(
    PathConfig::new(S, T),
    graph.neighbors(),
    graph.weight(),
    |_, _| 0.0,
  );
  assert_eq!(
    found_path(&result),
    Some((vec![S, B, A, T], 3.0)),
    "A4: a NaN edge must be skipped, not block the detour through B"
  );
}

#[test]
fn a4_dijkstra_skips_negative_weight_edge() {
  // A->B has a negative weight; without it the only path is S->B->T.
  let graph = Graph::weighted(&[(S, A, 1.0), (A, B, -1.0), (S, B, 5.0), (B, T, 1.0)]);
  let result = dijkstra(PathConfig::new(S, T), graph.neighbors(), graph.weight());
  assert_eq!(
    found_path(&result),
    Some((vec![S, B, T], 6.0)),
    "A4: a negative edge must be skipped (Dijkstra is wrong on negative weights)"
  );
}

// ============================================================================
// A5: every node-creation path stores the same metadata (key + type label)
// ============================================================================

#[test]
fn a5_every_create_path_labels_the_node() {
  let (_dir, mut kite) = open(schema());
  let label = kite
    .node_def("User")
    .and_then(|def| def.label_id)
    .expect("User label");
  let mut created: Vec<(&str, NodeId)> = Vec::new();

  let node = kite.create_node("User", "c", Props::new()).expect("create");
  created.push(("create_node", node.id()));
  let node = kite
    .insert("User")
    .and_then(|insert| insert.values("i1", Props::new()))
    .and_then(|insert| insert.returning())
    .expect("insert");
  created.push(("insert().values()", node.id()));
  let nodes = kite
    .insert("User")
    .and_then(|insert| insert.values_many(vec![("i2", Props::new())]))
    .and_then(|insert| insert.returning())
    .expect("insert many");
  created.push(("insert().values_many()", nodes[0].id()));
  let node = kite
    .upsert("User")
    .and_then(|upsert| upsert.values("u1", Props::new()))
    .and_then(|upsert| upsert.returning())
    .expect("upsert");
  created.push(("upsert().values() (create)", node.id()));
  let nodes = kite
    .upsert("User")
    .and_then(|upsert| upsert.values_many(vec![("u2", Props::new())]))
    .and_then(|upsert| upsert.returning())
    .expect("upsert many");
  created.push(("upsert().values_many() (create)", nodes[0].id()));
  kite
    .upsert_by_id("User", 4242)
    .and_then(|upsert| upsert.set("name", s("x")).execute())
    .expect("upsert_by_id");
  created.push(("upsert_by_id (create)", 4242));

  let mut failures = Vec::new();
  for (path, id) in created {
    let labels = kite.raw().node_labels(id);
    if !labels.contains(&label) {
      failures.push(format!(
        "{path}: node {id} has labels {labels:?}, missing the User label {label}"
      ));
    }
  }
  assert_no_failures("A5", &failures);
}

#[test]
fn a5_upsert_by_id_node_is_typed() {
  let (_dir, mut kite) = open(schema());
  kite
    .upsert_by_id("User", 4242)
    .and_then(|upsert| upsert.set("name", s("x")).execute())
    .expect("upsert_by_id");

  let mut failures = Vec::new();
  check_node_types(&kite, &[("User", 4242)], &mut failures);
  assert_no_failures("A5 (upsert_by_id)", &failures);
}

// ============================================================================
// A6: KitePathBuilder can weight edges; has_path takes &self
// ============================================================================

#[test]
fn a6_path_builder_weights_edges_by_prop() {
  let (_dir, mut kite) = open(schema());
  let [s_, a, b, c, t] = ["s", "a", "b", "c", "t"].map(|key| user(&mut kite, key));
  for (src, dst, weight) in [
    (s_, a, 10.0),
    (a, t, 10.0),
    (s_, b, 1.0),
    (b, c, 1.0),
    (c, t, 1.0),
  ] {
    kite
      .link_with_props(src, "F", dst, props(&[("weight", PropValue::F64(weight))]))
      .expect("link");
  }

  let result = kite
    .shortest_path(s_, t)
    .via("F")
    .expect("via")
    .weight_by_prop("weight")
    .find();
  assert_eq!(
    found_path(&result),
    Some((vec![s_, b, c, t], 3.0)),
    "A6: find() weighted by the `weight` edge prop must take the cheap 3-hop path"
  );
}

#[test]
fn a6_has_path_takes_shared_ref() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let b = user(&mut kite, "b");
  kite.link(a, "F", b).expect("link");

  let shared: &Kite = &kite;
  assert!(shared.has_path(a, b, Some("F")).expect("has_path"));
}

// ============================================================================
// A9: raw_edges() yields exactly the edges execute() yields
// ============================================================================

fn final_edges(builder: TraversalBuilder, graph: &Graph) -> Vec<RawEdge> {
  let mut edges: Vec<RawEdge> = builder
    .execute(graph.neighbors())
    .filter_map(|result| result.edge)
    .collect();
  edges.sort_unstable_by_key(|edge| (edge.src, edge.etype, edge.dst));
  edges
}

#[test]
fn a9_raw_edges_matches_execute_edges() {
  // 1 -1-> 2 -1-> 3, 1 -1-> 4, 1 -2-> 4
  let graph = Graph::typed(&[(1, 1, 2), (2, 1, 3), (1, 1, 4), (1, 2, 4)]);
  let cases: Vec<(&str, TraversalBuilder)> = vec![
    (
      "two hops",
      TraversalBuilder::from_node(1).out(Some(1)).out(Some(1)),
    ),
    (
      "take(1)",
      TraversalBuilder::from_node(1).out(Some(1)).take(1),
    ),
    (
      "unique (4 reached twice)",
      TraversalBuilder::from_node(1).out(None),
    ),
    (
      "where_edge",
      TraversalBuilder::from_node(1)
        .out(Some(1))
        .where_edge(|edge| edge.dst != 4),
    ),
    (
      "traverse()",
      TraversalBuilder::from_node(1)
        .traverse(Some(1), TraverseOptions::new(TraversalDirection::Out, 2)),
    ),
  ];

  let mut failures = Vec::new();
  for (label, builder) in cases {
    let expected = final_edges(builder.clone(), &graph);
    let raw = catch_unwind(AssertUnwindSafe(|| {
      builder.raw_edges(graph.neighbors()).collect::<Vec<_>>()
    }));
    match raw {
      Err(_) => failures.push(format!("{label}: raw_edges() panicked")),
      Ok(mut raw) => {
        raw.sort_unstable_by_key(|edge| (edge.src, edge.etype, edge.dst));
        if raw != expected {
          failures.push(format!(
            "{label}: raw_edges() = {raw:?}, execute() edges = {expected:?}"
          ));
        }
      }
    }
  }
  assert_no_failures("A9", &failures);
}

// ============================================================================
// A10: traversal filters see props; select() limits which node props they load
// ============================================================================

#[test]
fn a10_traverse_edge_filter_sees_edge_props() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let heavy = user(&mut kite, "heavy");
  let light = user(&mut kite, "light");
  kite
    .link_with_props(a, "F", heavy, props(&[("weight", PropValue::F64(2.0))]))
    .expect("link heavy");
  kite
    .link_with_props(a, "F", light, props(&[("weight", PropValue::F64(1.0))]))
    .expect("link light");

  let options = TraverseOptions::new(TraversalDirection::Out, 1)
    .with_edge_filter(|edge| edge.props.get("weight") == Some(&PropValue::F64(2.0)));
  let reached = kite
    .from(a)
    .traverse(Some("F"), options)
    .expect("traverse")
    .to_vec();
  assert_eq!(
    reached,
    vec![heavy],
    "A10: an edge filter on the `weight` prop must see the edge's props"
  );
}

#[test]
fn a10_traverse_node_filter_sees_node_props() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let keep = kite
    .create_node("User", "keep", props(&[("name", s("keep"))]))
    .expect("create keep")
    .id();
  let drop_ = kite
    .create_node("User", "drop", props(&[("name", s("drop"))]))
    .expect("create drop")
    .id();
  kite.link(a, "F", keep).expect("link keep");
  kite.link(a, "F", drop_).expect("link drop");

  let options = TraverseOptions::new(TraversalDirection::Out, 1)
    .with_node_filter(|node| node.props.get("name") == Some(&s("keep")));
  let reached = kite
    .from(a)
    .traverse(Some("F"), options)
    .expect("traverse")
    .to_vec();
  assert_eq!(
    reached,
    vec![keep],
    "A10: a node filter on the `name` prop must see the node's props"
  );
}

#[test]
fn a10_select_limits_props_loaded_for_filters() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let b = kite
    .create_node(
      "User",
      "b",
      props(&[("name", s("b")), ("age", PropValue::I64(30))]),
    )
    .expect("create b")
    .id();
  kite.link(a, "F", b).expect("link");

  let options = TraverseOptions::new(TraversalDirection::Out, 1).with_node_filter(|node| {
    node.props.get("name") == Some(&s("b")) && !node.props.contains_key("age")
  });
  let reached = kite
    .from(a)
    .select(&["name"])
    .traverse(Some("F"), options)
    .expect("traverse")
    .to_vec();
  assert_eq!(
    reached,
    vec![b],
    "A10: after select([\"name\"]), a node filter must see `name` and not `age`"
  );
}

#[test]
fn a10_global_where_edge_filters_results_not_hops() {
  // 1 -> 2 -> 3. The global filter tests each result's edge (2->3) only, as in the bindings
  // ("applied after traversal"); TraverseOptions filters are the per-hop ones.
  let graph = Graph::typed(&[(1, 1, 2), (2, 1, 3)]);
  let reached = TraversalBuilder::from_node(1)
    .out(None)
    .out(None)
    .where_edge(|edge| edge.src != 1)
    .collect_node_ids(graph.neighbors());
  assert_eq!(reached, vec![3]);
}

// ============================================================================
// A11: multi-target A* and k-shortest
// ============================================================================

#[test]
fn a11_multi_target_a_star_returns_cheapest_target() {
  // Targets on a line around S (x 0): T (x 0) costs 5, T2 (x 6) and T3 (x -6) cost 6.
  // h(n, t) = |x(n) - x(t)| is admissible for each target alone.
  let graph = Graph::weighted(&[(S, T, 5.0), (S, T2, 6.0), (S, T3, 6.0)]);
  let x = |node: NodeId| -> f64 {
    match node {
      T2 => 6.0,
      T3 => -6.0,
      _ => 0.0,
    }
  };

  // The heuristic's target is whichever the targets HashSet yields first, so repeat with fresh
  // sets.
  let mut wrong = Vec::new();
  for _ in 0..64 {
    let result = a_star(
      PathConfig::with_targets(S, [T, T2, T3]),
      graph.neighbors(),
      graph.weight(),
      |node, target| (x(node) - x(target)).abs(),
    );
    // Rejecting multiple targets (not found) is an accepted fix.
    if let Some(path) = found_path(&result).filter(|path| *path != (vec![S, T], 5.0)) {
      wrong.push(path);
    }
  }
  assert!(
    wrong.is_empty(),
    "A11: a_star to any of [T, T2, T3] returned a costlier path than S->T (5) in {}/64 runs, e.g. {:?}",
    wrong.len(),
    wrong.first()
  );
}

#[test]
fn a11_multi_target_k_shortest_ranks_paths_to_every_target() {
  // To any of {T, T2}: S->T (1), S->T2 (2), S->A->T (3).
  let graph = Graph::weighted(&[(S, T, 1.0), (S, T2, 2.0), (S, A, 1.5), (A, T, 1.5)]);
  let paths = yen_k_shortest(
    PathConfig::with_targets(S, [T, T2]),
    3,
    graph.neighbors(),
    graph.weight(),
  );
  let found: Vec<_> = paths
    .iter()
    .map(|path| (path.path.clone(), path.total_weight))
    .collect();
  // Rejecting multiple targets (no paths) is an accepted fix.
  assert!(
    found.is_empty() || found == vec![(vec![S, T], 1.0), (vec![S, T2], 2.0), (vec![S, A, T], 3.0)],
    "A11: k-shortest to any of [T, T2] must rank paths to both targets, got {found:?}"
  );
}

// ============================================================================
// A12: describe() prints correct totals
// ============================================================================

#[test]
fn a12_describe_reports_totals() {
  let (_dir, mut kite) = open(schema());
  let mut failures = Vec::new();
  let mut check = |kite: &Kite, stage: &str, nodes: &str, edges: &str| {
    let description = kite.describe();
    for line in [nodes, edges] {
      if !description.contains(line) {
        failures.push(format!("{stage}: expected {line:?} in:\n{description}"));
      }
    }
  };

  let ids: Vec<NodeId> = (0..10).map(|i| user(&mut kite, &format!("u{i}"))).collect();
  for pair in ids.windows(2).take(3) {
    kite.link(pair[0], "F", pair[1]).expect("link");
  }
  check(
    &kite,
    "fresh",
    "Nodes: 10 (snapshot: 0, delta: +10)",
    "Edges: 3 (snapshot: 0, delta: +3)",
  );

  kite.optimize().expect("optimize");
  check(
    &kite,
    "after optimize",
    "Nodes: 10 (snapshot: 10, delta: +0)",
    "Edges: 3 (snapshot: 3, delta: +0)",
  );

  let x = user(&mut kite, "x");
  user(&mut kite, "y");
  kite.link(ids[0], "F", x).expect("link");
  check(
    &kite,
    "after optimize + 2 nodes, 1 edge",
    "Nodes: 12 (snapshot: 10, delta: +2)",
    "Edges: 4 (snapshot: 3, delta: +1)",
  );

  assert_no_failures("A12", &failures);
}

// ============================================================================
// A13: a self-loop is one edge in both directions; depth inside traverse() continues the hop
// count
// ============================================================================

#[test]
fn a13_traverse_both_yields_self_loop_once() {
  let graph = Graph::typed(&[(1, 1, 1)]);
  let reached = TraversalBuilder::from_node(1)
    .unique(false)
    .traverse(
      None,
      TraverseOptions::new(TraversalDirection::Both, 1).with_unique(false),
    )
    .collect_node_ids(graph.neighbors());
  assert_eq!(
    reached,
    vec![1],
    "A13: one self-loop is one edge, so traverse(Both) must reach 1 once"
  );
}

#[test]
fn a13_traverse_depth_continues_hop_count() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let b = user(&mut kite, "b");
  let c = user(&mut kite, "c");
  kite.link(a, "F", b).expect("link a->b");
  kite.link(b, "F", c).expect("link b->c");

  let reached: Vec<(NodeId, usize)> = kite
    .from(a)
    .out(Some("F"))
    .and_then(|builder| {
      builder.traverse(Some("F"), TraverseOptions::new(TraversalDirection::Out, 1))
    })
    .expect("traverse")
    .execute()
    .map(|result| (result.node_id, result.depth))
    .collect();
  assert_eq!(
    reached,
    vec![(c, 2)],
    "A13: c is two hops from a, so traverse() after out() must report depth 2"
  );
}

// ============================================================================
// A14: transaction()/batch() inside an open tx must stay atomic
// ============================================================================

#[test]
fn a14_transaction_inside_open_tx_discards_writes_on_error() {
  let (_dir, mut kite) = open(schema());
  kite.raw().begin(false).expect("outer begin");
  let result: kitedb::Result<()> = kite.transaction(|ctx| {
    ctx.create_node("User", "alice", Props::new())?;
    Err(KiteError::Internal("closure failed".into()))
  });
  kite.raw().commit().expect("outer commit");

  assert!(result.is_err(), "the failing transaction returned Ok");
  assert!(
    kite.get("User", "alice").expect("get").is_none(),
    "A14: nested transaction() returned {result:?}, but its write survived the outer commit"
  );
}

#[test]
fn a14_batch_inside_open_tx_is_atomic() {
  let (_dir, mut kite) = open(schema());
  kite.raw().begin(false).expect("outer begin");
  let result = kite.batch(vec![
    BatchOp::CreateNode {
      node_type: "User".into(),
      key_suffix: "alice".into(),
      props: Props::new(),
    },
    BatchOp::CreateNode {
      node_type: "NoSuchType".into(),
      key_suffix: "bob".into(),
      props: Props::new(),
    },
  ]);
  kite.raw().commit().expect("outer commit");

  assert!(result.is_err(), "the failing batch returned Ok");
  assert!(
    kite.get("User", "alice").expect("get").is_none(),
    "A14: nested batch() failed with {:?}, but its first op survived the outer commit",
    result.err()
  );
}

// ============================================================================
// A17: strict_schema covers edge props; deleting a prop on a missing target errors
// ============================================================================

fn is_schema_violation<T>(result: &kitedb::Result<T>) -> bool {
  matches!(result, Err(KiteError::SchemaViolation(_)))
}

fn describe_result<T: std::fmt::Debug>(result: &kitedb::Result<T>) -> String {
  match result {
    Ok(value) => format!("Ok({value:?})"),
    Err(err) => format!("Err({err})"),
  }
}

#[test]
fn a17_strict_schema_checks_edge_prop_types() {
  let (_dir, mut kite) = open(schema().strict_schema(true));
  let a = user(&mut kite, "a");
  let bad = || props(&[("stars", s("five"))]);
  let mut fresh_dst = {
    let mut next = 0;
    move |kite: &mut Kite, linked: bool| {
      next += 1;
      let dst = user(kite, &format!("dst{next}"));
      if linked {
        kite.link(a, "RATED", dst).expect("link");
      }
      dst
    }
  };
  let mut failures = Vec::new();
  let mut check = |label: &str, result: kitedb::Result<()>| {
    if !is_schema_violation(&result) {
      failures.push(format!(
        "{label}: int prop `stars` set to a string gave {}",
        describe_result(&result)
      ));
    }
  };

  let dst = fresh_dst(&mut kite, false);
  check(
    "link_with_props",
    kite.link_with_props(a, "RATED", dst, bad()),
  );
  let dst = fresh_dst(&mut kite, true);
  check(
    "set_edge_prop",
    kite.set_edge_prop(a, "RATED", dst, "stars", s("five")),
  );
  let dst = fresh_dst(&mut kite, true);
  check(
    "set_edge_props",
    kite.set_edge_props(a, "RATED", dst, bad()),
  );
  let dst = fresh_dst(&mut kite, true);
  check(
    "update_edge().set()",
    kite
      .update_edge(a, "RATED", dst)
      .and_then(|update| update.set("stars", s("five")).execute()),
  );
  let dst = fresh_dst(&mut kite, false);
  check(
    "upsert_edge().set()",
    kite
      .upsert_edge(a, "RATED", dst)
      .and_then(|upsert| upsert.set("stars", s("five")).execute()),
  );
  let dst = fresh_dst(&mut kite, false);
  check(
    "batch LinkWithProps",
    kite
      .batch(vec![BatchOp::LinkWithProps {
        src: a,
        edge_type: "RATED".into(),
        dst,
        props: bad(),
      }])
      .map(drop),
  );
  let dst = fresh_dst(&mut kite, true);
  check(
    "batch SetEdgeProp",
    kite
      .batch(vec![BatchOp::SetEdgeProp {
        src: a,
        edge_type: "RATED".into(),
        dst,
        prop_name: "stars".into(),
        value: s("five"),
      }])
      .map(drop),
  );
  let dst = fresh_dst(&mut kite, true);
  check(
    "batch SetEdgeProps",
    kite
      .batch(vec![BatchOp::SetEdgeProps {
        src: a,
        edge_type: "RATED".into(),
        dst,
        props: bad(),
      }])
      .map(drop),
  );

  assert_no_failures("A17 (strict edge props)", &failures);
}

/// Core's delete_node_prop/delete_edge_prop get these existence checks in the core Phase 2 lane;
/// Kite adds no duplicate check. Un-ignore once that lands.
#[test]
#[ignore = "blocked on core: delete_node_prop/delete_edge_prop existence checks (core Phase 2)"]
fn a17_delete_prop_on_missing_target_errors() {
  let (_dir, mut kite) = open(schema());
  let a = user(&mut kite, "a");
  let b = user(&mut kite, "b");
  assert!(!kite.has_edge(a, "RATED", b).expect("has_edge"));
  let mut failures = Vec::new();

  let result = kite.del_edge_prop(a, "RATED", b, "stars");
  if !matches!(result, Err(KiteError::EdgeNotFound { .. })) {
    failures.push(format!(
      "del_edge_prop on a missing edge gave {}",
      describe_result(&result)
    ));
  }
  let result = kite.transaction(|ctx| ctx.del_prop(MISSING_NODE, "name"));
  if !matches!(result, Err(KiteError::NodeNotFound(MISSING_NODE))) {
    failures.push(format!(
      "TxContext::del_prop on a missing node gave {}",
      describe_result(&result)
    ));
  }
  let result = kite.batch(vec![BatchOp::DelProp {
    node_id: MISSING_NODE,
    prop_name: "name".into(),
  }]);
  if !matches!(result, Err(KiteError::NodeNotFound(MISSING_NODE))) {
    failures.push(format!(
      "batch DelProp on a missing node gave {}",
      describe_result(&result)
    ));
  }

  assert_no_failures("A17 (delete on missing target)", &failures);
}

// ============================================================================
// Perf measurements for A1, A7, A8 and A16 (timings only, no assertions). Run with:
//   cargo test --no-default-features --test w3_api perf_ -- --ignored --nocapture --test-threads=1
// ============================================================================

fn time_runs<T>(label: &str, runs: u32, mut run: impl FnMut() -> T) {
  let start = Instant::now();
  for _ in 0..runs {
    std::hint::black_box(run());
  }
  println!(
    "{label}: {:?} per run ({runs} runs)",
    start.elapsed() / runs
  );
}

/// `schema()` plus a `Post` type, without fsync, with room for one big setup transaction.
fn perf_schema() -> KiteOptions {
  schema()
    .node(NodeDef::new("Post", "post:").prop(PropDef::string("name")))
    .sync_off()
    .wal_size_mb(256)
}

fn etype(kite: &Kite, name: &str) -> ETypeId {
  kite
    .edge_def(name)
    .and_then(|def| def.etype_id)
    .expect("edge type")
}

/// Deterministic xorshift for reproducible graphs.
fn next_random(state: &mut u64) -> u64 {
  *state ^= *state << 13;
  *state ^= *state >> 7;
  *state ^= *state << 17;
  *state
}

#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn perf_a1_update_builder() {
  // Durable commits (the default sync mode) are what make the extra tx expensive.
  let (_dir, mut kite) = open(schema());
  let ids: Vec<NodeId> = (0..100)
    .map(|i| user(&mut kite, &format!("u{i}")))
    .collect();
  let mut next = ids.iter().cycle();
  time_runs("A1 update_by_id().set().execute()", 100, || {
    let id = *next.next().expect("id");
    kite
      .update_by_id(id)
      .expect("update_by_id")
      .set("name", s("x"))
      .execute()
      .expect("execute")
  });
}

#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn perf_a7_take_one_from_hub() {
  const FANOUT: usize = 200_000;
  let (_dir, kite) = open(perf_schema());
  let f = etype(&kite, "F");
  let db = kite.raw();
  db.begin(false).expect("begin");
  let hub = db.create_node(None).expect("hub");
  let leaves = db.create_nodes_batch(&vec![None; FANOUT]).expect("leaves");
  for &leaf in &leaves {
    db.add_edge(hub, f, leaf).expect("edge");
  }
  db.commit().expect("commit");

  time_runs("A7 from(hub).out(F).take(1).to_vec()", 20, || {
    kite.from(hub).out(Some("F")).expect("out").take(1).to_vec()
  });
  time_runs("A7 from(hub).out(F).first_node()", 20, || {
    kite.from(hub).out(Some("F")).expect("out").first_node()
  });
}

#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn perf_a8_a16_bulk_insert_and_count_by_type() {
  const PER_TYPE: usize = 50_000;
  let (_dir, mut kite) = open(perf_schema());
  for node_type in ["User", "Post"] {
    let items: Vec<(String, Props)> = (0..PER_TYPE)
      .map(|i| (format!("{i}"), props(&[("name", s("n"))])))
      .collect();
    let start = Instant::now();
    kite
      .insert(node_type)
      .and_then(|insert| insert.values_many_owned(items))
      .and_then(|insert| insert.execute())
      .expect("bulk insert");
    println!(
      "A16 insert({node_type}).values_many_owned({PER_TYPE}).execute(): {:?}",
      start.elapsed()
    );
  }

  time_runs("A8 count_nodes_by_type(User)", 5, || {
    kite.count_nodes_by_type("User").expect("count")
  });
  time_runs("A8 all(User).count()", 5, || {
    kite.all("User").expect("all").count()
  });
}

#[test]
#[ignore = "perf measurement; run with --ignored --nocapture"]
fn perf_a16_shortest_paths() {
  const NODES: usize = 20_000;
  const EDGES: usize = 100_000;
  let (_dir, kite) = open(perf_schema());
  let f = etype(&kite, "F");
  let db = kite.raw();
  db.begin(false).expect("begin");
  let nodes = db.create_nodes_batch(&vec![None; NODES]).expect("nodes");
  // A chain keeps every node reachable; random chords add the branching.
  for pair in nodes.windows(2) {
    db.add_edge(pair[0], f, pair[1]).expect("chain edge");
  }
  let mut state = 0x9E37_79B9_7F4A_7C15;
  for _ in 0..EDGES - (NODES - 1) {
    let src = nodes[next_random(&mut state) as usize % NODES];
    let dst = nodes[next_random(&mut state) as usize % NODES];
    db.add_edge(src, f, dst).expect("chord");
  }
  db.commit().expect("commit");
  let (source, target) = (nodes[0], nodes[NODES - 1]);

  time_runs("A16 shortest_path().find_bfs()", 20, || {
    kite.shortest_path(source, target).find_bfs().found
  });
  time_runs("A16 shortest_path().find() (dijkstra)", 20, || {
    kite.shortest_path(source, target).find().found
  });
  time_runs("A16 reachable_from(depth 3)", 20, || {
    kite
      .reachable_from(source, 3, Some("F"))
      .expect("reachable")
      .len()
  });
}
