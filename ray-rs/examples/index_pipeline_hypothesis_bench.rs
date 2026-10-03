//! Index pipeline hypothesis benchmark for code intelligence workloads.
//!
//! Tests two modes:
//! 1) Sequential: tree-sitter parse -> TS graph write -> SCIP parse -> SCIP graph write ->
//!    embed (simulated network) -> vector write.
//! 2) Parallel: tree-sitter + SCIP parse in parallel -> unified graph write -> enqueue;
//!    async embed workers batch results; vector writer applies batched writes.
//!
//! Goal: verify whether network latency dominates enough that async batching is the
//! right architecture choice.
//!
//! Usage:
//!   cargo run --release --example index_pipeline_hypothesis_bench --no-default-features -- [options]
//!
//! Options:
//!   --mode MODE                    sequential|parallel|both (default: both)
//!   --changes N                    Number of change events (default: 20000)
//!   --working-set N                Distinct chunk keys reused by events (default: 2000)
//!   --vector-dims N                Vector dimensions (default: 128)
//!   --tree-sitter-latency-ms N     Simulated tree-sitter parse latency per event (default: 0)
//!   --scip-latency-ms N            Simulated SCIP parse latency per event (default: 0)
//!   --embed-latency-ms N           Simulated remote embedding latency per batch (default: 200)
//!   --embed-batch-size N           Embedding request batch size (default: 64)
//!   --embed-flush-ms N             Max wait to fill embed batch (default: 25)
//!   --embed-inflight N             Parallel embedding requests (default: 4)
//!   --vector-apply-batch-size N    Vector writes per DB transaction (default: 256)
//!   --wal-size BYTES               WAL size in bytes (default: 1073741824)
//!   --sync-mode MODE               Sync mode: full|normal|off (default: normal)
//!   --group-commit-enabled, --group-commit-window-ms N
//!                                  Accepted for old command lines; no effect
//!                                  (every commit is group-committed)
//!   --auto-checkpoint              Enable auto-checkpoint (default: false)
//!   --seed N                       RNG seed for event generation (default: 42)
//!   --mvcc | --no-mvcc             MVCC mode (default: the library default)
//!   --keep-db                      Keep generated DB files for inspection
//!
//! Unknown options are an error.

use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use rand::{rngs::StdRng, Rng, SeedableRng};
use tempfile::tempdir;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
  Sequential,
  Parallel,
  Both,
}

#[derive(Debug, Clone)]
struct BenchConfig {
  mode: Mode,
  changes: usize,
  working_set: usize,
  vector_dims: usize,
  tree_sitter_latency_ms: u64,
  scip_latency_ms: u64,
  embed_latency_ms: u64,
  embed_batch_size: usize,
  embed_flush_ms: u64,
  embed_inflight: usize,
  vector_apply_batch_size: usize,
  wal_size: usize,
  sync_mode: SyncMode,
  auto_checkpoint: bool,
  seed: u64,
  /// None: the library default.
  mvcc: Option<bool>,
  keep_db: bool,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      mode: Mode::Both,
      changes: 20_000,
      working_set: 2_000,
      vector_dims: 128,
      tree_sitter_latency_ms: 0,
      scip_latency_ms: 0,
      embed_latency_ms: 200,
      embed_batch_size: 64,
      embed_flush_ms: 25,
      embed_inflight: 4,
      vector_apply_batch_size: 256,
      wal_size: 1024 * 1024 * 1024,
      sync_mode: SyncMode::Normal,
      auto_checkpoint: false,
      seed: 42,
      mvcc: None,
      keep_db: false,
    }
  }
}

#[derive(Debug, Clone)]
struct ChangeEvent {
  chunk_idx: usize,
  version: u64,
}

#[derive(Debug, Clone)]
struct EmbedJob {
  chunk_idx: usize,
  version: u64,
  hot_done_at: Instant,
}

#[derive(Debug, Default)]
struct QueueStats {
  enqueued_jobs: u64,
  replaced_jobs: u64,
  max_depth: usize,
  depth_sum: u128,
  depth_samples: u64,
}

#[derive(Debug)]
struct EmbedQueueState {
  pending_by_chunk: HashMap<usize, EmbedJob>,
  order: VecDeque<usize>,
  closed: bool,
  stats: QueueStats,
}

impl EmbedQueueState {
  fn new(capacity: usize) -> Self {
    Self {
      pending_by_chunk: HashMap::with_capacity(capacity),
      order: VecDeque::with_capacity(capacity),
      closed: false,
      stats: QueueStats::default(),
    }
  }

  fn sample_depth(&mut self) {
    let depth = self.pending_by_chunk.len();
    self.stats.max_depth = self.stats.max_depth.max(depth);
    self.stats.depth_sum += depth as u128;
    self.stats.depth_samples += 1;
  }
}

struct DbFixture {
  db: Arc<SingleFileDB>,
  node_ids: Vec<NodeId>,
  etype_rel: ETypeId,
  node_rev_key: PropKeyId,
  node_scip_rev_key: PropKeyId,
  edge_weight_key: PropKeyId,
  vector_key: PropKeyId,
  db_path: PathBuf,
  temp_dir: tempfile::TempDir,
}

#[derive(Debug, Default)]
struct BenchResult {
  mode: &'static str,
  changes: usize,
  applied_vectors: usize,
  total_elapsed: Duration,
  hot_path_elapsed: Duration,
  hot_path_ns: Vec<u128>,
  vector_freshness_ns: Vec<u128>,
  enqueued_jobs: u64,
  replaced_jobs: u64,
  queue_max_depth: usize,
  queue_avg_depth: f64,
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/index_pipeline_hypothesis_bench.rs for the options");
  exit(2);
}

fn value<T: std::str::FromStr>(args: &[String], i: &mut usize, flag: &str) -> T {
  *i += 1;
  let Some(raw) = args.get(*i) else {
    usage_error(&format!("{flag} needs a value"));
  };
  raw
    .parse()
    .unwrap_or_else(|_| usage_error(&format!("invalid value for {flag}: {raw}")))
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();
  let mut i = 1;

  while i < args.len() {
    let flag = args[i].as_str();
    match flag {
      "--mode" => {
        let mode: String = value(&args, &mut i, flag);
        config.mode = match mode.to_lowercase().as_str() {
          "sequential" => Mode::Sequential,
          "parallel" => Mode::Parallel,
          "both" => Mode::Both,
          other => usage_error(&format!(
            "--mode must be sequential, parallel or both, not {other}"
          )),
        };
      }
      "--changes" => config.changes = value(&args, &mut i, flag),
      "--working-set" => config.working_set = value(&args, &mut i, flag),
      "--vector-dims" => config.vector_dims = value(&args, &mut i, flag),
      "--tree-sitter-latency-ms" => config.tree_sitter_latency_ms = value(&args, &mut i, flag),
      "--scip-latency-ms" => config.scip_latency_ms = value(&args, &mut i, flag),
      "--embed-latency-ms" => config.embed_latency_ms = value(&args, &mut i, flag),
      "--embed-batch-size" => config.embed_batch_size = value(&args, &mut i, flag),
      "--embed-flush-ms" => config.embed_flush_ms = value(&args, &mut i, flag),
      "--embed-inflight" => config.embed_inflight = value(&args, &mut i, flag),
      "--vector-apply-batch-size" => config.vector_apply_batch_size = value(&args, &mut i, flag),
      "--wal-size" => config.wal_size = value(&args, &mut i, flag),
      "--sync-mode" => {
        let mode: String = value(&args, &mut i, flag);
        config.sync_mode = match mode.to_lowercase().as_str() {
          "full" => SyncMode::Full,
          "normal" => SyncMode::Normal,
          "off" => SyncMode::Off,
          other => usage_error(&format!(
            "--sync-mode must be full, normal or off, not {other}"
          )),
        };
      }
      // No effect since every commit is group-committed; accepted so old
      // command lines still run.
      "--group-commit-enabled" => {}
      "--group-commit-window-ms" => {
        let _: u64 = value(&args, &mut i, flag);
      }
      "--auto-checkpoint" => config.auto_checkpoint = true,
      "--seed" => config.seed = value(&args, &mut i, flag),
      "--mvcc" => config.mvcc = Some(true),
      "--no-mvcc" => config.mvcc = Some(false),
      "--keep-db" => config.keep_db = true,
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }

  if config.changes == 0 {
    config.changes = 1;
  }
  if config.working_set == 0 {
    config.working_set = 1;
  }
  if config.vector_dims == 0 {
    config.vector_dims = 1;
  }
  if config.embed_batch_size == 0 {
    config.embed_batch_size = 1;
  }
  if config.embed_inflight == 0 {
    config.embed_inflight = 1;
  }
  if config.vector_apply_batch_size == 0 {
    config.vector_apply_batch_size = 1;
  }

  config
}

/// The MVCC mode the bench runs in, for its header.
fn mvcc_label(requested: Option<bool>) -> String {
  let on = requested.unwrap_or(SingleFileOpenOptions::new().mvcc);
  format!(
    "{}{}",
    if on { "on" } else { "off" },
    if requested.is_none() {
      " (library default)"
    } else {
      ""
    }
  )
}

fn generate_events(config: &BenchConfig) -> Vec<ChangeEvent> {
  let mut rng = StdRng::seed_from_u64(config.seed);
  let mut versions = vec![0u64; config.working_set];
  let mut events = Vec::with_capacity(config.changes);

  for _ in 0..config.changes {
    let chunk_idx = rng.gen_range(0..config.working_set);
    versions[chunk_idx] += 1;
    events.push(ChangeEvent {
      chunk_idx,
      version: versions[chunk_idx],
    });
  }

  events
}

fn format_rate(count: usize, elapsed: Duration) -> String {
  let seconds = elapsed.as_secs_f64();
  if seconds <= 0.0 {
    return "n/a".to_string();
  }
  let rate = count as f64 / seconds;
  if rate >= 1_000_000.0 {
    return format!("{:.2}M/s", rate / 1_000_000.0);
  }
  if rate >= 1_000.0 {
    return format!("{:.2}K/s", rate / 1_000.0);
  }
  format!("{rate:.2}/s")
}

fn format_latency_ns(ns: u128) -> String {
  if ns < 1_000 {
    format!("{ns}ns")
  } else if ns < 1_000_000 {
    format!("{:.2}us", ns as f64 / 1_000.0)
  } else if ns < 1_000_000_000 {
    format!("{:.2}ms", ns as f64 / 1_000_000.0)
  } else {
    format!("{:.2}s", ns as f64 / 1_000_000_000.0)
  }
}

fn percentile_ns(samples: &[u128], percentile: f64) -> u128 {
  if samples.is_empty() {
    return 0;
  }
  let mut sorted = samples.to_vec();
  sorted.sort_unstable();
  let idx = ((sorted.len() as f64) * percentile).floor() as usize;
  sorted[idx.min(sorted.len() - 1)]
}

fn setup_fixture(config: &BenchConfig, label: &str) -> DbFixture {
  let temp_dir = tempdir().expect("expected value");
  let db_path = temp_dir
    .path()
    .join(format!("index-pipeline-{label}.kitedb"));

  let mut open_opts = SingleFileOpenOptions::new()
    .wal_size(config.wal_size)
    .sync_mode(config.sync_mode)
    .auto_checkpoint(config.auto_checkpoint);
  if let Some(mvcc) = config.mvcc {
    open_opts = open_opts.mvcc(mvcc);
  }

  let db = open_single_file(&db_path, open_opts).expect("expected value");
  let db = Arc::new(db);

  db.begin(false).expect("expected value");
  let etype_rel = db.define_etype("REL").expect("expected value");
  let node_rev_key = db.define_propkey("rev").expect("expected value");
  let node_scip_rev_key = db.define_propkey("scip_rev").expect("expected value");
  let edge_weight_key = db.define_propkey("weight").expect("expected value");
  let vector_key = db.define_propkey("embedding").expect("expected value");
  db.commit().expect("expected value");

  let mut node_ids = Vec::with_capacity(config.working_set);
  let create_batch = 5000usize;
  for start in (0..config.working_set).step_by(create_batch) {
    let end = (start + create_batch).min(config.working_set);
    db.begin_bulk().expect("expected value");
    let mut keys = Vec::with_capacity(end - start);
    for idx in start..end {
      keys.push(format!("chunk:{idx}"));
    }
    let key_refs: Vec<Option<&str>> = keys.iter().map(|k| Some(k.as_str())).collect();
    let ids = db.create_nodes_batch(&key_refs).expect("expected value");
    node_ids.extend(ids);
    db.commit().expect("expected value");
  }

  let edge_batch = 10_000usize;
  for start in (0..config.working_set).step_by(edge_batch) {
    let end = (start + edge_batch).min(config.working_set);
    db.begin_bulk().expect("expected value");
    let mut edges = Vec::with_capacity(end - start);
    for idx in start..end {
      let src = node_ids[idx];
      let dst = node_ids[(idx + 1) % node_ids.len()];
      edges.push((src, etype_rel, dst));
    }
    db.add_edges_batch(&edges).expect("expected value");
    db.commit().expect("expected value");
  }

  db.vector_store_or_create(vector_key, config.vector_dims)
    .expect("expected value");

  DbFixture {
    db,
    node_ids,
    etype_rel,
    node_rev_key,
    node_scip_rev_key,
    edge_weight_key,
    vector_key,
    db_path,
    temp_dir,
  }
}

fn apply_graph_change_ts_tx(fixture: &DbFixture, event: &ChangeEvent) {
  let src = fixture.node_ids[event.chunk_idx];
  let dst = fixture.node_ids[(event.chunk_idx + 1) % fixture.node_ids.len()];

  fixture.db.begin(false).expect("expected value");
  fixture
    .db
    .set_node_prop(
      src,
      fixture.node_rev_key,
      PropValue::I64(event.version as i64),
    )
    .expect("expected value");
  fixture
    .db
    .set_edge_prop(
      src,
      fixture.etype_rel,
      dst,
      fixture.edge_weight_key,
      PropValue::F64((event.version % 1024) as f64 / 1024.0),
    )
    .expect("expected value");
  fixture.db.commit().expect("expected value");
}

fn apply_graph_change_scip_tx(fixture: &DbFixture, event: &ChangeEvent) {
  let src = fixture.node_ids[event.chunk_idx];

  fixture.db.begin(false).expect("expected value");
  fixture
    .db
    .set_node_prop(
      src,
      fixture.node_scip_rev_key,
      PropValue::I64(event.version as i64),
    )
    .expect("expected value");
  fixture.db.commit().expect("expected value");
}

fn apply_graph_change_unified_tx(fixture: &DbFixture, event: &ChangeEvent) {
  let src = fixture.node_ids[event.chunk_idx];
  let dst = fixture.node_ids[(event.chunk_idx + 1) % fixture.node_ids.len()];

  fixture.db.begin(false).expect("expected value");
  fixture
    .db
    .set_node_prop(
      src,
      fixture.node_rev_key,
      PropValue::I64(event.version as i64),
    )
    .expect("expected value");
  fixture
    .db
    .set_node_prop(
      src,
      fixture.node_scip_rev_key,
      PropValue::I64(event.version as i64),
    )
    .expect("expected value");
  fixture
    .db
    .set_edge_prop(
      src,
      fixture.etype_rel,
      dst,
      fixture.edge_weight_key,
      PropValue::F64((event.version % 1024) as f64 / 1024.0),
    )
    .expect("expected value");
  fixture.db.commit().expect("expected value");
}

fn apply_vector_batch(
  fixture: &DbFixture,
  dims: usize,
  jobs: &[EmbedJob],
  freshness_samples: &mut Vec<u128>,
) {
  if jobs.is_empty() {
    return;
  }

  fixture.db.begin(false).expect("expected value");
  for job in jobs {
    let node_id = fixture.node_ids[job.chunk_idx];
    let value = (job.version % 1024) as f32 / 1024.0;
    let vector = vec![value; dims];
    fixture
      .db
      .set_node_vector(node_id, fixture.vector_key, &vector)
      .expect("expected value");
  }
  fixture.db.commit().expect("expected value");

  let now = Instant::now();
  for job in jobs {
    freshness_samples.push(now.duration_since(job.hot_done_at).as_nanos());
  }
}

fn run_sequential(config: &BenchConfig, events: &[ChangeEvent]) -> BenchResult {
  let fixture = setup_fixture(config, "sequential");
  let run_start = Instant::now();
  let mut hot_path_ns = Vec::with_capacity(events.len());
  let mut vector_freshness_ns = Vec::with_capacity(events.len());
  let ts_sleep = Duration::from_millis(config.tree_sitter_latency_ms);
  let scip_sleep = Duration::from_millis(config.scip_latency_ms);
  let embed_sleep = Duration::from_millis(config.embed_latency_ms);
  let mut last_hot_done = run_start;

  for event in events {
    let op_start = Instant::now();
    if config.tree_sitter_latency_ms > 0 {
      thread::sleep(ts_sleep);
    }
    apply_graph_change_ts_tx(&fixture, event);
    if config.scip_latency_ms > 0 {
      thread::sleep(scip_sleep);
    }
    apply_graph_change_scip_tx(&fixture, event);
    let hot_done = Instant::now();
    last_hot_done = hot_done;
    hot_path_ns.push(hot_done.duration_since(op_start).as_nanos());

    if config.embed_latency_ms > 0 {
      thread::sleep(embed_sleep);
    }
    let job = EmbedJob {
      chunk_idx: event.chunk_idx,
      version: event.version,
      hot_done_at: hot_done,
    };
    apply_vector_batch(
      &fixture,
      config.vector_dims,
      &[job],
      &mut vector_freshness_ns,
    );
  }

  let total_elapsed = run_start.elapsed();
  let hot_path_elapsed = last_hot_done.duration_since(run_start);

  if config.keep_db {
    println!("Sequential DB kept at: {}", fixture.db_path.display());
    let _ = fixture.temp_dir.keep();
  }

  if let Ok(db) = Arc::try_unwrap(fixture.db) {
    close_single_file(db).expect("expected value");
  } else {
    println!("Warning: failed to unwrap DB Arc; skipping explicit close");
  }

  BenchResult {
    mode: "sequential",
    changes: events.len(),
    applied_vectors: vector_freshness_ns.len(),
    total_elapsed,
    hot_path_elapsed,
    hot_path_ns,
    vector_freshness_ns,
    ..BenchResult::default()
  }
}

fn enqueue_job(
  queue: &Arc<(Mutex<EmbedQueueState>, Condvar)>,
  chunk_capacity: usize,
  job: EmbedJob,
) {
  let (lock, cv) = &**queue;
  let mut state = lock.lock().expect("expected value");

  if state.pending_by_chunk.capacity() == 0 {
    state.pending_by_chunk.reserve(chunk_capacity);
  }

  state.stats.enqueued_jobs += 1;
  let chunk_idx = job.chunk_idx;
  if state.pending_by_chunk.insert(chunk_idx, job).is_some() {
    state.stats.replaced_jobs += 1;
  } else {
    state.order.push_back(chunk_idx);
  }
  state.sample_depth();
  cv.notify_one();
}

fn take_embed_batch(
  queue: &Arc<(Mutex<EmbedQueueState>, Condvar)>,
  batch_size: usize,
  flush_window: Duration,
) -> Option<Vec<EmbedJob>> {
  let (lock, cv) = &**queue;
  let mut state = lock.lock().expect("expected value");

  loop {
    while state.order.is_empty() && !state.closed {
      state = cv.wait(state).expect("expected value");
    }

    if state.order.is_empty() && state.closed {
      return None;
    }

    if !flush_window.is_zero() && state.order.len() < batch_size && !state.closed {
      let (next_state, _) = cv
        .wait_timeout(state, flush_window)
        .expect("expected value");
      state = next_state;
      if state.order.is_empty() && state.closed {
        return None;
      }
    }

    let mut batch = Vec::with_capacity(batch_size);
    while batch.len() < batch_size {
      let Some(chunk_idx) = state.order.pop_front() else {
        break;
      };
      if let Some(job) = state.pending_by_chunk.remove(&chunk_idx) {
        batch.push(job);
        state.sample_depth();
      }
    }

    if !batch.is_empty() {
      return Some(batch);
    }

    if state.closed {
      return None;
    }
  }
}

fn run_parallel(config: &BenchConfig, events: &[ChangeEvent]) -> BenchResult {
  let fixture = setup_fixture(config, "parallel");
  let run_start = Instant::now();
  let mut hot_path_ns = Vec::with_capacity(events.len());
  let ts_sleep = Duration::from_millis(config.tree_sitter_latency_ms);
  let scip_sleep = Duration::from_millis(config.scip_latency_ms);
  let embed_sleep = Duration::from_millis(config.embed_latency_ms);
  let embed_flush = Duration::from_millis(config.embed_flush_ms);
  let mut last_hot_done = run_start;

  let queue = Arc::new((
    Mutex::new(EmbedQueueState::new(config.working_set)),
    Condvar::new(),
  ));
  let (result_tx, result_rx): (Sender<Vec<EmbedJob>>, Receiver<Vec<EmbedJob>>) = unbounded();

  let mut embed_handles = Vec::with_capacity(config.embed_inflight);
  for _ in 0..config.embed_inflight {
    let queue = Arc::clone(&queue);
    let tx = result_tx.clone();
    let batch_size = config.embed_batch_size;
    embed_handles.push(thread::spawn(move || {
      while let Some(batch) = take_embed_batch(&queue, batch_size, embed_flush) {
        if !embed_sleep.is_zero() {
          thread::sleep(embed_sleep);
        }
        if tx.send(batch).is_err() {
          return;
        }
      }
    }));
  }
  drop(result_tx);

  let writer_db = Arc::clone(&fixture.db);
  let writer_node_ids = fixture.node_ids.clone();
  let vector_key = fixture.vector_key;
  let dims = config.vector_dims;
  let apply_batch_size = config.vector_apply_batch_size;
  let writer_handle = thread::spawn(move || {
    let mut apply_buffer: Vec<EmbedJob> = Vec::with_capacity(apply_batch_size * 2);
    let mut freshness = Vec::new();
    let mut applied = 0usize;

    for mut batch in result_rx {
      apply_buffer.append(&mut batch);
      while apply_buffer.len() >= apply_batch_size {
        let chunk: Vec<EmbedJob> = apply_buffer.drain(..apply_batch_size).collect();
        writer_db.begin(false).expect("expected value");
        for job in &chunk {
          let node_id = writer_node_ids[job.chunk_idx];
          let value = (job.version % 1024) as f32 / 1024.0;
          let vector = vec![value; dims];
          writer_db
            .set_node_vector(node_id, vector_key, &vector)
            .expect("expected value");
        }
        writer_db.commit().expect("expected value");
        let now = Instant::now();
        for job in &chunk {
          freshness.push(now.duration_since(job.hot_done_at).as_nanos());
        }
        applied += chunk.len();
      }
    }

    if !apply_buffer.is_empty() {
      writer_db.begin(false).expect("expected value");
      for job in &apply_buffer {
        let node_id = writer_node_ids[job.chunk_idx];
        let value = (job.version % 1024) as f32 / 1024.0;
        let vector = vec![value; dims];
        writer_db
          .set_node_vector(node_id, vector_key, &vector)
          .expect("expected value");
      }
      writer_db.commit().expect("expected value");
      let now = Instant::now();
      for job in &apply_buffer {
        freshness.push(now.duration_since(job.hot_done_at).as_nanos());
      }
      applied += apply_buffer.len();
    }

    (freshness, applied)
  });

  for event in events {
    let op_start = Instant::now();
    if config.tree_sitter_latency_ms > 0 || config.scip_latency_ms > 0 {
      let parse_parallel_sleep = ts_sleep.max(scip_sleep);
      thread::sleep(parse_parallel_sleep);
    }
    apply_graph_change_unified_tx(&fixture, event);
    let hot_done = Instant::now();
    last_hot_done = hot_done;
    hot_path_ns.push(hot_done.duration_since(op_start).as_nanos());

    enqueue_job(
      &queue,
      config.working_set,
      EmbedJob {
        chunk_idx: event.chunk_idx,
        version: event.version,
        hot_done_at: hot_done,
      },
    );
  }

  {
    let (lock, cv) = &*queue;
    let mut state = lock.lock().expect("expected value");
    state.closed = true;
    cv.notify_all();
  }

  for handle in embed_handles {
    handle.join().expect("expected value");
  }

  let (vector_freshness_ns, applied_vectors) = writer_handle.join().expect("expected value");
  let total_elapsed = run_start.elapsed();
  let hot_path_elapsed = last_hot_done.duration_since(run_start);

  let (enqueued_jobs, replaced_jobs, queue_max_depth, queue_avg_depth) = {
    let (lock, _) = &*queue;
    let state = lock.lock().expect("expected value");
    let samples = state.stats.depth_samples.max(1);
    (
      state.stats.enqueued_jobs,
      state.stats.replaced_jobs,
      state.stats.max_depth,
      state.stats.depth_sum as f64 / samples as f64,
    )
  };

  if config.keep_db {
    println!("Parallel DB kept at: {}", fixture.db_path.display());
    let _ = fixture.temp_dir.keep();
  }

  if let Ok(db) = Arc::try_unwrap(fixture.db) {
    close_single_file(db).expect("expected value");
  } else {
    println!("Warning: failed to unwrap DB Arc; skipping explicit close");
  }

  BenchResult {
    mode: "parallel",
    changes: events.len(),
    applied_vectors,
    total_elapsed,
    hot_path_elapsed,
    hot_path_ns,
    vector_freshness_ns,
    enqueued_jobs,
    replaced_jobs,
    queue_max_depth,
    queue_avg_depth,
  }
}

fn print_result(result: &BenchResult) {
  let hot_p50 = percentile_ns(&result.hot_path_ns, 0.50);
  let hot_p95 = percentile_ns(&result.hot_path_ns, 0.95);
  let hot_p99 = percentile_ns(&result.hot_path_ns, 0.99);
  let fresh_p50 = percentile_ns(&result.vector_freshness_ns, 0.50);
  let fresh_p95 = percentile_ns(&result.vector_freshness_ns, 0.95);
  let fresh_p99 = percentile_ns(&result.vector_freshness_ns, 0.99);
  let hot_rate = format_rate(result.changes, result.hot_path_elapsed);
  let end_to_end_rate = format_rate(result.changes, result.total_elapsed);

  println!("\n--- {} ---", result.mode);
  println!("Changes: {}", result.changes);
  println!("Vectors applied: {}", result.applied_vectors);
  println!(
    "Hot path elapsed: {:.3}s",
    result.hot_path_elapsed.as_secs_f64()
  );
  println!("Total elapsed: {:.3}s", result.total_elapsed.as_secs_f64());
  println!("Hot path rate: {hot_rate}");
  println!("End-to-end rate: {end_to_end_rate}");
  println!(
    "Hot path latency: p50={} p95={} p99={}",
    format_latency_ns(hot_p50),
    format_latency_ns(hot_p95),
    format_latency_ns(hot_p99)
  );
  println!(
    "Vector freshness: p50={} p95={} p99={}",
    format_latency_ns(fresh_p50),
    format_latency_ns(fresh_p95),
    format_latency_ns(fresh_p99)
  );

  if result.mode == "parallel" {
    let replace_rate = if result.enqueued_jobs > 0 {
      (result.replaced_jobs as f64 / result.enqueued_jobs as f64) * 100.0
    } else {
      0.0
    };
    println!(
      "Queue: enqueued={} replaced={} ({replace_rate:.2}%) max_depth={} avg_depth={:.2}",
      result.enqueued_jobs, result.replaced_jobs, result.queue_max_depth, result.queue_avg_depth
    );
  }
}

fn print_comparison(seq: &BenchResult, par: &BenchResult) {
  let seq_hot_p95 = percentile_ns(&seq.hot_path_ns, 0.95);
  let par_hot_p95 = percentile_ns(&par.hot_path_ns, 0.95);
  let seq_fresh_p95 = percentile_ns(&seq.vector_freshness_ns, 0.95);
  let par_fresh_p95 = percentile_ns(&par.vector_freshness_ns, 0.95);

  let hot_gain = if par.hot_path_elapsed.as_nanos() > 0 {
    seq.hot_path_elapsed.as_secs_f64() / par.hot_path_elapsed.as_secs_f64()
  } else {
    0.0
  };
  let end_to_end_gain = if par.total_elapsed.as_nanos() > 0 {
    seq.total_elapsed.as_secs_f64() / par.total_elapsed.as_secs_f64()
  } else {
    0.0
  };

  println!("\n=== Comparison (sequential vs parallel) ===");
  println!("Hot path elapsed speedup: {hot_gain:.2}x");
  println!("End-to-end elapsed speedup: {end_to_end_gain:.2}x");
  println!(
    "Hot p95: {} -> {}",
    format_latency_ns(seq_hot_p95),
    format_latency_ns(par_hot_p95)
  );
  println!(
    "Freshness p95: {} -> {}",
    format_latency_ns(seq_fresh_p95),
    format_latency_ns(par_fresh_p95)
  );
}

fn main() {
  let config = parse_args();
  let events = generate_events(&config);

  println!("==================================================================");
  println!("Index Pipeline Hypothesis Benchmark");
  println!("==================================================================");
  println!("Mode: {:?}", config.mode);
  println!("Changes: {}", config.changes);
  println!("Working set: {}", config.working_set);
  println!("Vector dims: {}", config.vector_dims);
  println!(
    "Parse latency: tree-sitter={}ms scip={}ms",
    config.tree_sitter_latency_ms, config.scip_latency_ms
  );
  println!("Embed latency: {}ms per batch", config.embed_latency_ms);
  println!(
    "Embed batching: size={} flush={}ms inflight={}",
    config.embed_batch_size, config.embed_flush_ms, config.embed_inflight
  );
  println!(
    "Vector apply batch size: {}",
    config.vector_apply_batch_size
  );
  println!("WAL size: {} bytes", config.wal_size);
  println!("Sync mode: {:?}", config.sync_mode);
  println!("Group commit: every commit");
  println!("Auto-checkpoint: {}", config.auto_checkpoint);
  println!("MVCC: {}", mvcc_label(config.mvcc));
  println!("Seed: {}", config.seed);
  println!("==================================================================");

  let mut seq_result: Option<BenchResult> = None;
  let mut par_result: Option<BenchResult> = None;

  match config.mode {
    Mode::Sequential => {
      let result = run_sequential(&config, &events);
      print_result(&result);
      seq_result = Some(result);
    }
    Mode::Parallel => {
      let result = run_parallel(&config, &events);
      print_result(&result);
      par_result = Some(result);
    }
    Mode::Both => {
      let seq = run_sequential(&config, &events);
      print_result(&seq);
      let par = run_parallel(&config, &events);
      print_result(&par);
      seq_result = Some(seq);
      par_result = Some(par);
    }
  }

  if let (Some(seq), Some(par)) = (seq_result.as_ref(), par_result.as_ref()) {
    print_comparison(seq, par);
  }
}
