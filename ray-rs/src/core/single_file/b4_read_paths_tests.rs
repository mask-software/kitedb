//! raydb-b4 `read-paths` lane: reads over a large uncheckpointed delta, and snapshot edge
//! listings.
//!
//! - F1: a page of nodes or edges, `count_nodes` / `count_edges`, and the label listing visit
//!   what they return plus the changes that can hide or add to it, not every change since the
//!   last checkpoint;
//! - F2: listing a node's edges from the snapshot, when no layer above it changes them,
//!   checks no edge against those layers one by one.
//!
//! The equivalence of these paths with the full listings is checked by the randomized test
//! of the query-core lane (`b4_query_core_tests.rs`), which covers large deltas too.
use crate::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use crate::api::traversal::{DbNeighbors, NeighborSource, TraversalDirection};
use crate::core::single_file::read::{EDGES_EXAMINED, NODES_EXAMINED, OVERLAY_CHECKS};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::streaming::{edges_page_single, nodes_page_single, PaginationOptions};
use crate::types::{ETypeId, NodeId};
use std::collections::HashMap;
use tempfile::tempdir;

fn reset_counters() {
  NODES_EXAMINED.with(|count| count.set(0));
  EDGES_EXAMINED.with(|count| count.set(0));
  OVERLAY_CHECKS.with(|count| count.set(0));
}

fn examined() -> usize {
  NODES_EXAMINED.with(|count| count.get()) + EDGES_EXAMINED.with(|count| count.get())
}

fn overlay_checks() -> usize {
  OVERLAY_CHECKS.with(|count| count.get())
}

fn db_options(mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .sync_mode(SyncMode::Off)
    .auto_checkpoint(false)
    .wal_size(256 << 20)
}

fn page(limit: usize, cursor: Option<String>) -> PaginationOptions {
  PaginationOptions { limit, cursor }
}

/// `count` keyed nodes created in commits of 5000, left in the delta.
fn delta_nodes(db: &SingleFileDB, count: usize, prefix: &str) -> Vec<NodeId> {
  let mut ids = Vec::with_capacity(count);
  let keys: Vec<String> = (0..count).map(|i| format!("{prefix}{i}")).collect();
  for chunk in keys.chunks(5000) {
    db.begin(false).expect("begin");
    let chunk: Vec<Option<&str>> = chunk.iter().map(|key| Some(key.as_str())).collect();
    ids.extend(db.create_nodes_batch(&chunk).expect("nodes"));
    db.commit().expect("commit");
  }
  ids
}

/// `per_node` edges from each of `ids` to later ones, of `etype`, in commits of 20000.
fn delta_edges(db: &SingleFileDB, ids: &[NodeId], etype: ETypeId, per_node: usize) {
  let edges: Vec<_> = ids
    .iter()
    .enumerate()
    .flat_map(|(i, &src)| (1..=per_node).map(move |k| (src, i + k)))
    .map(|(src, j)| (src, etype, ids[j % ids.len()]))
    .collect();
  for chunk in edges.chunks(20_000) {
    db.begin(false).expect("begin");
    db.add_edges_batch(chunk).expect("edges");
    db.commit().expect("commit");
  }
}

// ============================================================================
// F1: reads over a large delta
// ============================================================================

/// A page of nodes from the middle of 20000 nodes created since the last checkpoint visits
/// the page, also inside a transaction that created 3000 more. Regression: every page passed
/// over every node the delta (and the transaction) created, 4.9 ms a page at 1M nodes.
#[test]
fn read_paths_f1_node_page_in_a_large_delta_visits_only_the_page() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("temp dir");
    let db = open_single_file(dir.path().join("db.kitedb"), db_options(mvcc)).expect("open");
    let ids = delta_nodes(&db, 20_000, "n");
    let limit = 50;
    let cursor = Some(format!("n:{}", ids[10_000]));

    reset_counters();
    let items = nodes_page_single(&db, page(limit, cursor.clone())).items;
    let visited = examined();
    assert_eq!(items, ids[10_001..10_001 + limit].to_vec());
    let bound = limit + 16;
    assert!(
      visited <= bound,
      "mvcc {mvcc}: a page of {limit} of 20000 delta nodes visited {visited} node entries \
       (bound {bound})"
    );

    db.begin(false).expect("begin");
    let pending: Vec<NodeId> = (0..3000)
      .map(|_| db.create_node(None).expect("pending node"))
      .collect();
    reset_counters();
    let items = nodes_page_single(&db, page(limit, cursor)).items;
    let visited = examined();
    assert_eq!(items, ids[10_001..10_001 + limit].to_vec());
    assert!(
      visited <= bound,
      "mvcc {mvcc}: a page inside a transaction with 3000 created nodes visited {visited} \
       node entries (bound {bound})"
    );
    let last = nodes_page_single(&db, page(limit, Some(format!("n:{}", ids[19_990])))).items;
    let mut expected = ids[19_991..].to_vec();
    expected.extend(&pending[..limit - expected.len()]);
    assert_eq!(
      last, expected,
      "mvcc {mvcc}: the page across delta and transaction"
    );
    db.rollback().expect("rollback");
    close_single_file(db).expect("close");
  }
}

/// A page of edges from the middle of 20000 edges added since the last checkpoint visits
/// about the page. Regression: every page passed over every source with added edges, 1.4 ms
/// a page at 5M edges.
#[test]
fn read_paths_f1_edge_page_in_a_large_delta_visits_only_the_page() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("temp dir");
    let db = open_single_file(dir.path().join("db.kitedb"), db_options(mvcc)).expect("open");
    db.begin(false).expect("begin");
    let etype = db.define_etype("E").expect("etype");
    db.commit().expect("commit");
    let ids = delta_nodes(&db, 5000, "n");
    delta_edges(&db, &ids, etype, 4);

    let mut edges: Vec<_> = db
      .list_edges(None)
      .into_iter()
      .map(|edge| (edge.src, edge.etype, edge.dst))
      .collect();
    edges.sort_unstable();
    let (src, etype, dst) = edges[10_000];
    let limit = 50;
    reset_counters();
    let items: Vec<_> = edges_page_single(&db, page(limit, Some(format!("e:{src}:{etype}:{dst}"))))
      .items
      .iter()
      .map(|edge| (edge.src, edge.etype, edge.dst))
      .collect();
    let visited = examined();
    assert_eq!(items, edges[10_001..10_001 + limit].to_vec());
    let bound = 2 * limit + 32;
    assert!(
      visited <= bound,
      "mvcc {mvcc}: a page of {limit} of 20000 delta edges visited {visited} node and edge \
       entries (bound {bound})"
    );
    close_single_file(db).expect("close");
  }
}

/// `count_nodes` and `count_edges` (the bindings' page totals) over 20000 nodes and 40000
/// edges created since a checkpoint, with deletes, a recreate and tombstones among them,
/// visit only those changes. Regression: they passed over every created node and added
/// edge, 9.9 ms and 17.8 ms at 1M nodes and 5M edges.
#[test]
fn read_paths_f1_counts_over_a_large_delta_visit_only_the_changes() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("temp dir");
    let db = open_single_file(dir.path().join("db.kitedb"), db_options(mvcc)).expect("open");
    db.begin(false).expect("begin");
    let etype = db.define_etype("E").expect("etype");
    db.commit().expect("commit");
    let base = delta_nodes(&db, 1000, "s");
    delta_edges(&db, &base, etype, 2);
    db.checkpoint().expect("checkpoint");
    let ids = delta_nodes(&db, 20_000, "n");
    delta_edges(&db, &ids, etype, 2);
    db.begin(false).expect("begin");
    for &node in &[base[10], base[20], ids[30], ids[40]] {
      db.delete_node(node).expect("delete");
    }
    db.delete_edge(base[100], etype, base[101])
      .expect("tombstone");
    db.delete_edge(ids[100], etype, ids[101])
      .expect("delete delta edge");
    db.create_node_with_id(base[10], Some("again"))
      .expect("recreate");
    db.add_edge(base[10], etype, ids[5]).expect("edge");
    db.commit().expect("commit");

    for in_tx in [false, true] {
      if in_tx {
        db.begin(false).expect("begin");
        db.delete_node(ids[50]).expect("pending delete");
        db.delete_edge(base[200], etype, base[201])
          .expect("pending tombstone");
        let node = db.create_node(None).expect("pending node");
        db.add_edge(node, etype, ids[60]).expect("pending edge");
      }
      let nodes = db.list_nodes().len();
      let edges = db.list_edges(None).len();
      reset_counters();
      let counted_nodes = db.count_nodes();
      let node_visits = examined();
      reset_counters();
      let counted_edges = db.count_edges();
      let edge_visits = examined();
      assert_eq!(
        counted_nodes, nodes,
        "mvcc {mvcc} in_tx {in_tx}: count_nodes"
      );
      if !(mvcc && in_tx) {
        // An MVCC write transaction lists the edges instead (its conflict check notes the
        // sources it read).
        assert_eq!(
          counted_edges, edges,
          "mvcc {mvcc} in_tx {in_tx}: count_edges"
        );
        assert!(
          edge_visits <= 100,
          "mvcc {mvcc} in_tx {in_tx}: count_edges visited {edge_visits} entries for a few \
           deletes and tombstones"
        );
      }
      assert!(
        node_visits <= 100,
        "mvcc {mvcc} in_tx {in_tx}: count_nodes visited {node_visits} entries for a few \
         deletes and a recreate"
      );
      if in_tx {
        db.rollback().expect("rollback");
      }
    }
    close_single_file(db).expect("close");
  }
}

/// `nodes_with_label` over 10000 nodes created since the last checkpoint, 100 of them with
/// the label, and 50 snapshot nodes labeled since, visits about those. Regression: it
/// passed over every created and modified node.
#[test]
fn read_paths_f1_label_listing_over_a_large_delta_visits_the_labeled_nodes() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("temp dir");
    let db = open_single_file(dir.path().join("db.kitedb"), db_options(mvcc)).expect("open");
    let base = delta_nodes(&db, 200, "s");
    db.checkpoint().expect("checkpoint");
    db.begin(false).expect("begin");
    let label = db.define_label("L").expect("label");
    let other = db.define_label("M").expect("label");
    db.commit().expect("commit");
    let ids = delta_nodes(&db, 10_000, "n");
    db.begin(false).expect("begin");
    for &node in ids.iter().step_by(100) {
      db.add_node_label(node, label).expect("label");
    }
    for &node in ids.iter().skip(1).step_by(50) {
      db.add_node_label(node, other).expect("other label");
    }
    for &node in base.iter().step_by(4) {
      db.add_node_label(node, label).expect("label");
    }
    db.remove_node_label(ids[0], label).expect("unlabel");
    db.commit().expect("commit");

    let mut expected: Vec<NodeId> = db
      .list_nodes()
      .into_iter()
      .filter(|&node| db.node_labels(node).contains(&label))
      .collect();
    expected.sort_unstable();
    reset_counters();
    let listed = db.nodes_with_label(label);
    let visited = examined();
    assert_eq!(listed, expected, "mvcc {mvcc}");
    let bound = base.len() + 2 * (100 + 50) + 16;
    assert!(
      visited <= bound,
      "mvcc {mvcc}: nodes_with_label visited {visited} node entries for {} labeled nodes \
       (bound {bound})",
      listed.len()
    );
    close_single_file(db).expect("close");
  }
}

// ============================================================================
// F2: snapshot edge listings
// ============================================================================

fn hub_kite_options(mvcc: bool) -> KiteOptions {
  let mut options = KiteOptions::new()
    .node(NodeDef::new("T", "t:"))
    .node(NodeDef::new("Leaf", "leaf:"))
    .edge(EdgeDef::new("A"))
    .edge(EdgeDef::new("B"));
  options.sync_mode = SyncMode::Off;
  options.mvcc = mvcc;
  options
}

/// Listing every edge of a checkpointed hub (3000 out-edges, 3000 in-edges into a sink),
/// whole or by type, when no layer above the snapshot changes them, checks no edge against
/// those layers. Regression: each edge went through the delete, tombstone and history
/// checks, which made the snapshot listing twice as slow as the delta's (1.17 ms against
/// 597 us for 200000 edges).
#[test]
fn read_paths_f2_snapshot_edge_listing_checks_no_edge_one_by_one() {
  for mvcc in [false, true] {
    let dir = tempdir().expect("temp dir");
    let mut kite = Kite::open(dir.path().join("db.kitedb"), hub_kite_options(mvcc)).expect("open");
    let hub = kite
      .create_node("T", "hub", HashMap::new())
      .expect("hub")
      .id();
    let sink = kite
      .create_node("T", "sink", HashMap::new())
      .expect("sink")
      .id();
    let leaves = 3000;
    let mut leaf_ids = Vec::new();
    kite
      .transaction(|tx| {
        for i in 0..leaves {
          let leaf = tx.create_node("Leaf", &i.to_string(), HashMap::new())?.id();
          tx.link(hub, "A", leaf)?;
          tx.link(leaf, "A", sink)?;
          if i % 600 == 0 {
            tx.link(hub, "B", leaf)?;
          }
          leaf_ids.push(leaf);
        }
        Ok(())
      })
      .expect("graph");
    kite.raw().checkpoint().expect("checkpoint");
    // Changes elsewhere: none of them touch the hub's or the sink's edges.
    kite
      .transaction(|tx| {
        let late = tx.create_node("Leaf", "late", HashMap::new())?.id();
        tx.link(late, "A", leaf_ids[7])?;
        Ok(())
      })
      .expect("unrelated change");

    let db = kite.raw();
    let a = kite.edge_def("A").and_then(|def| def.etype_id).expect("A");
    let full_out = db.out_edges(hub);
    assert_eq!(full_out.len(), leaves + 5);
    let full_in = db.in_edges(sink);
    assert_eq!(full_in.len(), leaves);

    type Case<'k> = (&'static str, usize, Box<dyn Fn() -> usize + 'k>);
    let cases: Vec<Case<'_>> = vec![
      (
        "out_edges(hub)",
        leaves + 5,
        Box::new(|| db.out_edges(hub).len()),
      ),
      (
        "in_edges(sink)",
        leaves,
        Box::new(|| db.in_edges(sink).len()),
      ),
      (
        "out_edges_after(hub, A, all)",
        leaves,
        Box::new(|| db.out_edges_after(hub, Some(a), None, usize::MAX).len()),
      ),
      (
        "in_edges_after(sink, all)",
        leaves,
        Box::new(|| db.in_edges_after(sink, None, None, usize::MAX).len()),
      ),
      (
        "neighbors_out(hub)",
        leaves + 5,
        Box::new(|| kite.neighbors_out(hub, None).expect("neighbors").len()),
      ),
      (
        "neighbors_out(hub, A)",
        leaves,
        Box::new(|| kite.neighbors_out(hub, Some("A")).expect("neighbors").len()),
      ),
      (
        "lazy edges(hub, out)",
        leaves + 5,
        Box::new(|| {
          DbNeighbors::new(db)
            .edges(hub, TraversalDirection::Out, None)
            .count()
        }),
      ),
    ];
    for (name, len, run) in cases {
      reset_counters();
      assert_eq!(run(), len, "mvcc {mvcc}: {name}");
      let checks = overlay_checks();
      assert!(
        checks <= 16,
        "mvcc {mvcc}: {name} checked {checks} of its {len} snapshot edges one by one"
      );
    }

    // A deleted leaf masks its edges: each edge is checked once against the deletes.
    let db = kite.raw();
    db.begin(false).expect("begin");
    db.delete_node(leaf_ids[3]).expect("delete leaf");
    db.commit().expect("commit");
    let out: Vec<NodeId> = db.out_edges(hub).into_iter().map(|(_, dst)| dst).collect();
    assert_eq!(out.len(), leaves + 5 - 1, "mvcc {mvcc}");
    assert!(!out.contains(&leaf_ids[3]), "mvcc {mvcc}");
    let expected: Vec<_> = full_out
      .iter()
      .copied()
      .filter(|&(_, dst)| dst != leaf_ids[3])
      .collect();
    assert_eq!(db.out_edges(hub), expected, "mvcc {mvcc}");
    assert_eq!(
      db.out_edges_after(hub, None, None, usize::MAX),
      expected,
      "mvcc {mvcc}"
    );
    kite.close().expect("close");
  }
}
