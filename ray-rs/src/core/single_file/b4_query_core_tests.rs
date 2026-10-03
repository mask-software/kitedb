//! raydb-b4 `query-core` lane: reads that should cost what they return.
//!
//! - F1: a page of nodes or edges examines the page, not the whole listing, and a page's
//!   edge total does not list every edge;
//! - F2: `Kite::all` / `count_nodes_by_type` read only the nodes that may be of the type;
//! - F3: `take(n)` from a hub, and a typed neighbor list, examine about `n` edges.
//!
//! The randomized tests check that the paging, type listing and traversal paths return
//! exactly what the full listings say, with MVCC off and on, inside transactions with pending
//! changes, for an MVCC reader that sees version history, with deletes and recreates, and
//! after a checkpoint and a reopen; also over deltas large enough for their node maps to
//! take dense parts and keep their sparse ids in order (the `read-paths` lane's ordered
//! delta reads), and over snapshot hubs that no layer changes (its snapshot fast paths).
use crate::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use crate::api::traversal::{DbNeighbors, NeighborSource, TraversalDirection, TraverseOptions};
use crate::core::single_file::read::{EDGES_EXAMINED, NODES_EXAMINED, NODE_LOOKUPS};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::streaming::{edges_page_single, nodes_page_single, PaginationOptions};
use crate::types::{ETypeId, Edge, LabelId, NodeId};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use tempfile::tempdir;

// ============================================================================
// Helpers
// ============================================================================

fn reset_counters() {
  NODES_EXAMINED.with(|count| count.set(0));
  EDGES_EXAMINED.with(|count| count.set(0));
  NODE_LOOKUPS.with(|count| count.set(0));
}

fn nodes_examined() -> usize {
  NODES_EXAMINED.with(|count| count.get())
}

fn edges_examined() -> usize {
  EDGES_EXAMINED.with(|count| count.get())
}

fn node_lookups() -> usize {
  NODE_LOOKUPS.with(|count| count.get())
}

fn db_options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .sync_mode(SyncMode::Off)
    .auto_checkpoint(false)
}

fn page_opts(limit: usize, cursor: Option<String>) -> PaginationOptions {
  PaginationOptions { limit, cursor }
}

fn edge_key(edge: &Edge) -> (NodeId, ETypeId, NodeId) {
  (edge.src, edge.etype, edge.dst)
}

/// Every edge `list_edges` lists, sorted; it must list each edge once.
fn sorted_edges(db: &SingleFileDB) -> Vec<(NodeId, ETypeId, NodeId)> {
  let mut edges: Vec<_> = db
    .list_edges(None)
    .into_iter()
    .map(|edge| (edge.src, edge.etype, edge.dst))
    .collect();
  edges.sort_unstable();
  let listed = edges.len();
  edges.dedup();
  assert_eq!(listed, edges.len(), "list_edges listed an edge twice");
  edges
}

/// `count` nodes with keys `n0..`, checkpointed, then `extra` more in the delta.
fn keyed_nodes(db: &SingleFileDB, count: usize, extra: usize) -> Vec<NodeId> {
  db.begin(false).expect("begin");
  let keys: Vec<String> = (0..count).map(|i| format!("n{i}")).collect();
  let keys: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  let mut ids = db.create_nodes_batch(&keys).expect("nodes");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  db.begin(false).expect("begin");
  for i in 0..extra {
    ids.push(db.create_node(Some(&format!("x{i}"))).expect("node"));
  }
  db.commit().expect("commit");
  ids
}

// ============================================================================
// F1: paging
// ============================================================================

#[test]
fn query_core_f1_node_page_examines_only_the_page() {
  let dir = tempdir().expect("temp dir");
  let db = open_single_file(dir.path().join("db.kitedb"), db_options(false)).expect("open");
  let delta_nodes = 10;
  let ids = keyed_nodes(&db, 4000, delta_nodes);
  let limit = 50;

  reset_counters();
  let page = nodes_page_single(&db, page_opts(limit, Some(format!("n:{}", ids[2000]))));
  let examined = nodes_examined();

  assert_eq!(page.items, ids[2001..2001 + limit].to_vec());
  assert!(page.has_more);
  let bound = limit + delta_nodes + 16;
  assert!(
    examined <= bound,
    "a page of {limit} nodes examined {examined} node entries (bound {bound})"
  );
  close_single_file(db).expect("close");
}

#[test]
fn query_core_f1_edge_page_examines_only_the_page() {
  let dir = tempdir().expect("temp dir");
  let db = open_single_file(dir.path().join("db.kitedb"), db_options(false)).expect("open");
  let ids = keyed_nodes(&db, 1000, 0);
  db.begin(false).expect("begin");
  let etypes = [
    db.define_etype("A").expect("etype"),
    db.define_etype("B").expect("etype"),
  ];
  let mut rng = StdRng::seed_from_u64(1);
  for (i, &src) in ids.iter().enumerate() {
    for k in 0..4 {
      let dst = ids[(i + 1 + k * 7) % ids.len()];
      db.add_edge(src, etypes[rng.gen_range(0..2)], dst)
        .expect("edge");
    }
  }
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  let delta_edges = 20;
  db.begin(false).expect("begin");
  for i in 0..delta_edges {
    db.add_edge(ids[i * 31], etypes[0], ids[(i * 31 + 500) % ids.len()])
      .expect("edge");
  }
  db.commit().expect("commit");

  let edges = sorted_edges(&db);
  let (src, etype, dst) = edges[2000];
  let limit = 50;
  reset_counters();
  let page = edges_page_single(
    &db,
    page_opts(limit, Some(format!("e:{src}:{etype}:{dst}"))),
  );
  let examined = edges_examined();

  let items: Vec<_> = page.items.iter().map(edge_key).collect();
  assert_eq!(items, edges[2001..2001 + limit].to_vec());
  let bound = limit + delta_edges + 16;
  assert!(
    examined <= bound,
    "a page of {limit} edges examined {examined} edge entries (bound {bound})"
  );
  close_single_file(db).expect("close");
}

/// The bindings report `count_edges` as an edge page's total: it must follow the changes
/// since the checkpoint, not the edge count.
#[test]
fn query_core_f1_edge_count_examines_only_the_changes() {
  let dir = tempdir().expect("temp dir");
  let db = open_single_file(dir.path().join("db.kitedb"), db_options(false)).expect("open");
  let ids = keyed_nodes(&db, 1000, 0);
  db.begin(false).expect("begin");
  let etype = db.define_etype("A").expect("etype");
  for (i, &src) in ids.iter().enumerate() {
    for k in 1..=4 {
      db.add_edge(src, etype, ids[(i + k) % ids.len()])
        .expect("edge");
    }
  }
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  db.begin(false).expect("begin");
  for i in 0..20 {
    db.add_edge(ids[i * 40], etype, ids[(i * 40 + 300) % ids.len()])
      .expect("edge");
  }
  db.delete_edge(ids[3], etype, ids[4]).expect("delete edge");
  db.delete_node(ids[600]).expect("delete node");
  db.commit().expect("commit");

  let expected = db.list_edges(None).len();
  reset_counters();
  let count = db.count_edges();
  let examined = edges_examined() + nodes_examined();

  assert_eq!(count, expected);
  assert!(
    examined <= 100,
    "count_edges examined {examined} entries for 22 changed edges and 1 deleted node"
  );
  close_single_file(db).expect("close");
}

// ============================================================================
// F2: nodes by type
// ============================================================================

const TYPES: usize = 5;

fn typed_kite_options() -> KiteOptions {
  let mut options = KiteOptions::new();
  for t in 0..TYPES {
    options = options.node(NodeDef::new(&format!("T{t}"), &format!("t{t}:")));
  }
  options = options.node(NodeDef::new("Leaf", "leaf:"));
  options = options.edge(EdgeDef::new("A")).edge(EdgeDef::new("B"));
  options.sync_mode = SyncMode::Off;
  options
}

#[test]
fn query_core_f2_all_reads_only_nodes_of_the_type() {
  let dir = tempdir().expect("temp dir");
  let mut kite = Kite::open(dir.path().join("db.kitedb"), typed_kite_options()).expect("open");
  kite
    .transaction(|tx| {
      for i in 0..1000 {
        tx.create_node(&format!("T{}", i % TYPES), &i.to_string(), HashMap::new())?;
      }
      Ok(())
    })
    .expect("create");
  kite.raw().checkpoint().expect("checkpoint");
  kite
    .transaction(|tx| {
      for i in 1000..1025 {
        tx.create_node(&format!("T{}", i % TYPES), &i.to_string(), HashMap::new())?;
      }
      Ok(())
    })
    .expect("create");

  reset_counters();
  let all = kite.all("T2").expect("all").count();
  let all_lookups = node_lookups();
  reset_counters();
  let counted = kite.count_nodes_by_type("T2").expect("count");
  let count_lookups = node_lookups();

  assert_eq!(all, 205);
  assert_eq!(counted, 205);
  assert!(
    all_lookups <= all,
    "all(T2) read the key or labels of {all_lookups} nodes for {all} T2 nodes"
  );
  assert!(
    count_lookups <= all,
    "count_nodes_by_type(T2) read the key or labels of {count_lookups} nodes for {all} T2 nodes"
  );
  kite.close().expect("close");
}

// ============================================================================
// F3: lazy neighbors
// ============================================================================

#[test]
fn query_core_f3_take_one_from_a_hub_examines_few_edges() {
  let dir = tempdir().expect("temp dir");
  let mut kite = Kite::open(dir.path().join("db.kitedb"), typed_kite_options()).expect("open");
  let hub = kite
    .create_node("T0", "hub", HashMap::new())
    .expect("hub")
    .id();
  let sink = kite
    .create_node("T1", "sink", HashMap::new())
    .expect("sink")
    .id();
  let leaves = 3000;
  kite
    .transaction(|tx| {
      for i in 0..leaves {
        let leaf = tx.create_node("Leaf", &i.to_string(), HashMap::new())?.id();
        tx.link(hub, "A", leaf)?;
        tx.link(leaf, "A", sink)?;
        if i < 5 {
          tx.link(hub, "B", leaf)?;
        }
      }
      Ok(())
    })
    .expect("graph");
  kite.raw().checkpoint().expect("checkpoint");
  kite
    .transaction(|tx| {
      for i in 0..20 {
        let leaf = tx
          .create_node("Leaf", &format!("late{i}"), HashMap::new())?
          .id();
        tx.link(hub, "A", leaf)?;
        tx.link(leaf, "A", sink)?;
      }
      Ok(())
    })
    .expect("delta");

  type Case<'k> = (&'static str, Box<dyn Fn() -> usize + 'k>);
  let cases: Vec<Case<'_>> = vec![
    (
      "from(hub).out(None).take(1)",
      Box::new(|| {
        kite
          .from(hub)
          .out(None)
          .expect("out")
          .take(1)
          .to_vec()
          .len()
      }),
    ),
    (
      "from(hub).both(None).take(1)",
      Box::new(|| {
        kite
          .from(hub)
          .both(None)
          .expect("both")
          .take(1)
          .to_vec()
          .len()
      }),
    ),
    (
      "from(sink).in(None).take(1)",
      Box::new(|| {
        kite
          .from(sink)
          .r#in(None)
          .expect("in")
          .take(1)
          .to_vec()
          .len()
      }),
    ),
    (
      "from(hub).traverse(out, depth 1).take(1)",
      Box::new(|| {
        kite
          .from(hub)
          .traverse(None, TraverseOptions::new(TraversalDirection::Out, 1))
          .expect("traverse")
          .take(1)
          .to_vec()
          .len()
      }),
    ),
  ];
  for (name, run) in cases {
    reset_counters();
    let results = run();
    let examined = edges_examined();
    assert_eq!(results, 1, "{name}");
    assert!(examined <= 64, "{name} examined {examined} edge entries");
  }

  reset_counters();
  let typed = kite.neighbors_out(hub, Some("B")).expect("neighbors");
  let examined = edges_examined();
  assert_eq!(typed.len(), 5);
  assert!(
    examined <= 32,
    "neighbors_out(hub, B) examined {examined} edge entries for 5 B edges"
  );
  kite.close().expect("close");
}

// ============================================================================
// Randomized equivalence with the full listings
// ============================================================================

/// Node types with a shadowing prefix (`Admin` under `User`) and a shared one (`Post` and
/// `Draft`), so type resolution goes through every rule.
fn random_kite_options(mvcc: bool) -> KiteOptions {
  let mut options = KiteOptions::new()
    .node(NodeDef::new("User", "user:"))
    .node(NodeDef::new("Admin", "user:admin:"))
    .node(NodeDef::new("Post", "post:"))
    .node(NodeDef::new("Draft", "post:"))
    .node(NodeDef::new("Tag", "tag:"))
    .edge(EdgeDef::new("A"))
    .edge(EdgeDef::new("B"));
  options.sync_mode = SyncMode::Off;
  options.mvcc = mvcc;
  options.checkpoint_threshold = Some(1.0);
  options
}

const NODE_TYPES: [&str; 5] = ["User", "Admin", "Post", "Draft", "Tag"];
const KEY_PREFIXES: [&str; 5] = ["user:", "user:admin:", "post:", "tag:", "zz:"];

struct Ids {
  labels: Vec<LabelId>,
  etypes: Vec<ETypeId>,
  /// Few nodes with many edges each, so neighbor reads span several slices
  dense: bool,
}

fn ids(kite: &Kite, dense: bool) -> Ids {
  Ids {
    dense,
    labels: NODE_TYPES
      .iter()
      .map(|name| {
        kite
          .node_def(name)
          .and_then(|def| def.label_id)
          .expect("label")
      })
      .collect(),
    etypes: ["A", "B"]
      .iter()
      .map(|name| {
        kite
          .edge_def(name)
          .and_then(|def| def.etype_id)
          .expect("etype")
      })
      .collect(),
  }
}

/// `ops` random writes in the calling thread's open write transaction: creates (keyed by
/// any prefix, by none, or unkeyed, with random labels), deletes, recreates of deleted ids,
/// edges (self-loops too) added and deleted, labels added and removed.
fn random_writes(db: &SingleFileDB, ids: &Ids, rng: &mut StdRng, seq: &mut usize, ops: usize) {
  for _ in 0..ops {
    let nodes = db.list_nodes();
    let pick = |rng: &mut StdRng| nodes[rng.gen_range(0..nodes.len())];
    let roll = rng.gen_range(0..100);
    // Dense graphs create nodes only to stay at about 12, delete few, and add
    // half of their edges to or from the first node (a hub).
    let roll = match ids.dense {
      false => roll,
      true if nodes.len() < 12 => 0,
      true if roll < 1 => 22,
      true if roll < 3 => 28 + roll,
      true => 34 + roll * 66 / 100,
    };
    let hub = |rng: &mut StdRng| {
      if ids.dense && rng.gen_bool(0.5) {
        nodes[0]
      } else {
        pick(rng)
      }
    };
    if nodes.len() < 4 || roll < 22 {
      *seq += 1;
      let key = random_key(rng, *seq);
      let node = db.create_node(key.as_deref()).expect("create");
      add_random_labels(db, ids, rng, node);
    } else if roll < 28 {
      db.delete_node(pick(rng)).expect("delete");
    } else if roll < 34 {
      let max = nodes.iter().copied().max().unwrap_or(1);
      let id = rng.gen_range(1..=max + 1);
      if !nodes.contains(&id) {
        *seq += 1;
        let key = random_key(rng, *seq);
        db.create_node_with_id(id, key.as_deref())
          .expect("recreate");
        add_random_labels(db, ids, rng, id);
      }
    } else if roll < 80 {
      let (src, dst) = if rng.gen_bool(0.5) {
        (hub(rng), pick(rng))
      } else {
        (pick(rng), hub(rng))
      };
      let dst = if rng.gen_bool(0.1) { src } else { dst };
      let etype = ids.etypes[rng.gen_range(0..ids.etypes.len())];
      db.add_edge(src, etype, dst).expect("add edge");
    } else if roll < 88 {
      let src = pick(rng);
      let out = db.out_edges(src);
      if !out.is_empty() {
        let (etype, dst) = out[rng.gen_range(0..out.len())];
        db.delete_edge(src, etype, dst).expect("delete edge");
      }
    } else if roll < 94 {
      let label = ids.labels[rng.gen_range(0..ids.labels.len())];
      db.add_node_label(pick(rng), label).expect("add label");
    } else {
      let node = pick(rng);
      let labels = db.node_labels(node);
      if !labels.is_empty() {
        db.remove_node_label(node, labels[rng.gen_range(0..labels.len())])
          .expect("remove label");
      }
    }
  }
}

fn random_key(rng: &mut StdRng, seq: usize) -> Option<String> {
  if rng.gen_bool(0.15) {
    return None;
  }
  let prefix = KEY_PREFIXES[rng.gen_range(0..KEY_PREFIXES.len())];
  Some(format!("{prefix}{seq}"))
}

fn add_random_labels(db: &SingleFileDB, ids: &Ids, rng: &mut StdRng, node: NodeId) {
  for _ in 0..rng.gen_range(0..3) {
    let label = ids.labels[rng.gen_range(0..ids.labels.len())];
    db.add_node_label(node, label).expect("label");
  }
}

fn commit_random(kite: &Kite, ids: &Ids, rng: &mut StdRng, seq: &mut usize, ops: usize) {
  let db = kite.raw();
  db.begin(false).expect("begin");
  random_writes(db, ids, rng, seq, ops);
  db.commit().expect("commit");
}

/// Walk every page of `limit` nodes.
fn walk_node_pages(db: &SingleFileDB, limit: usize) -> Vec<NodeId> {
  let mut items = Vec::new();
  let mut cursor = None;
  loop {
    let page = nodes_page_single(db, page_opts(limit, cursor));
    assert!(page.items.len() <= if limit == 0 { 100 } else { limit });
    assert_eq!(page.has_more, page.next_cursor.is_some());
    items.extend(page.items);
    if !page.has_more {
      return items;
    }
    cursor = page.next_cursor;
  }
}

/// Walk every page of `limit` edges.
fn walk_edge_pages(db: &SingleFileDB, limit: usize) -> Vec<(NodeId, ETypeId, NodeId)> {
  let mut items = Vec::new();
  let mut cursor = None;
  loop {
    let page = edges_page_single(db, page_opts(limit, cursor));
    assert!(page.items.len() <= if limit == 0 { 100 } else { limit });
    assert_eq!(page.has_more, page.next_cursor.is_some());
    items.extend(page.items.iter().map(edge_key));
    if !page.has_more {
      return items;
    }
    cursor = page.next_cursor;
  }
}

/// Pages, and pages from arbitrary cursors, against the full listings.
fn check_pages(db: &SingleFileDB, rng: &mut StdRng, at: &str) {
  let nodes = db.list_nodes();
  let edges = sorted_edges(db);
  for limit in [1, 2, 3, 7, 0] {
    assert_eq!(
      walk_node_pages(db, limit),
      nodes,
      "{at}: node pages of {limit}"
    );
    assert_eq!(
      walk_edge_pages(db, limit),
      edges,
      "{at}: edge pages of {limit}"
    );
  }
  let max_id = nodes.last().copied().unwrap_or(0);
  for _ in 0..12 {
    let cursor = rng.gen_range(0..=max_id + 2);
    let limit = rng.gen_range(1..5);
    let expected: Vec<_> = nodes
      .iter()
      .copied()
      .filter(|&id| id > cursor)
      .take(limit)
      .collect();
    let page = nodes_page_single(db, page_opts(limit, Some(format!("n:{cursor}"))));
    assert_eq!(page.items, expected, "{at}: nodes after {cursor}");

    let etype = rng.gen_range(0..4);
    let dst = rng.gen_range(0..=max_id + 2);
    let after = (cursor, etype, dst);
    let expected: Vec<_> = edges
      .iter()
      .copied()
      .filter(|&edge| edge > after)
      .take(limit)
      .collect();
    let page = edges_page_single(
      db,
      page_opts(limit, Some(format!("e:{cursor}:{etype}:{dst}"))),
    );
    let items: Vec<_> = page.items.iter().map(edge_key).collect();
    assert_eq!(items, expected, "{at}: edges after {after:?}");
  }
}

/// `Kite::all` and `count_nodes_by_type` against resolving every node's type.
fn check_types(kite: &Kite, at: &str) {
  let db = kite.raw();
  let mut expected: HashMap<String, Vec<(NodeId, Option<String>)>> = HashMap::new();
  for node_id in db.list_nodes() {
    let node = kite
      .node_by_id(node_id)
      .expect("node")
      .expect("listed node");
    expected
      .entry(node.node_type().to_string())
      .or_default()
      .push((node_id, node.key().map(str::to_string)));
  }
  for name in NODE_TYPES {
    let all: Vec<_> = kite
      .all(name)
      .expect("all")
      .map(|node| {
        assert_eq!(node.node_type(), name);
        (node.id(), node.key().map(str::to_string))
      })
      .collect();
    let want = expected.get(name).cloned().unwrap_or_default();
    assert_eq!(all, want, "{at}: all({name})");
    let count = kite.count_nodes_by_type(name).expect("count");
    assert_eq!(
      count,
      want.len() as u64,
      "{at}: count_nodes_by_type({name})"
    );
  }
}

/// The edges `Kite::neighbors` lists, from the full out- and in-edge lists.
fn expected_neighbors(
  db: &SingleFileDB,
  node_id: NodeId,
  direction: TraversalDirection,
  etype: Option<ETypeId>,
) -> Vec<Edge> {
  let wanted = |e: ETypeId| etype.is_none_or(|t| t == e);
  let out = || {
    db.out_edges(node_id)
      .into_iter()
      .filter(|&(e, _)| wanted(e))
      .map(|(e, dst)| Edge {
        src: node_id,
        etype: e,
        dst,
      })
      .collect::<Vec<_>>()
  };
  let incoming = || {
    db.in_edges(node_id)
      .into_iter()
      .filter(|&(e, _)| wanted(e))
      .map(|(e, src)| Edge {
        src,
        etype: e,
        dst: node_id,
      })
      .collect::<Vec<_>>()
  };
  match direction {
    TraversalDirection::Out => out(),
    TraversalDirection::In => incoming(),
    TraversalDirection::Both => {
      let mut edges = out();
      edges.extend(incoming().into_iter().filter(|edge| edge.src != edge.dst));
      edges
    }
  }
}

/// The nodes per-node checks read: all of `nodes`, or in large states the first and last
/// (hubs of the large states) and `SAMPLED_NODES` random ones.
fn sampled(nodes: &[NodeId], rng: &mut StdRng) -> Vec<NodeId> {
  const SAMPLED_NODES: usize = 300;
  if nodes.len() <= SAMPLED_NODES {
    return nodes.to_vec();
  }
  let mut picked: Vec<NodeId> = (0..SAMPLED_NODES)
    .map(|_| nodes[rng.gen_range(0..nodes.len())])
    .chain([nodes[0], nodes[nodes.len() - 1]])
    .collect();
  picked.sort_unstable();
  picked.dedup();
  picked
}

/// Neighbor lists and `take(n)` traversals against the full lists.
fn check_traversals(kite: &Kite, ids: &Ids, rng: &mut StdRng, at: &str) {
  let db = kite.raw();
  let nodes = db.list_nodes();
  if nodes.is_empty() {
    return;
  }
  let directions = [
    TraversalDirection::Out,
    TraversalDirection::In,
    TraversalDirection::Both,
  ];
  for node_id in sampled(&nodes, rng) {
    for direction in directions {
      for etype in [None, Some(ids.etypes[0]), Some(ids.etypes[1])] {
        let expected = expected_neighbors(db, node_id, direction, etype);
        assert_eq!(
          kite.neighbors(node_id, direction, etype),
          expected,
          "{at}: neighbors({node_id}, {direction:?}, {etype:?})"
        );
        let lazy: Vec<_> = DbNeighbors::new(db)
          .edges(node_id, direction, etype)
          .collect();
        assert_eq!(
          lazy, expected,
          "{at}: lazy neighbors({node_id}, {direction:?}, {etype:?})"
        );
      }
    }
  }
  let etype_names = [None, Some("A"), Some("B")];
  for _ in 0..20 {
    let start = nodes[rng.gen_range(0..nodes.len())];
    let first = etype_names[rng.gen_range(0..3)];
    let second = etype_names[rng.gen_range(0..3)];
    let take = rng.gen_range(0..6);
    let builder = || {
      let from = kite.from(start);
      let from = match rng.clone().gen_range(0..3) {
        0 => from.out(first),
        1 => from.r#in(first),
        _ => from.both(first),
      }
      .expect("step");
      match take % 3 {
        0 => from,
        1 => from.both(second).expect("step"),
        _ => from
          .traverse(
            second,
            TraverseOptions::new(directions[take % directions.len()], 2),
          )
          .expect("step"),
      }
    };
    let full = builder().to_vec();
    let taken = builder().take(take).to_vec();
    assert_eq!(
      taken,
      full[..full.len().min(take)].to_vec(),
      "{at}: take({take}) from {start}"
    );
    assert_eq!(builder().count(), full.len(), "{at}: count from {start}");
  }
}

/// The seek, label, key-prefix and count reads against the full listings.
fn check_listings(db: &SingleFileDB, ids: &Ids, rng: &mut StdRng, at: &str) {
  let nodes = db.list_nodes();
  let edges = sorted_edges(db);
  let max_id = nodes.last().copied().unwrap_or(0);

  // nodes_after / edges_after
  for _ in 0..16 {
    let after = rng.gen_bool(0.2).then(|| rng.gen_range(0..=max_id + 2));
    let limit = rng.gen_range(0..8);
    let expected: Vec<_> = nodes
      .iter()
      .copied()
      .filter(|&id| after.is_none_or(|after| id > after))
      .take(limit)
      .collect();
    assert_eq!(
      db.nodes_after(after, limit),
      expected,
      "{at}: nodes_after({after:?}, {limit})"
    );

    let after = rng
      .gen_bool(0.8)
      .then(|| match edges.get(rng.gen_range(0..edges.len().max(1))) {
        Some(&(src, etype, dst)) if rng.gen_bool(0.6) => (src, etype, dst),
        _ => (
          rng.gen_range(0..=max_id + 2),
          rng.gen_range(0..4),
          rng.gen_range(0..=max_id + 2),
        ),
      });
    let expected: Vec<_> = edges
      .iter()
      .copied()
      .filter(|&edge| after.is_none_or(|after| edge > after))
      .take(limit)
      .collect();
    let got: Vec<_> = db
      .edges_after(after, limit)
      .into_iter()
      .map(|edge| (edge.src, edge.etype, edge.dst))
      .collect();
    assert_eq!(got, expected, "{at}: edges_after({after:?}, {limit})");
  }
  assert_eq!(db.count_edges(), edges.len(), "{at}: count_edges");

  // out_edges_after / in_edges_after, from every node (and a missing one)
  for node_id in sampled(&nodes, rng).into_iter().chain([max_id + 1]) {
    let out = db.out_edges(node_id);
    let incoming = db.in_edges(node_id);
    for (all, slice) in [
      (
        &out,
        &(|e, a, l| db.out_edges_after(node_id, e, a, l)) as &dyn Fn(_, _, _) -> _,
      ),
      (&incoming, &|e, a, l| db.in_edges_after(node_id, e, a, l)),
    ] {
      for _ in 0..4 {
        let etype = [None, Some(ids.etypes[0]), Some(ids.etypes[1]), Some(99)][rng.gen_range(0..4)];
        let after = match rng.gen_range(0..3) {
          0 => None,
          1 if !all.is_empty() => Some(all[rng.gen_range(0..all.len())]),
          _ => Some((rng.gen_range(0..4), rng.gen_range(0..=max_id + 2))),
        };
        let limit = [0, 1, 2, 5, usize::MAX][rng.gen_range(0..5)];
        let expected: Vec<_> = all
          .iter()
          .copied()
          .filter(|&(e, _)| etype.is_none_or(|t| t == e))
          .filter(|&key| after.is_none_or(|after| key > after))
          .take(limit)
          .collect();
        assert_eq!(
          slice(etype, after, limit),
          expected,
          "{at}: edges of {node_id} ({etype:?}, after {after:?}, {limit})"
        );
      }
    }
  }

  // nodes_with_label / count_nodes_with_label
  for &label in ids.labels.iter().chain(&[9999]) {
    let expected: Vec<_> = nodes
      .iter()
      .copied()
      .filter(|&id| db.node_labels(id).contains(&label))
      .collect();
    assert_eq!(
      db.nodes_with_label(label),
      expected,
      "{at}: nodes_with_label({label})"
    );
    assert_eq!(
      db.count_nodes_with_label(label),
      expected.len(),
      "{at}: count_nodes_with_label({label})"
    );
    for &id in &expected {
      assert!(
        db.node_has_label(id, label),
        "{at}: node_has_label({id}, {label})"
      );
    }
  }

  // nodes_with_key_prefix
  for prefix in KEY_PREFIXES
    .iter()
    .copied()
    .chain(["", "user:a", "post:1", "nope"])
  {
    let expected: Vec<_> = nodes
      .iter()
      .filter_map(|&id| {
        let key = db.node_key(id)?;
        key.starts_with(prefix).then_some((id, key))
      })
      .collect();
    assert_eq!(
      db.nodes_with_key_prefix(prefix),
      expected,
      "{at}: nodes_with_key_prefix({prefix:?})"
    );
  }
}

fn check_all(kite: &Kite, ids: &Ids, rng: &mut StdRng, at: &str) {
  check_pages(kite.raw(), rng, at);
  check_listings(kite.raw(), ids, rng, at);
  check_types(kite, at);
  check_traversals(kite, ids, rng, at);
}

/// Run the checks in every state the lane cares about, for one seed and MVCC mode.
fn run_random_states(path: &Path, mvcc: bool, dense: bool, seed: u64) {
  let mut rng = StdRng::seed_from_u64(seed);
  let mut seq = 0;
  let kite = Kite::open(path, random_kite_options(mvcc)).expect("open");
  let ids = ids(&kite, dense);
  let mode = match (mvcc, dense) {
    (false, false) => "no mvcc",
    (true, false) => "mvcc",
    (false, true) => "no mvcc dense",
    (true, true) => "mvcc dense",
  };

  // Committed, partly in the snapshot and partly in the delta.
  for _ in 0..4 {
    commit_random(&kite, &ids, &mut rng, &mut seq, 50);
  }
  kite.raw().checkpoint().expect("checkpoint");
  for _ in 0..4 {
    commit_random(&kite, &ids, &mut rng, &mut seq, 30);
  }
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} committed"),
  );

  // Inside a write transaction with pending changes.
  kite.raw().begin(false).expect("begin");
  random_writes(kite.raw(), &ids, &mut rng, &mut seq, 30);
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} pending"),
  );
  kite.raw().rollback().expect("rollback");

  // A reader that began before later commits: with MVCC, it reads version history. Without
  // MVCC, an open write transaction would hold off the commits (one writer at a time).
  let readers: &[bool] = if mvcc { &[true, false] } else { &[true] };
  for &read_only in readers {
    let at = format!("{mode} seed {seed} reader (read_only {read_only})");
    let mut reader_rng = StdRng::seed_from_u64(seed ^ 0x5eed);
    let (began_tx, began_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
      let kite = &kite;
      let ids = &ids;
      let at = &at;
      let reader = scope.spawn(move || {
        kite.raw().begin(read_only).expect("begin reader");
        if !read_only {
          let mut seq = 1_000_000;
          random_writes(kite.raw(), ids, &mut reader_rng, &mut seq, 10);
        }
        began_tx.send(()).expect("began");
        go_rx.recv().expect("go");
        check_all(kite, ids, &mut reader_rng, at);
        kite.raw().rollback().expect("rollback reader");
      });
      began_rx.recv().expect("reader began");
      for _ in 0..2 {
        commit_random(kite, ids, &mut rng, &mut seq, 15);
      }
      go_tx.send(()).expect("go");
      reader.join().expect("reader");
    });
  }

  // After a checkpoint and a reopen.
  kite.raw().checkpoint().expect("checkpoint");
  commit_random(&kite, &ids, &mut rng, &mut seq, 10);
  kite.close().expect("close");
  let kite = Kite::open(path, random_kite_options(mvcc)).expect("reopen");
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} reopened"),
  );
  kite.close().expect("close");
}

/// `count` nodes created in batches with random keys (any prefix, none, or unkeyed) and
/// labels, each with `edges` out-edges to random nodes (the new ones, or any of `targets`).
fn create_batch(
  db: &SingleFileDB,
  ids: &Ids,
  rng: &mut StdRng,
  seq: &mut usize,
  count: usize,
  edges: usize,
  targets: &[NodeId],
) -> Vec<NodeId> {
  let keys: Vec<Option<String>> = (0..count)
    .map(|_| {
      *seq += 1;
      random_key(rng, *seq)
    })
    .collect();
  let keys: Vec<Option<&str>> = keys.iter().map(Option::as_deref).collect();
  let created = db.create_nodes_batch(&keys).expect("nodes");
  let targets: Vec<NodeId> = targets
    .iter()
    .copied()
    .filter(|&node| db.node_exists(node))
    .collect();
  for &node in &created {
    add_random_labels(db, ids, rng, node);
  }
  let mut batch = Vec::with_capacity(count * edges);
  for &src in &created {
    for _ in 0..edges {
      let dst = if targets.is_empty() || rng.gen_bool(0.5) {
        created[rng.gen_range(0..created.len())]
      } else {
        targets[rng.gen_range(0..targets.len())]
      };
      batch.push((src, ids.etypes[rng.gen_range(0..ids.etypes.len())], dst));
    }
  }
  db.add_edges_batch(&batch).expect("edges");
  created
}

/// Deletes of `count` random nodes of `pool`, then recreates of some of those ids, then a run
/// of 64 aligned new ids (a whole chunk of a dense part, freed) and tombstones of random
/// edges, in the calling thread's open write transaction.
fn churn(
  db: &SingleFileDB,
  ids: &Ids,
  rng: &mut StdRng,
  seq: &mut usize,
  pool: &[NodeId],
  count: usize,
) {
  let mut deleted = Vec::new();
  for _ in 0..count {
    let node = pool[rng.gen_range(0..pool.len())];
    if db.node_exists(node) {
      db.delete_node(node).expect("delete");
      deleted.push(node);
    }
  }
  for &node in &deleted {
    if rng.gen_bool(0.4) {
      continue;
    }
    *seq += 1;
    let key = random_key(rng, *seq);
    db.create_node_with_id(node, key.as_deref())
      .expect("recreate");
    add_random_labels(db, ids, rng, node);
    let dst = pool[rng.gen_range(0..pool.len())];
    if db.node_exists(dst) {
      db.add_edge(node, ids.etypes[0], dst).expect("edge");
    }
  }
  let nodes = db.list_nodes();
  for _ in 0..count {
    let src = nodes[rng.gen_range(0..nodes.len())];
    let out = db.out_edges(src);
    if !out.is_empty() {
      let (etype, dst) = out[rng.gen_range(0..out.len())];
      db.delete_edge(src, etype, dst).expect("delete edge");
    }
  }
}

/// Delete the 64 nodes of an aligned run of ids in `created` (a dense part's chunk).
fn free_a_chunk(db: &SingleFileDB, created: &[NodeId]) {
  let Some(start) = created.iter().position(|&id| id % 64 == 0) else {
    return;
  };
  for &node in created[start..]
    .iter()
    .take_while(|&&id| id < created[start] + 64)
  {
    if db.node_exists(node) {
      db.delete_node(node).expect("delete");
    }
  }
}

/// The checks over deltas large enough for their node maps to take dense parts (more than
/// 1024 created nodes and sources) beside sparse ids kept in order (recreated ids far below
/// the new ones), with freed chunks; inside a transaction that creates as many; for an MVCC
/// reader with history; after a reopen that replays them; and over a snapshot hub (the
/// first node) that no layer changes, then one that deletes nodes elsewhere.
fn run_large_delta_states(path: &Path, mvcc: bool, seed: u64) {
  let mut rng = StdRng::seed_from_u64(seed ^ 0x1a26e);
  let mut seq = 0;
  let mode = if mvcc { "mvcc large" } else { "no mvcc large" };
  let kite = Kite::open(path, random_kite_options(mvcc)).expect("open");
  let ids = ids(&kite, true);
  let db = kite.raw();

  // The snapshot: a hub (the first node) with edges to 3000 of 6000 nodes.
  db.begin(false).expect("begin");
  let base = create_batch(db, &ids, &mut rng, &mut seq, 6000, 1, &[]);
  let hub_edges: Vec<_> = base[1..]
    .iter()
    .step_by(2)
    .map(|&dst| (base[0], ids.etypes[rng.gen_range(0..2)], dst))
    .collect();
  db.add_edges_batch(&hub_edges).expect("hub edges");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  // A large delta with no deletes: the hub's edges stay the snapshot's alone.
  let targets: Vec<NodeId> = base[1..].to_vec();
  let mut created = Vec::new();
  for _ in 0..2 {
    db.begin(false).expect("begin");
    created.extend(create_batch(db, &ids, &mut rng, &mut seq, 900, 2, &targets));
    db.commit().expect("commit");
  }
  check_all(&kite, &ids, &mut rng, &format!("{mode} seed {seed} grown"));

  // Deletes and recreates of snapshot ids (sparse below the new ones), a freed chunk,
  // tombstones, and the hub's own patches.
  db.begin(false).expect("begin");
  churn(db, &ids, &mut rng, &mut seq, &base[..3000], 150);
  free_a_chunk(db, &created);
  db.add_edge(base[0], ids.etypes[1], created[5])
    .expect("hub patch");
  db.commit().expect("commit");
  commit_random(&kite, &ids, &mut rng, &mut seq, 30);
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} churned"),
  );

  // Inside a write transaction that creates, deletes and recreates as much.
  db.begin(false).expect("begin");
  let pending = create_batch(db, &ids, &mut rng, &mut seq, 1100, 2, &created);
  churn(db, &ids, &mut rng, &mut seq, &created, 60);
  free_a_chunk(db, &pending);
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} large pending"),
  );
  db.rollback().expect("rollback");

  // A reader that began before more large commits (with MVCC, it reads version history).
  if mvcc {
    let at = format!("{mode} seed {seed} reader");
    let mut reader_rng = StdRng::seed_from_u64(seed ^ 0xbeef);
    let (began_tx, began_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
      let kite = &kite;
      let ids = &ids;
      let at = &at;
      let reader = scope.spawn(move || {
        kite.raw().begin(true).expect("begin reader");
        began_tx.send(()).expect("began");
        go_rx.recv().expect("go");
        check_all(kite, ids, &mut reader_rng, at);
        kite.raw().rollback().expect("rollback reader");
      });
      began_rx.recv().expect("reader began");
      db.begin(false).expect("begin");
      let more = create_batch(db, ids, &mut rng, &mut seq, 600, 2, &created);
      churn(db, ids, &mut rng, &mut seq, &created, 40);
      free_a_chunk(db, &more);
      db.commit().expect("commit");
      go_tx.send(()).expect("go");
      reader.join().expect("reader");
    });
  }

  // Reopened: the delta replayed from the WAL.
  kite.close().expect("close");
  let kite = Kite::open(path, random_kite_options(mvcc)).expect("reopen");
  check_all(
    &kite,
    &ids,
    &mut rng,
    &format!("{mode} seed {seed} reopened"),
  );
  kite.close().expect("close");
}

#[test]
fn query_core_reads_match_full_listings_randomized() {
  for mvcc in [false, true] {
    for dense in [false, true] {
      for seed in 0..6u64 {
        let dir = tempdir().expect("temp dir");
        run_random_states(&dir.path().join("db.kitedb"), mvcc, dense, seed);
      }
    }
    for seed in 0..2u64 {
      let dir = tempdir().expect("temp dir");
      run_large_delta_states(&dir.path().join("db.kitedb"), mvcc, seed);
    }
  }
}
