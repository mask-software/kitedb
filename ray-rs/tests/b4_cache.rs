//! b4 cache lane: the single-file cache layer was removed.
//!
//! It was never read: no lookup consulted it, so enabling it gave zero hits
//! and only added a global lock plus invalidation work to every write. The
//! open options that configured it (`SingleFileOpenOptions::cache`,
//! `enable_cache()`) are still accepted, deprecated, and have no effect. These
//! tests pin that down; the ignored test measures the write path with the
//! options on and off.

// The deprecated cache options are exercised on purpose.
#![allow(deprecated)]

use std::path::Path;
use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::{
  CacheOptions, ETypeId, NodeId, PropKeyId, PropValue, PropertyCacheConfig, QueryCacheConfig,
  TraversalCacheConfig,
};

const NODES: usize = 64;

struct Graph {
  nodes: Vec<NodeId>,
  knows: ETypeId,
  name: PropKeyId,
  weight: PropKeyId,
}

/// A ring of `NODES` keyed nodes, each with a `name` prop and one outgoing
/// `KNOWS` edge carrying a `weight` prop.
fn seed_ring(db: &SingleFileDB) -> Graph {
  db.begin(false).expect("begin");
  let knows = db.define_etype("KNOWS").expect("etype");
  let name = db.define_propkey("name").expect("name");
  let weight = db.define_propkey("weight").expect("weight");
  let nodes: Vec<NodeId> = (0..NODES)
    .map(|i| db.create_node(Some(&format!("n{i}"))).expect("node"))
    .collect();
  for (i, &node) in nodes.iter().enumerate() {
    db.set_node_prop(node, name, PropValue::String(format!("node {i}")))
      .expect("name");
    let next = nodes[(i + 1) % NODES];
    db.add_edge_with_props(node, knows, next, vec![(weight, PropValue::I64(i as i64))])
      .expect("edge");
  }
  db.commit().expect("commit");
  Graph {
    nodes,
    knows,
    name,
    weight,
  }
}

/// Reads every key, node prop, edge list, and edge prop twice through the
/// public read API, expecting node `i` to be named `name_of(i)`.
fn read_ring(db: &SingleFileDB, g: &Graph, name_of: impl Fn(usize) -> String) {
  for _ in 0..2 {
    for (i, &node) in g.nodes.iter().enumerate() {
      let next = g.nodes[(i + 1) % NODES];
      assert_eq!(db.node_by_key(&format!("n{i}")), Some(node));
      assert_eq!(
        db.node_prop(node, g.name),
        Some(PropValue::String(name_of(i)))
      );
      assert_eq!(db.node_props(node).map(|props| props.len()), Some(1));
      assert_eq!(db.out_neighbors(node, g.knows), vec![next]);
      assert_eq!(db.out_edges(node), vec![(g.knows, next)]);
      assert_eq!(db.in_edges(next), vec![(g.knows, node)]);
      assert_eq!(
        db.edge_prop(node, g.knows, next, g.weight),
        Some(PropValue::I64(i as i64))
      );
    }
  }
}

/// Seeds, reads, overwrites every name, reads again, checkpoints, and reopens,
/// checking every read sees the latest write.
fn exercise(path: &Path, options: SingleFileOpenOptions) {
  let db = open_single_file(path, options.clone()).expect("open");
  let g = seed_ring(&db);
  read_ring(&db, &g, |i| format!("node {i}"));

  db.begin(false).expect("begin");
  for (i, &node) in g.nodes.iter().enumerate() {
    db.set_node_prop(node, g.name, PropValue::String(format!("renamed {i}")))
      .expect("rename");
  }
  db.commit().expect("commit");
  read_ring(&db, &g, |i| format!("renamed {i}"));

  db.checkpoint().expect("checkpoint");
  read_ring(&db, &g, |i| format!("renamed {i}"));
  close_single_file(db).expect("close");

  let db = open_single_file(path, options).expect("reopen");
  read_ring(&db, &g, |i| format!("renamed {i}"));
  close_single_file(db).expect("close");
}

#[test]
fn cache_options_are_accepted_and_ignored() {
  let base = || SingleFileOpenOptions::new().auto_checkpoint(false);
  let tiny_caches = CacheOptions {
    enabled: true,
    property_cache: Some(PropertyCacheConfig {
      max_node_props: 0,
      max_edge_props: 1,
    }),
    traversal_cache: Some(TraversalCacheConfig {
      max_entries: 1,
      max_neighbors_per_entry: 0,
    }),
    query_cache: Some(QueryCacheConfig {
      max_entries: 0,
      ttl_ms: Some(0),
    }),
  };
  let variants = [
    ("plain", base()),
    ("enable_cache", base().enable_cache()),
    ("cache(tiny)", base().cache(Some(tiny_caches))),
    (
      "cache(disabled)",
      base().cache(Some(CacheOptions::default())),
    ),
  ];

  let dir = tempfile::tempdir().expect("tempdir");
  for (name, options) in variants {
    exercise(&dir.path().join(format!("{name}.kitedb")), options);
  }
}

// ============================================================================
// Write path, cache options on vs off
// ============================================================================

const BENCH_NODES: usize = 1_000;
const BENCH_WRITES: usize = 100_000;
const BENCH_RUNS: usize = 7;

#[derive(Clone, Copy, Debug)]
enum Shape {
  /// One `set_node_prop` per transaction.
  OneProp,
  /// 100 `set_node_prop` per transaction.
  HundredProps,
  /// 100 new `add_edge` per transaction.
  HundredEdges,
}

fn commit_loop(shape: Shape, cache: bool) -> Duration {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("bench.kitedb");
  let mut options = SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Off)
    .auto_checkpoint(false)
    .wal_size(256 << 20);
  if cache {
    options = options.enable_cache();
  }
  let db = open_single_file(&path, options).expect("open");

  db.begin(false).expect("begin");
  let knows = db.define_etype("KNOWS").expect("etype");
  let value = db.define_propkey("value").expect("propkey");
  let nodes: Vec<NodeId> = (0..BENCH_NODES)
    .map(|_| db.create_node(None).expect("node"))
    .collect();
  db.commit().expect("commit");

  let per_tx = match shape {
    Shape::OneProp => 1,
    Shape::HundredProps | Shape::HundredEdges => 100,
  };
  let started = Instant::now();
  for tx in 0..BENCH_WRITES / per_tx {
    db.begin(false).expect("begin");
    for op in 0..per_tx {
      let i = tx * per_tx + op;
      let src = nodes[i % BENCH_NODES];
      match shape {
        Shape::OneProp | Shape::HundredProps => {
          db.set_node_prop(src, value, PropValue::I64(i as i64))
            .expect("set prop");
        }
        Shape::HundredEdges => {
          // i / BENCH_NODES < 100, so every (src, dst) pair is new.
          let dst = nodes[(i % BENCH_NODES + i / BENCH_NODES + 1) % BENCH_NODES];
          db.add_edge(src, knows, dst).expect("add edge");
        }
      }
    }
    db.commit().expect("commit");
  }
  let elapsed = started.elapsed();
  close_single_file(db).expect("close");
  elapsed
}

fn median(mut runs: Vec<Duration>) -> Duration {
  runs.sort();
  runs[runs.len() / 2]
}

fn ms(runs: &[Duration]) -> Vec<u128> {
  runs.iter().map(Duration::as_millis).collect()
}

#[test]
#[ignore = "benchmark: cargo test --release --no-default-features --test b4_cache -- --ignored --nocapture"]
fn write_path_cache_options_on_vs_off() {
  for shape in [Shape::OneProp, Shape::HundredProps, Shape::HundredEdges] {
    commit_loop(shape, false);
    commit_loop(shape, true);
    let mut off = Vec::new();
    let mut on = Vec::new();
    // Interleaved so drift (load, thermal, page cache) hits both sides alike.
    for _ in 0..BENCH_RUNS {
      off.push(commit_loop(shape, false));
      on.push(commit_loop(shape, true));
    }
    let ns_per_write = |d: Duration| d.as_nanos() as f64 / BENCH_WRITES as f64;
    let (off_min, on_min) = (
      *off.iter().min().expect("runs"),
      *on.iter().min().expect("runs"),
    );
    let (off_med, on_med) = (median(off.clone()), median(on.clone()));
    println!(
      "{shape:?}: {BENCH_WRITES} writes, ns/write cache off min {:.0} / median {:.0}, \
       on min {:.0} / median {:.0}\n  off runs (ms): {:?}\n  on runs (ms):  {:?}",
      ns_per_write(off_min),
      ns_per_write(off_med),
      ns_per_write(on_min),
      ns_per_write(on_med),
      ms(&off),
      ms(&on),
    );
  }
}
