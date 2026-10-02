//! What MVCC costs: read and write throughput of single-file KiteDB with MVCC
//! off vs on.
//!
//! Sections:
//!   reads   N reader threads, each running one read op on random nodes
//!   mixed   4 reader threads (`node_prop`) plus 1 writer committing `update_prop`
//!   writes  1 writer thread committing small transactions
//!
//! Modes (each gets a fresh database with the same seeded graph):
//!   off     MVCC disabled
//!   on      MVCC enabled, no transaction left open
//!   reader  MVCC enabled, with one read transaction open for the whole run, so every
//!           commit records versions and nothing in the history can be pruned.
//!           Before measuring, `--history-rounds` commits rewrite a prop and toggle an
//!           edge on `--history-nodes` nodes, so reads walk version history.
//!
//! Read ops (outside a transaction unless noted):
//!   node_prop     db.node_prop(node, prop)
//!   node_props    db.node_props(node)
//!   out_edges     db.out_edges(node)
//!   tx_node_prop  db.node_prop(node, prop) inside read transactions of 1000 reads
//!
//! Write ops (one transaction each):
//!   update_prop   set one prop on a random existing node
//!   insert        create a keyed node, set one prop, add one edge to a random node
//!
//! Each case runs `--repeat` times and reports the median. Sync mode defaults to
//! off so commit costs are CPU costs, not fsync waits.
//!
//! Usage:
//!   cargo run --release --example mvcc_overhead_bench --no-default-features -- [options]
//!
//! Options:
//!   --nodes N             Nodes (default: 20000)
//!   --edges-per-node N    Out-edges per node (default: 8)
//!   --props-per-node N    Properties per node (default: 4)
//!   --threads LIST        Reader thread counts for `reads` (default: 1,4,8)
//!   --duration-ms N       Measurement time per run (default: 1000)
//!   --repeat N            Runs per case; the median is reported (default: 3)
//!   --modes LIST          Comma-separated modes (default: off,on,reader)
//!   --sections LIST       Comma-separated sections (default: reads,mixed,writes)
//!   --ops LIST            Read ops for `reads` (default: node_prop,node_props,out_edges,tx_node_prop)
//!   --write-ops LIST      Write ops for `writes` (default: update_prop,insert)
//!   --history-nodes N     Nodes with version history in `reader` mode (default: 2000)
//!   --history-rounds N    History commits in `reader` mode (default: 8)
//!   --sync-mode MODE      off|normal|full (default: off)

use std::env;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};

const MIXED_READERS: usize = 4;
const TX_READS: usize = 1_000;
const READ_BATCH: usize = 64;

/// Unique suffix for keys created by the `insert` op, across runs and modes.
static INSERT_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
struct BenchConfig {
  nodes: usize,
  edges_per_node: usize,
  props_per_node: usize,
  threads: Vec<usize>,
  duration: Duration,
  repeat: usize,
  modes: Vec<Mode>,
  sections: Vec<Section>,
  ops: Vec<ReadOp>,
  write_ops: Vec<WriteOp>,
  history_nodes: usize,
  history_rounds: usize,
  sync_mode: SyncMode,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      nodes: 20_000,
      edges_per_node: 8,
      props_per_node: 4,
      threads: vec![1, 4, 8],
      duration: Duration::from_millis(1000),
      repeat: 3,
      modes: vec![Mode::Off, Mode::On, Mode::Reader],
      sections: vec![Section::Reads, Section::Mixed, Section::Writes],
      ops: vec![
        ReadOp::NodeProp,
        ReadOp::NodeProps,
        ReadOp::OutEdges,
        ReadOp::TxNodeProp,
      ],
      write_ops: vec![WriteOp::UpdateProp, WriteOp::Insert],
      history_nodes: 2_000,
      history_rounds: 8,
      sync_mode: SyncMode::Off,
    }
  }
}

macro_rules! named_enum {
  ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum $name {
      $($variant),+
    }

    impl $name {
      fn parse(value: &str) -> Option<Self> {
        match value {
          $($text => Some(Self::$variant),)+
          _ => None,
        }
      }

      fn name(self) -> &'static str {
        match self {
          $(Self::$variant => $text,)+
        }
      }
    }
  };
}

named_enum!(Mode { Off => "off", On => "on", Reader => "reader" });
named_enum!(Section { Reads => "reads", Mixed => "mixed", Writes => "writes" });
named_enum!(ReadOp {
  NodeProp => "node_prop",
  NodeProps => "node_props",
  OutEdges => "out_edges",
  TxNodeProp => "tx_node_prop",
});
named_enum!(WriteOp { UpdateProp => "update_prop", Insert => "insert" });

fn parse_list<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Vec<T> {
  value
    .split(',')
    .map(str::trim)
    .filter(|item| !item.is_empty())
    .map(|item| parse(item).unwrap_or_else(|| panic!("unknown list item: {item}")))
    .collect()
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();
  let mut i = 1;
  while i < args.len() {
    let value = args.get(i + 1).map(String::as_str);
    match (args[i].as_str(), value) {
      ("--nodes", Some(v)) => config.nodes = v.parse().expect("--nodes"),
      ("--edges-per-node", Some(v)) => config.edges_per_node = v.parse().expect("--edges-per-node"),
      ("--props-per-node", Some(v)) => config.props_per_node = v.parse().expect("--props-per-node"),
      ("--threads", Some(v)) => config.threads = parse_list(v, |t| t.parse().ok()),
      ("--duration-ms", Some(v)) => {
        config.duration = Duration::from_millis(v.parse().expect("--duration-ms"))
      }
      ("--repeat", Some(v)) => config.repeat = v.parse().expect("--repeat"),
      ("--modes", Some(v)) => config.modes = parse_list(v, Mode::parse),
      ("--sections", Some(v)) => config.sections = parse_list(v, Section::parse),
      ("--ops", Some(v)) => config.ops = parse_list(v, ReadOp::parse),
      ("--write-ops", Some(v)) => config.write_ops = parse_list(v, WriteOp::parse),
      ("--history-nodes", Some(v)) => config.history_nodes = v.parse().expect("--history-nodes"),
      ("--history-rounds", Some(v)) => config.history_rounds = v.parse().expect("--history-rounds"),
      ("--sync-mode", Some(v)) => {
        config.sync_mode = match v {
          "off" => SyncMode::Off,
          "normal" => SyncMode::Normal,
          "full" => SyncMode::Full,
          other => panic!("unknown sync mode: {other}"),
        }
      }
      (flag, _) => panic!("unknown or incomplete option: {flag}"),
    }
    i += 2;
  }
  config.nodes = config.nodes.max(2);
  config.props_per_node = config.props_per_node.max(1);
  config.repeat = config.repeat.max(1);
  config.history_nodes = config.history_nodes.min(config.nodes);
  config
}

struct Graph {
  nodes: Vec<NodeId>,
  props: Vec<PropKeyId>,
  etype: ETypeId,
}

fn seed(db: &SingleFileDB, config: &BenchConfig) -> Graph {
  const BATCH: usize = 2_000;
  db.begin(false).expect("begin schema");
  let etype = db.define_etype("LINK").expect("define etype");
  let props: Vec<PropKeyId> = (0..config.props_per_node)
    .map(|p| db.define_propkey(&format!("p{p}")).expect("define propkey"))
    .collect();
  db.commit().expect("commit schema");

  let mut nodes = Vec::with_capacity(config.nodes);
  for start in (0..config.nodes).step_by(BATCH) {
    db.begin(false).expect("begin nodes");
    for i in start..(start + BATCH).min(config.nodes) {
      let node = db.create_node(Some(&format!("n{i}"))).expect("create node");
      for (p, &prop) in props.iter().enumerate() {
        db.set_node_prop(node, prop, PropValue::I64((i * 31 + p) as i64))
          .expect("set prop");
      }
      nodes.push(node);
    }
    db.commit().expect("commit nodes");
  }

  let mut rng = XorShift::new(0x5eed);
  for start in (0..config.nodes).step_by(BATCH) {
    db.begin(false).expect("begin edges");
    for &src in &nodes[start..(start + BATCH).min(config.nodes)] {
      for _ in 0..config.edges_per_node {
        let dst = nodes[rng.next_index(nodes.len())];
        db.add_edge(src, etype, dst).expect("add edge");
      }
    }
    db.commit().expect("commit edges");
  }
  db.checkpoint().expect("checkpoint seed");
  Graph {
    nodes,
    props,
    etype,
  }
}

/// Holds a read transaction open on its own thread until dropped.
struct OpenReader {
  release: Option<mpsc::Sender<()>>,
  handle: Option<thread::JoinHandle<()>>,
}

impl OpenReader {
  fn open(db: &Arc<SingleFileDB>) -> Self {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let db = Arc::clone(db);
    let handle = thread::spawn(move || {
      db.begin(true).expect("begin reader");
      ready_tx.send(()).expect("reader ready");
      let _ = release_rx.recv();
      db.commit().expect("end reader");
    });
    ready_rx.recv().expect("reader began");
    Self {
      release: Some(release_tx),
      handle: Some(handle),
    }
  }
}

impl Drop for OpenReader {
  fn drop(&mut self) {
    drop(self.release.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

/// Rewrites a prop and toggles an edge on the first `history_nodes` nodes,
/// `history_rounds` times, while the open reader keeps every version alive.
fn build_history(db: &SingleFileDB, graph: &Graph, config: &BenchConfig) {
  let prop = graph.props[0];
  for round in 0..config.history_rounds {
    db.begin(false).expect("begin history");
    for (i, &node) in graph.nodes[..config.history_nodes].iter().enumerate() {
      db.set_node_prop(node, prop, PropValue::I64((round * 1_000_003 + i) as i64))
        .expect("rewrite prop");
      let other = graph.nodes[(i + 1) % graph.nodes.len()];
      if round % 2 == 0 {
        db.add_edge(node, graph.etype, other)
          .expect("add history edge");
      } else {
        db.delete_edge(node, graph.etype, other)
          .expect("delete history edge");
      }
    }
    db.commit().expect("commit history");
  }
}

struct XorShift(u64);

impl XorShift {
  fn new(seed: u64) -> Self {
    Self(seed.max(1))
  }

  fn next_index(&mut self, len: usize) -> usize {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    (x % len as u64) as usize
  }
}

/// Runs one read op until `stop`; returns the number of reads.
fn read_loop(db: &SingleFileDB, graph: &Graph, op: ReadOp, seed: u64, stop: &AtomicBool) -> u64 {
  let mut rng = XorShift::new(seed);
  let prop = graph.props[0];
  let mut reads = 0u64;
  let mut sink = 0usize;
  while !stop.load(Ordering::Relaxed) {
    let batch = if op == ReadOp::TxNodeProp {
      db.begin(true).expect("begin read tx");
      TX_READS
    } else {
      READ_BATCH
    };
    for _ in 0..batch {
      let node = graph.nodes[rng.next_index(graph.nodes.len())];
      sink += match op {
        ReadOp::NodeProp | ReadOp::TxNodeProp => db.node_prop(node, prop).is_some() as usize,
        ReadOp::NodeProps => db.node_props(node).map_or(0, |props| props.len()),
        ReadOp::OutEdges => db.out_edges(node).len(),
      };
    }
    if op == ReadOp::TxNodeProp {
      db.commit().expect("end read tx");
    }
    reads += batch as u64;
  }
  std::hint::black_box(sink);
  reads
}

/// Commits one small transaction per iteration until `stop`; returns the number of commits.
fn write_loop(db: &SingleFileDB, graph: &Graph, op: WriteOp, seed: u64, stop: &AtomicBool) -> u64 {
  let mut rng = XorShift::new(seed);
  let prop = graph.props[0];
  let mut commits = 0u64;
  while !stop.load(Ordering::Relaxed) {
    db.begin(false).expect("begin write tx");
    match op {
      WriteOp::UpdateProp => {
        let node = graph.nodes[rng.next_index(graph.nodes.len())];
        db.set_node_prop(node, prop, PropValue::I64(commits as i64))
          .expect("update prop");
      }
      WriteOp::Insert => {
        let seq = INSERT_SEQ.fetch_add(1, Ordering::Relaxed);
        let node = db
          .create_node(Some(&format!("w{seq}")))
          .expect("insert node");
        db.set_node_prop(node, prop, PropValue::I64(seq as i64))
          .expect("insert prop");
        let dst = graph.nodes[rng.next_index(graph.nodes.len())];
        db.add_edge(node, graph.etype, dst).expect("insert edge");
      }
    }
    db.commit().expect("commit write tx");
    commits += 1;
  }
  commits
}

/// Runs `readers` threads of `read_op` and, if given, one writer for `duration`.
/// Returns (reads/s, commits/s).
fn run_once(
  db: &Arc<SingleFileDB>,
  graph: &Arc<Graph>,
  readers: usize,
  read_op: ReadOp,
  writer: Option<WriteOp>,
  duration: Duration,
) -> (f64, f64) {
  let stop = Arc::new(AtomicBool::new(false));
  let workers = readers + usize::from(writer.is_some());
  let barrier = Arc::new(Barrier::new(workers + 1));

  let spawn = |work: Box<dyn FnOnce(&SingleFileDB, &Graph, &AtomicBool) -> u64 + Send>| {
    let db = Arc::clone(db);
    let graph = Arc::clone(graph);
    let stop = Arc::clone(&stop);
    let barrier = Arc::clone(&barrier);
    thread::spawn(move || {
      barrier.wait();
      work(&db, &graph, &stop)
    })
  };

  let reader_handles: Vec<_> = (0..readers)
    .map(|t| {
      let seed = 0x9e37_79b9_7f4a_7c15 ^ (t as u64 + 1);
      spawn(Box::new(move |db, graph, stop| {
        read_loop(db, graph, read_op, seed, stop)
      }))
    })
    .collect();
  let writer_handle = writer.map(|op| {
    spawn(Box::new(move |db, graph, stop| {
      write_loop(db, graph, op, 0xdead_beef, stop)
    }))
  });

  barrier.wait();
  let started = Instant::now();
  thread::sleep(duration);
  stop.store(true, Ordering::Relaxed);
  let reads: u64 = reader_handles
    .into_iter()
    .map(|handle| handle.join().expect("reader thread"))
    .sum();
  let commits = writer_handle.map_or(0, |handle| handle.join().expect("writer thread"));
  let secs = started.elapsed().as_secs_f64();
  (reads as f64 / secs, commits as f64 / secs)
}

fn median(mut values: Vec<f64>) -> f64 {
  values.sort_by(f64::total_cmp);
  values[values.len() / 2]
}

/// One measured number for one mode.
struct Row {
  section: Section,
  op: &'static str,
  threads: String,
  metric: &'static str,
  mode: Mode,
  value: f64,
}

fn bench_mode(mode: Mode, config: &BenchConfig, rows: &mut Vec<Row>) {
  let dir = tempdir().expect("tempdir");
  let options = SingleFileOpenOptions::new()
    .mvcc(mode != Mode::Off)
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .sync_mode(config.sync_mode)
    .wal_size(1024 * 1024 * 1024);
  let db = Arc::new(open_single_file(dir.path().join("bench.kitedb"), options).expect("open"));
  let graph = Arc::new(seed(&db, config));
  let reader = (mode == Mode::Reader).then(|| {
    let reader = OpenReader::open(&db);
    build_history(&db, &graph, config);
    reader
  });

  let measure = |readers: usize, read_op: ReadOp, writer: Option<WriteOp>| {
    let runs: Vec<(f64, f64)> = (0..config.repeat)
      .map(|_| run_once(&db, &graph, readers, read_op, writer, config.duration))
      .collect();
    (
      median(runs.iter().map(|run| run.0).collect()),
      median(runs.iter().map(|run| run.1).collect()),
    )
  };

  for &section in &config.sections {
    match section {
      Section::Reads => {
        for &op in &config.ops {
          for &threads in &config.threads {
            let (reads, _) = measure(threads, op, None);
            rows.push(Row {
              section,
              op: op.name(),
              threads: threads.to_string(),
              metric: "reads/s",
              mode,
              value: reads,
            });
          }
        }
      }
      Section::Mixed => {
        let (reads, commits) = measure(MIXED_READERS, ReadOp::NodeProp, Some(WriteOp::UpdateProp));
        let threads = format!("{MIXED_READERS}r+1w");
        rows.push(Row {
          section,
          op: ReadOp::NodeProp.name(),
          threads: threads.clone(),
          metric: "reads/s",
          mode,
          value: reads,
        });
        rows.push(Row {
          section,
          op: WriteOp::UpdateProp.name(),
          threads,
          metric: "commits/s",
          mode,
          value: commits,
        });
      }
      Section::Writes => {
        for &op in &config.write_ops {
          let (_, commits) = measure(0, ReadOp::NodeProp, Some(op));
          rows.push(Row {
            section,
            op: op.name(),
            threads: "1w".to_string(),
            metric: "commits/s",
            mode,
            value: commits,
          });
        }
      }
    }
  }

  drop(reader);
  drop(graph);
  let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still shared"));
  close_single_file(db).expect("close");
}

fn print_table(config: &BenchConfig, rows: &[Row]) {
  let mut header = format!(
    "{:<7} {:<13} {:>7} {:<10}",
    "section", "op", "threads", "metric"
  );
  for &mode in &config.modes {
    header.push_str(&format!(" {:>12}", mode.name()));
    if mode != Mode::Off && config.modes.contains(&Mode::Off) {
      header.push_str(&format!(" {:>8}", "vs off"));
    }
  }
  println!("{header}");

  let mut seen: Vec<(Section, &str, &str, &str)> = Vec::new();
  for row in rows {
    let key = (row.section, row.op, row.threads.as_str(), row.metric);
    if seen.contains(&key) {
      continue;
    }
    seen.push(key);
    let value = |mode: Mode| {
      rows
        .iter()
        .find(|r| (r.section, r.op, r.threads.as_str(), r.metric) == key && r.mode == mode)
        .map(|r| r.value)
    };
    let off = value(Mode::Off);
    let mut line = format!(
      "{:<7} {:<13} {:>7} {:<10}",
      row.section.name(),
      row.op,
      row.threads,
      row.metric
    );
    for &mode in &config.modes {
      let v = value(mode);
      line.push_str(&match v {
        Some(v) => format!(" {v:>12.0}"),
        None => format!(" {:>12}", "-"),
      });
      if mode != Mode::Off && config.modes.contains(&Mode::Off) {
        line.push_str(&match (v, off) {
          (Some(v), Some(off)) if off > 0.0 => format!(" {:>+7.1}%", (v / off - 1.0) * 100.0),
          _ => format!(" {:>8}", "-"),
        });
      }
    }
    println!("{line}");
  }
}

fn main() {
  let config = parse_args();
  println!(
    "mvcc_overhead_bench: nodes={} edges/node={} props/node={} duration={:?} repeat={} \
     history={}x{} sync={:?} cpus={}",
    config.nodes,
    config.edges_per_node,
    config.props_per_node,
    config.duration,
    config.repeat,
    config.history_nodes,
    config.history_rounds,
    config.sync_mode,
    thread::available_parallelism().map_or(0, |n| n.get())
  );

  let mut rows = Vec::new();
  for &mode in &config.modes {
    let started = Instant::now();
    bench_mode(mode, &config, &mut rows);
    eprintln!("mode {} done in {:?}", mode.name(), started.elapsed());
  }
  print_table(&config, &rows);
}
