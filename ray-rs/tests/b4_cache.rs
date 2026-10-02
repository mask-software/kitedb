//! b4 cache lane: the single-file cache layer is dead code.
//!
//! `SingleFileOpenOptions::cache` / `enable_cache()` (and `cacheEnabled` /
//! `cache_enabled` in the Node and Python bindings) build a `CacheManager`, and
//! every write invalidates it under a global lock, but no read path ever looks a
//! value up in it or stores one. The first test asserts what the option
//! advertises, that an enabled cache serves repeated reads, and fails: every
//! counter stays at zero. The ignored test measures what the dead layer costs
//! the write path.

use std::time::{Duration, Instant};

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::metrics::collect_metrics_single_file;
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};

const NODES: usize = 64;
const READ_ROUNDS: usize = 50;

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

/// Reads every key, node prop, edge list, and edge prop `READ_ROUNDS` times
/// through the public read API. Returns the number of read calls made.
fn read_ring(db: &SingleFileDB, g: &Graph) -> usize {
  let mut reads = 0;
  for _ in 0..READ_ROUNDS {
    for (i, &node) in g.nodes.iter().enumerate() {
      let next = g.nodes[(i + 1) % NODES];
      assert_eq!(db.node_by_key(&format!("n{i}")), Some(node));
      assert_eq!(
        db.node_prop(node, g.name),
        Some(PropValue::String(format!("node {i}")))
      );
      assert_eq!(db.node_props(node).map(|props| props.len()), Some(1));
      assert_eq!(db.out_neighbors(node, g.knows), vec![next]);
      assert_eq!(db.out_edges(node), vec![(g.knows, next)]);
      assert_eq!(db.in_edges(next), vec![(g.knows, node)]);
      assert_eq!(
        db.edge_prop(node, g.knows, next, g.weight),
        Some(PropValue::I64(i as i64))
      );
      assert_eq!(
        db.edge_props(node, g.knows, next).map(|props| props.len()),
        Some(1)
      );
      reads += 8;
    }
  }
  reads
}

#[test]
fn enabled_cache_serves_repeated_reads() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("cache.kitedb");
  let db = open_single_file(
    &path,
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .enable_cache(),
  )
  .expect("open");
  assert!(db.cache_is_enabled(), "enable_cache() left the cache off");

  let g = seed_ring(&db);
  // Served from the delta first, then from the snapshot after a checkpoint.
  let mut reads = read_ring(&db, &g);
  db.checkpoint().expect("checkpoint");
  reads += read_ring(&db, &g);

  let stats = db.cache_stats().expect("an enabled cache reports stats");
  let metrics = collect_metrics_single_file(&db).cache;
  close_single_file(db).expect("close");

  // Every cache lookup counts a hit or a miss, so `lookups == 0` means no
  // read consulted the cache, and `entries == 0` means none populated it.
  let lookups = stats.property_cache_hits
    + stats.property_cache_misses
    + stats.traversal_cache_hits
    + stats.traversal_cache_misses
    + stats.query_cache_hits
    + stats.query_cache_misses;
  let hits = stats.property_cache_hits + stats.traversal_cache_hits + stats.query_cache_hits;
  let entries = stats.property_cache_size + stats.traversal_cache_size + stats.query_cache_size;
  assert!(
    metrics.enabled,
    "metrics report the enabled cache as off: {metrics:?}"
  );
  assert!(
    lookups > 0 && hits > 0 && entries > 0,
    "cache enabled, but {reads} reads (each key/prop/edge read {} times) never consulted it: \
     lookups={lookups} hits={hits} entries={entries}\n  cache_stats: {stats:?}\n  metrics: {metrics:?}",
    2 * READ_ROUNDS,
  );
}

// ============================================================================
// Write-path overhead of the dead layer
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
  assert_eq!(db.cache_is_enabled(), cache);

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

/// Time `INVALIDATIONS` direct calls of the hook every write makes
/// (`cache_invalidate_node` / `cache_invalidate_edge`). Returns ns per call.
fn invalidation_calls(cache: bool, edge: bool) -> f64 {
  const INVALIDATIONS: u64 = 10_000_000;
  let dir = tempfile::tempdir().expect("tempdir");
  let mut options = SingleFileOpenOptions::new().sync_mode(SyncMode::Off);
  if cache {
    options = options.enable_cache();
  }
  let db = open_single_file(dir.path().join("hook.kitedb"), options).expect("open");
  let started = Instant::now();
  for i in 0..INVALIDATIONS {
    let node = std::hint::black_box(i % BENCH_NODES as u64);
    if edge {
      db.cache_invalidate_edge(node, 1, node + 1);
    } else {
      db.cache_invalidate_node(node);
    }
  }
  let ns = started.elapsed().as_nanos() as f64 / INVALIDATIONS as f64;
  close_single_file(db).expect("close");
  ns
}

#[test]
#[ignore = "benchmark: cargo test --release --no-default-features --test b4_cache -- --ignored --nocapture"]
fn write_overhead_cache_enabled_vs_disabled() {
  for edge in [false, true] {
    let best = |cache| {
      (0..BENCH_RUNS)
        .map(|_| invalidation_calls(cache, edge))
        .fold(f64::INFINITY, f64::min)
    };
    let (off, on) = (best(false), best(true));
    let hook = if edge { "edge" } else { "node" };
    println!("invalidate_{hook} hook, best of {BENCH_RUNS}: cache off {off:.1} ns/call, on {on:.1} ns/call");
  }

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
       on min {:.0} / median {:.0} (min {:+.1}%, median {:+.1}%)\n  \
       off runs (ms): {:?}\n  on runs (ms):  {:?}",
      ns_per_write(off_min),
      ns_per_write(off_med),
      ns_per_write(on_min),
      ns_per_write(on_med),
      (on_min.as_secs_f64() / off_min.as_secs_f64() - 1.0) * 100.0,
      (on_med.as_secs_f64() / off_med.as_secs_f64() - 1.0) * 100.0,
      ms(&off),
      ms(&on),
    );
  }
}
