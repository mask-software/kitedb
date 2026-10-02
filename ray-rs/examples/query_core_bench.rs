//! Query-path costs that should follow the result size, not the graph size.
//!
//! Sections:
//!   paging  `streaming::nodes_page_single` / `edges_page_single` (what the
//!           bindings' `getNodesPage` / `getEdgesPage` run): page 1 and page
//!           `--page` of `--limit` items, plus the `count_nodes` /
//!           `count_edges` the bindings report as a page's `total`.
//!   types   `Kite::all(type).count()` and `Kite::count_nodes_by_type` over
//!           `--type-nodes` nodes spread over 5 types.
//!   hub     `kite.from(hub).out(None).take(1)` from a node with `--hub-edges`
//!           out-edges, and the hub's whole neighbor list and traversal for
//!           comparison.
//!
//! Each section measures its graph twice: with every change still in the WAL
//! (`delta`) and after a checkpoint folded it into the snapshot (`snapshot`).
//! Each op runs `--repeat` times; the report shows the median and the minimum.
//!
//! Usage:
//!   cargo run --release --example query_core_bench --no-default-features -- [options]
//!
//! Options:
//!   --sections LIST   Comma-separated sections (default: paging,types,hub)
//!   --nodes N         Nodes for `paging` (default: 1000000)
//!   --edges-per-node N  Out-edges per node for `paging` (default: 5)
//!   --limit N         Page size (default: 100)
//!   --page N          The far page measured (default: 1000)
//!   --type-nodes N    Nodes for `types` (default: 100000)
//!   --hub-edges N     Out-edges of the hub for `hub` (default: 200000)
//!   --repeat N        Runs per op (default: 7)
//!   --states LIST     delta,snapshot (default: both)

use std::collections::HashMap;
use std::env;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tempfile::tempdir;

use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef};
use kitedb::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode};
use kitedb::streaming::{edges_page_single, nodes_page_single, PaginationOptions};
use kitedb::types::NodeId;

const TYPES: usize = 5;
const WAL_BYTES: usize = 2 << 30;

struct Config {
  sections: Vec<String>,
  nodes: usize,
  edges_per_node: usize,
  limit: usize,
  page: usize,
  type_nodes: usize,
  hub_edges: usize,
  repeat: usize,
  states: Vec<String>,
}

impl Config {
  fn parse() -> Self {
    let mut config = Self {
      sections: vec!["paging".into(), "types".into(), "hub".into()],
      nodes: 1_000_000,
      edges_per_node: 5,
      limit: 100,
      page: 1000,
      type_nodes: 100_000,
      hub_edges: 200_000,
      repeat: 7,
      states: vec!["delta".into(), "snapshot".into()],
    };
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
      let value = args.get(i + 1).cloned().unwrap_or_default();
      let list = || value.split(',').map(str::to_string).collect::<Vec<_>>();
      let number = || value.parse::<usize>().expect("a number");
      match args[i].as_str() {
        "--sections" => config.sections = list(),
        "--nodes" => config.nodes = number(),
        "--edges-per-node" => config.edges_per_node = number(),
        "--limit" => config.limit = number(),
        "--page" => config.page = number(),
        "--type-nodes" => config.type_nodes = number(),
        "--hub-edges" => config.hub_edges = number(),
        "--repeat" => config.repeat = number(),
        "--states" => config.states = list(),
        other => panic!("unknown option {other}"),
      }
      i += 2;
    }
    config
  }
}

/// Median and minimum of `repeat` runs of `op`.
fn measure<T>(repeat: usize, mut op: impl FnMut() -> T) -> (Duration, Duration) {
  let mut times: Vec<Duration> = (0..repeat.max(1))
    .map(|_| {
      let start = Instant::now();
      std::hint::black_box(op());
      start.elapsed()
    })
    .collect();
  times.sort();
  (times[times.len() / 2], times[0])
}

fn report<T>(state: &str, name: &str, repeat: usize, op: impl FnMut() -> T) {
  let (median, min) = measure(repeat, op);
  println!(
    "  {state:<8} {name:<44} median {:>12.1} us   min {:>12.1} us",
    median.as_secs_f64() * 1e6,
    min.as_secs_f64() * 1e6
  );
}

fn db_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Off)
    .wal_size(WAL_BYTES)
    .auto_checkpoint(false)
}

fn kite_options() -> KiteOptions {
  let mut options = KiteOptions::new();
  for t in 0..TYPES {
    options = options.node(NodeDef::new(&format!("T{t}"), &format!("t{t}:")));
  }
  options = options.node(NodeDef::new("Leaf", "leaf:"));
  options = options.edge(EdgeDef::new("LINK"));
  options.sync_mode = SyncMode::Off;
  options.wal_size = Some(WAL_BYTES);
  options.checkpoint_threshold = Some(1.0);
  options.close_checkpoint_if_wal_usage_at_least = None;
  options
}

fn paging(config: &Config) {
  println!(
    "paging: {} nodes, {} edges, page size {}, pages 1 and {}",
    config.nodes,
    config.nodes * config.edges_per_node,
    config.limit,
    config.page
  );
  let dir = tempdir().expect("temp dir");
  let db = open_single_file(dir.path().join("paging.kitedb"), db_options()).expect("open");
  load_paging_graph(&db, config);
  for state in ["delta", "snapshot"] {
    if state == "snapshot" {
      db.checkpoint().expect("checkpoint");
    }
    if !config.states.iter().any(|s| s == state) {
      continue;
    }
    let far = config.limit * (config.page - 1);
    let node_cursor = db.list_nodes().get(far - 1).map(|id| format!("n:{id}"));
    let mut edges: Vec<_> = db
      .list_edges(None)
      .into_iter()
      .map(|e| (e.src, e.etype, e.dst))
      .collect();
    edges.sort_unstable();
    let edge_cursor = edges
      .get(far - 1)
      .map(|(src, etype, dst)| format!("e:{src}:{etype}:{dst}"));
    drop(edges);
    let page = |cursor: Option<String>| PaginationOptions {
      limit: config.limit,
      cursor,
    };
    report(state, "nodes page 1", config.repeat, || {
      nodes_page_single(&db, page(None))
    });
    report(
      state,
      &format!("nodes page {}", config.page),
      config.repeat,
      || nodes_page_single(&db, page(node_cursor.clone())),
    );
    report(state, "edges page 1", config.repeat, || {
      edges_page_single(&db, page(None))
    });
    report(
      state,
      &format!("edges page {}", config.page),
      config.repeat,
      || edges_page_single(&db, page(edge_cursor.clone())),
    );
    report(state, "count_nodes (page total)", config.repeat, || {
      db.count_nodes()
    });
    report(state, "count_edges (page total)", config.repeat, || {
      db.count_edges()
    });
  }
}

fn load_paging_graph(db: &SingleFileDB, config: &Config) {
  let mut rng = StdRng::seed_from_u64(7);
  db.begin_bulk().expect("begin");
  let etypes: Vec<_> = (0..3)
    .map(|i| db.define_etype(&format!("E{i}")).expect("etype"))
    .collect();
  db.commit().expect("commit");
  let mut ids: Vec<NodeId> = Vec::with_capacity(config.nodes);
  for chunk in (0..config.nodes).collect::<Vec<_>>().chunks(100_000) {
    db.begin_bulk().expect("begin");
    let keys: Vec<String> = chunk.iter().map(|i| format!("n{i}")).collect();
    let keys: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
    ids.extend(db.create_nodes_batch(&keys).expect("nodes"));
    db.commit().expect("commit");
  }
  for chunk in ids.chunks(50_000) {
    db.begin_bulk().expect("begin");
    let mut edges = Vec::with_capacity(chunk.len() * config.edges_per_node);
    for &src in chunk {
      for _ in 0..config.edges_per_node {
        let dst = ids[rng.gen_range(0..ids.len())];
        edges.push((src, etypes[rng.gen_range(0..etypes.len())], dst));
      }
    }
    db.add_edges_batch(&edges).expect("edges");
    db.commit().expect("commit");
  }
}

fn types(config: &Config) {
  println!(
    "types: {} nodes over {TYPES} types, all(T2) and count_nodes_by_type(T2)",
    config.type_nodes
  );
  let dir = tempdir().expect("temp dir");
  let mut kite = Kite::open(dir.path().join("types.kitedb"), kite_options()).expect("open");
  let per_chunk = 10_000;
  let mut created = 0;
  while created < config.type_nodes {
    let end = (created + per_chunk).min(config.type_nodes);
    kite
      .transaction(|tx| {
        for i in created..end {
          tx.create_node(&format!("T{}", i % TYPES), &i.to_string(), HashMap::new())?;
        }
        Ok(())
      })
      .expect("create");
    created = end;
  }
  for state in ["delta", "snapshot"] {
    if state == "snapshot" {
      kite.raw().checkpoint().expect("checkpoint");
    }
    if !config.states.iter().any(|s| s == state) {
      continue;
    }
    report(state, "all(T2).count()", config.repeat, || {
      kite.all("T2").expect("all").count()
    });
    report(state, "count_nodes_by_type(T2)", config.repeat, || {
      kite.count_nodes_by_type("T2").expect("count")
    });
  }
  kite.close().expect("close");
}

fn hub(config: &Config) {
  println!(
    "hub: take(1) from a node with {} out-edges",
    config.hub_edges
  );
  let dir = tempdir().expect("temp dir");
  let mut kite = Kite::open(dir.path().join("hub.kitedb"), kite_options()).expect("open");
  let hub = kite
    .create_node("T0", "hub", HashMap::new())
    .expect("hub")
    .id();
  let per_chunk = 20_000;
  let mut created = 0;
  while created < config.hub_edges {
    let end = (created + per_chunk).min(config.hub_edges);
    kite
      .transaction(|tx| {
        for i in created..end {
          let leaf = tx.create_node("Leaf", &i.to_string(), HashMap::new())?;
          tx.link(hub, "LINK", leaf.id())?;
        }
        Ok(())
      })
      .expect("create");
    created = end;
  }
  for state in ["delta", "snapshot"] {
    if state == "snapshot" {
      kite.raw().checkpoint().expect("checkpoint");
    }
    if !config.states.iter().any(|s| s == state) {
      continue;
    }
    report(state, "from(hub).out(None).take(1)", config.repeat, || {
      kite.from(hub).out(None).expect("out").take(1).to_vec()
    });
    report(state, "from(hub).out(LINK).take(1)", config.repeat, || {
      kite
        .from(hub)
        .out(Some("LINK"))
        .expect("out")
        .take(1)
        .to_vec()
    });
    report(
      state,
      "neighbors_out(hub) (all edges)",
      config.repeat,
      || kite.neighbors_out(hub, None).expect("neighbors").len(),
    );
    report(
      state,
      "neighbors_out(hub, LINK) (all edges)",
      config.repeat,
      || {
        kite
          .neighbors_out(hub, Some("LINK"))
          .expect("neighbors")
          .len()
      },
    );
    report(
      state,
      "from(hub).out(None).to_vec() (all)",
      config.repeat,
      || kite.from(hub).out(None).expect("out").to_vec().len(),
    );
  }
  kite.close().expect("close");
}

fn main() {
  let config = Config::parse();
  for section in &config.sections {
    match section.as_str() {
      "paging" => paging(&config),
      "types" => types(&config),
      "hub" => hub(&config),
      other => panic!("unknown section {other}"),
    }
  }
}
