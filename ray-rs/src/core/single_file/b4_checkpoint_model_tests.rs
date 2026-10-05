//! A seeded randomized model test of WAL segments and checkpoints, the kind
//! that would have caught review finding R7 (a transaction committed across
//! a cut, lost after a reopen). Included from checkpoint.rs for its hooks.
//!
//! Each seed opens a database with small, random log options (WAL size,
//! segment extents, the segment limit, the checkpoint budget, sync mode,
//! MVCC, automatic and background checkpoints, the checkpoint thread) and
//! runs random steps: small, large and WAL-sized transactions (creates,
//! updates and deletes of keyed nodes), rollbacks, a long-open transaction
//! on a helper thread that spans spills and cuts, background and blocking
//! checkpoints, application background checkpoints on a thread of their own
//! running beside the steps, forced spills, injected checkpoint failures
//! (then cleared), close or drop (which keeps WAL segments a close would
//! checkpoint away) and reopen (sometimes with a checkpoint thread's run in
//! flight), read-only reopens, copies of the file taken between steps, and
//! crash images of one step's writes and syncs (process crashes, and in
//! `Full` mode OS crashes that lose, reorder or tear unsynced writes, and
//! tear a write at a 512-byte sector boundary, header writes that change
//! the segment table at every one). A
//! copy or image is sometimes opened a second time after the first open's
//! recovery (closed or dropped), and sometimes the steps go on with it as
//! the database, as a process that crashed and reopened its file would.
//! One seed in three puts the segment table under pressure: a few entries,
//! or a dozen and more (a test hook), the smallest extents, a trigger far
//! beyond the table, and steps that fill it past a long transaction pinning
//! a late segment, then checkpoint.
//!
//! An oracle holds the acknowledged commits. After every step the live
//! database, and after every reopen, copy and crash image the reopened
//! file, must match it: nothing lost, nothing extra, no partial transaction.
//! Only a step in flight at a crash may be either wholly there or wholly
//! absent. A write may fail only where no checkpoint can make room, and a
//! background checkpoint that returns `Ok` must have covered every segment
//! there was when it started. And the database is never wedged: with no
//! transaction open and no fault armed, a background checkpoint covers the
//! log and a write commits (`Model::liveness_step`, at random, after
//! filling the table, and at the end).
//!
//! The quick run takes 100 seeds; `REGRESSION_SEEDS` pins seeds that catch
//! the bugs reviews found (see `scripts/model-regression-seeds.py`).
//!
//! Environment: `KITE_MODEL_SEEDS` seeds (default 100) from
//! `KITE_MODEL_FIRST_SEED` (default 0), `KITE_MODEL_STEPS` steps each
//! (default 60), on `KITE_MODEL_THREADS` threads (default: the CPUs, at
//! most four); `KITE_MODEL_SEED` runs that seed (or those,
//! comma-separated); `KITE_MODEL_VERBOSE` prints each seed's progress,
//! `KITE_MODEL_TRACE` each step as it runs, and a seed running past
//! `KITE_MODEL_SEED_TIMEOUT` seconds (default 120) fails as hung. A failure names its seed and the steps that led to it. Seeds with
//! the checkpoint thread or concurrent application checkpoints depend on
//! timing too, so they may not replay exactly; the rest do. The run prints
//! what the seeds covered (commits, cuts, crash images, ...).

use super::b4_checkpoint_segments_tests::{crash_image, sector_tears, CrashModel, SECTOR};
use super::*;
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

/// Key -> the node's `data` property: what the acknowledged commits made.
type State = BTreeMap<String, String>;

/// The checkpoint steps a fault may fail (every one a checkpoint or a spill
/// reaches).
const FAULT_PHASES: [CheckpointPhase; 14] = [
  CheckpointPhase::GateAcquired,
  CheckpointPhase::CutReleased,
  CheckpointPhase::SnapshotPageWritten,
  CheckpointPhase::SnapshotWritten,
  CheckpointPhase::SnapshotDurable,
  CheckpointPhase::HeaderWritten,
  CheckpointPhase::HeaderDurable,
  CheckpointPhase::SnapshotReload,
  CheckpointPhase::PostCutReplay,
  CheckpointPhase::SpillSegmentWritten,
  CheckpointPhase::SpillHeaderDurable,
  CheckpointPhase::SegmentsReleased,
  CheckpointPhase::TailTruncate,
  CheckpointPhase::SpillDecided,
];

fn env_number(name: &str, default: u64) -> u64 {
  match std::env::var(name) {
    Ok(value) => value
      .parse()
      .unwrap_or_else(|_| panic!("{name}={value:?} is not a number")),
    Err(_) => default,
  }
}

#[derive(Clone, Debug)]
enum Write {
  Create(String, String),
  Update(String, String),
  Delete(String),
}

/// About the bytes of records `writes` make.
fn write_bytes(writes: &[Write]) -> u64 {
  writes
    .iter()
    .map(|write| match write {
      Write::Create(key, value) | Write::Update(key, value) => key.len() + value.len() + 64,
      Write::Delete(key) => key.len() + 64,
    } as u64)
    .sum()
}

fn apply_writes(state: &mut State, writes: &[Write]) {
  for write in writes {
    match write {
      Write::Create(key, value) | Write::Update(key, value) => {
        state.insert(key.clone(), value.clone());
      }
      Write::Delete(key) => {
        state.remove(key);
      }
    }
  }
}

/// Make `writes` in the transaction open on this thread.
fn run_writes(db: &SingleFileDB, writes: &[Write]) -> Result<()> {
  let data = db
    .propkey_id("data")
    .ok_or_else(|| KiteError::Internal("model: no data property key".to_string()))?;
  for write in writes {
    match write {
      Write::Create(key, value) => {
        let node = db.create_node(Some(key))?;
        db.set_node_prop(node, data, PropValue::String(value.clone()))?;
      }
      Write::Update(key, value) => {
        let node = db
          .node_by_key(key)
          .ok_or_else(|| KiteError::Internal(format!("model: no node {key} to update")))?;
        db.set_node_prop(node, data, PropValue::String(value.clone()))?;
      }
      Write::Delete(key) => {
        let node = db
          .node_by_key(key)
          .ok_or_else(|| KiteError::Internal(format!("model: no node {key} to delete")))?;
        db.delete_node(node)?;
      }
    }
  }
  Ok(())
}

/// Commit `writes` in a transaction of their own (rolled back if a write
/// fails).
fn commit_writes(db: &SingleFileDB, writes: &[Write]) -> Result<()> {
  db.begin(false)?;
  if let Err(error) = run_writes(db, writes) {
    db.rollback()?;
    return Err(error);
  }
  db.commit()
}

/// What `db` holds, checking that each node's key finds it.
fn read_state(db: &SingleFileDB) -> std::result::Result<State, String> {
  let data = db.propkey_id("data");
  let mut state = State::new();
  let nodes = db.list_nodes();
  for &node in &nodes {
    let key = db
      .node_key(node)
      .ok_or_else(|| format!("node {node} has no key"))?;
    let value = match data.and_then(|data| db.node_prop(node, data)) {
      Some(PropValue::String(value)) => value,
      other => format!("<node without its data: {other:?}>"),
    };
    if db.node_by_key(&key) != Some(node) {
      return Err(format!(
        "key {key} finds {:?}, not its node {node}",
        db.node_by_key(&key)
      ));
    }
    if state.insert(key.clone(), value).is_some() {
      return Err(format!("two nodes have key {key}"));
    }
  }
  if db.count_nodes() != nodes.len() {
    return Err(format!(
      "count_nodes() is {}, but {} nodes are listed",
      db.count_nodes(),
      nodes.len()
    ));
  }
  Ok(state)
}

fn describe_difference(expected: &State, actual: &State) -> String {
  let missing: Vec<_> = expected
    .keys()
    .filter(|key| !actual.contains_key(*key))
    .take(4)
    .collect();
  let extra: Vec<_> = actual
    .keys()
    .filter(|key| !expected.contains_key(*key))
    .take(4)
    .collect();
  let changed: Vec<_> = expected
    .iter()
    .filter(|(key, value)| actual.get(*key).is_some_and(|actual| actual != *value))
    .map(|(key, value)| {
      format!(
        "{key}: {} bytes expected, {} found",
        value.len(),
        actual[key].len()
      )
    })
    .take(4)
    .collect();
  format!(
    "{} nodes expected, {} found; missing {missing:?}, extra {extra:?}, changed {changed:?}",
    expected.len(),
    actual.len()
  )
}

/// `db` holds one of `allowed`: which.
fn check_state(
  what: &str,
  db: &SingleFileDB,
  allowed: &[&State],
) -> std::result::Result<usize, String> {
  let actual = read_state(db).map_err(|error| format!("{what}: {error}"))?;
  if let Some(index) = allowed.iter().position(|state| **state == actual) {
    return Ok(index);
  }
  let differences: Vec<String> = allowed
    .iter()
    .map(|state| describe_difference(state, &actual))
    .collect();
  Err(format!("{what}: matches no allowed state: {differences:?}"))
}

/// Open the file at `path` (read-only or not), check it holds one of
/// `allowed`, and close it, or drop it if `drop_after` says so for that
/// round (dropping keeps WAL segments a close would checkpoint away); with
/// `twice`, open and check it again (after a writable open's recovery).
/// Returns which of `allowed` it holds.
fn reopen_and_check(
  what: &str,
  path: &Path,
  options: &SingleFileOpenOptions,
  read_only: bool,
  twice: bool,
  allowed: &[&State],
  mut drop_after: impl FnMut() -> bool,
) -> std::result::Result<usize, String> {
  let mut matched = 0;
  for round in 0..if twice { 2 } else { 1 } {
    let what = format!(
      "{what} (open {}{})",
      round + 1,
      if read_only { ", read-only" } else { "" }
    );
    let db = open_single_file(path, options.clone().read_only(read_only))
      .map_err(|error| format!("{what}: the open failed: {error}"))?;
    let checked = check_state(&what, &db, allowed);
    if drop_after() {
      drop(db);
    } else {
      close_single_file(db).map_err(|error| format!("{what}: the close failed: {error}"))?;
    }
    matched = checked?;
  }
  Ok(matched)
}

#[derive(Clone, Debug)]
struct Config {
  wal_size: usize,
  segment_size: u64,
  segment_limit: Option<u64>,
  log_ratio: f64,
  log_budget: u64,
  sync: SyncMode,
  mvcc: bool,
  auto_checkpoint: bool,
  background: bool,
  thread: bool,
  /// Entries of the WAL segment table the database uses, if fewer than all
  /// (`set_wal_segment_test_capacity`): with the smallest extents and no
  /// byte limit to speak of, a few spills fill it, so full tables, pinned
  /// segments and failed cuts meet often.
  table: Option<usize>,
}

impl Config {
  fn random(rng: &mut StdRng) -> Self {
    let sync = match rng.gen_range(0..10) {
      0 => SyncMode::Off,
      1..=4 => SyncMode::Normal,
      _ => SyncMode::Full,
    };
    Self {
      wal_size: if rng.gen_bool(0.8) {
        64 * 1024
      } else {
        128 * 1024
      },
      segment_size: [1, 16 * 1024, 64 * 1024, 256 * 1024][rng.gen_range(0..4)],
      segment_limit: match rng.gen_range(0..4) {
        0 => Some(64 * 1024),
        1 => Some(512 * 1024),
        _ => None,
      },
      log_ratio: 0.5,
      log_budget: [16 * 1024, 64 * 1024, 256 * 1024, 1024 * 1024][rng.gen_range(0..4)],
      sync,
      mvcc: rng.gen_bool(0.85),
      auto_checkpoint: rng.gen_bool(0.8),
      background: rng.gen_bool(0.8),
      thread: rng.gen_bool(0.35),
      table: None,
    }
    .with_table_pressure(rng)
  }

  /// One seed in three: a small segment table (half of them a few entries,
  /// half eleven and more, past the header page's first sector, where a
  /// header write torn at a sector boundary splits the table from the
  /// fixed fields), the smallest extents, a byte limit far beyond them, and
  /// a checkpoint trigger far beyond the table (a hundred times the
  /// snapshot): writers fill the table and wait at it, and checkpoints meet
  /// full tables, with open transactions pinning segments.
  fn with_table_pressure(mut self, rng: &mut StdRng) -> Self {
    if rng.gen_bool(1.0 / 3.0) {
      self.table = Some(if rng.gen_bool(0.5) {
        rng.gen_range(3..=6)
      } else {
        rng.gen_range(11..=16)
      });
      self.segment_size = 1;
      self.segment_limit = Some(256 * 1024 * 1024);
      self.log_ratio = 100.0;
      self.log_budget = 1024 * 1024 * 1024;
      // Long transactions pin segments (see `Model::fill_table_step`).
      self.mvcc = true;
      self.background = true;
    }
    self
  }

  fn options(&self) -> SingleFileOpenOptions {
    let mut options = SingleFileOpenOptions::new()
      .wal_size(self.wal_size)
      .wal_segment_size(self.segment_size)
      .checkpoint_log_ratio(self.log_ratio)
      .checkpoint_log_budget(self.log_budget)
      .sync_mode(self.sync)
      .mvcc(self.mvcc)
      .auto_checkpoint(self.auto_checkpoint)
      .background_checkpoint(self.background)
      .checkpoint_thread(self.thread);
    if let Some(limit) = self.segment_limit {
      options = options.wal_segment_limit(limit);
    }
    options
  }

  /// A copy of the file between steps holds every acknowledged commit:
  /// not in `SyncMode::Off`, where commits stay in memory.
  fn copies_hold_commits(&self) -> bool {
    self.sync != SyncMode::Off
  }

  /// Every pager write and sync of a step happens on the stepping thread
  /// (no checkpoint thread), and acknowledged commits are on disk.
  fn crash_images_apply(&self) -> bool {
    !self.thread && self.copies_hold_commits()
  }
}

/// How often the seeds reached what the model is for, summed over a run:
/// a run that reaches none of it proves nothing.
type Coverage = BTreeMap<&'static str, u64>;

/// A request to the long-open transaction's thread.
enum LongRequest {
  Begin,
  Write(Vec<Write>),
  Commit,
  Rollback,
}

/// A thread that holds a write transaction open across the steps (a
/// transaction is its thread's).
struct LongThread {
  requests: mpsc::Sender<(Arc<SingleFileDB>, LongRequest, bool)>,
  replies: mpsc::Receiver<(Result<()>, Vec<IoEvent>)>,
}

impl LongThread {
  fn spawn() -> Self {
    let (requests, incoming) = mpsc::channel::<(Arc<SingleFileDB>, LongRequest, bool)>();
    let (outgoing, replies) = mpsc::channel();
    std::thread::Builder::new()
      .name("model-long-transaction".to_string())
      .spawn(move || {
        for (db, request, record) in incoming {
          let run = || match request {
            LongRequest::Begin => db.begin(false).map(|_| ()),
            LongRequest::Write(writes) => run_writes(&db, &writes).inspect_err(|_| {
              let _ = db.rollback();
            }),
            LongRequest::Commit => db.commit(),
            LongRequest::Rollback => db.rollback(),
          };
          let reply = if record {
            io_hooks::record_io_during(run)
          } else {
            (run(), Vec::new())
          };
          drop(db);
          if outgoing.send(reply).is_err() {
            break;
          }
        }
      })
      .expect("spawn the long transaction's thread");
    Self { requests, replies }
  }

  fn call(
    &self,
    db: &Arc<SingleFileDB>,
    request: LongRequest,
    record: bool,
  ) -> (Result<()>, Vec<IoEvent>) {
    self
      .requests
      .send((Arc::clone(db), request, record))
      .expect("the long transaction's thread ended");
    self
      .replies
      .recv()
      .expect("the long transaction's thread ended (it panicked)")
  }
}

/// Application background checkpoints on a thread of their own, beside the
/// steps, until stopped (or dropped).
struct Checkpointer {
  stop: Arc<AtomicBool>,
  handle: Option<std::thread::JoinHandle<Vec<String>>>,
}

impl Drop for Checkpointer {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Release);
  }
}

impl Checkpointer {
  fn start(db: &Arc<SingleFileDB>) -> Self {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
      let (db, stop) = (Arc::clone(db), Arc::clone(&stop));
      std::thread::Builder::new()
        .name("model-checkpointer".to_string())
        .spawn(move || {
          let mut unexpected = Vec::new();
          while !stop.load(Ordering::Acquire) {
            match db.background_checkpoint() {
              Ok(()) | Err(KiteError::CheckpointDeclined(_)) => {}
              // Only an armed fault injects.
              Err(error) if error.to_string().contains("injected") => {}
              Err(error) => unexpected.push(error.to_string()),
            }
            std::thread::sleep(Duration::from_micros(200));
          }
          unexpected
        })
        .expect("spawn the checkpointer")
    };
    Self {
      stop,
      handle: Some(handle),
    }
  }

  /// Stop it, and return the errors it should not have had.
  fn stop(mut self) -> Vec<String> {
    self.stop.store(true, Ordering::Release);
    let handle = self.handle.take().expect("running");
    handle.join().expect("the checkpointer panicked")
  }
}

/// A step the model leans to next, half the time, after one that sets up
/// what it would check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hint {
  /// A reopen after a drop, or a copy gone on with.
  Reopen,
  /// A background checkpoint.
  Checkpoint,
}

/// One seed's run.
struct Model {
  seed: u64,
  rng: StdRng,
  config: Config,
  _dir: tempfile::TempDir,
  dir: PathBuf,
  path: PathBuf,
  db: Option<Arc<SingleFileDB>>,
  /// The acknowledged commits.
  state: State,
  /// The open long transaction's writes, if one is open.
  long: Option<Vec<Write>>,
  long_thread: LongThread,
  checkpointer: Option<Checkpointer>,
  /// Checkpoint faults were armed in this session (since the open): they
  /// may fail checkpoints, spills and the writes that need them.
  faults_seen: bool,
  /// Faults armed now.
  faults_armed: bool,
  next_key: u64,
  next_file: u64,
  log: Vec<String>,
  coverage: Coverage,
  /// Cuts made when the long transaction began.
  long_cuts: usize,
  /// Writes refused for pinned segments before this step.
  pinned_refusals_before: u64,
  /// What the next step leans to.
  hint: Option<Hint>,
  /// Bytes of records the step writes (about: its values).
  step_bytes: u64,
}

type Outcome = std::result::Result<(), String>;

impl Model {
  fn new(seed: u64) -> Self {
    let mut rng = StdRng::seed_from_u64(seed);
    let config = Config::random(&mut rng);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("model.kitedb");
    let db = open_model_db(&path, &config).expect("open");
    db.begin(false).expect("begin");
    db.define_propkey("data").expect("define data");
    db.commit().expect("commit the property key");
    Self {
      seed,
      rng,
      config,
      dir: dir.path().to_path_buf(),
      _dir: dir,
      path,
      db: Some(Arc::new(db)),
      state: State::new(),
      long: None,
      long_thread: LongThread::spawn(),
      checkpointer: None,
      faults_seen: false,
      faults_armed: false,
      next_key: 0,
      next_file: 0,
      log: Vec::new(),
      coverage: Coverage::new(),
      long_cuts: 0,
      pinned_refusals_before: 0,
      step_bytes: 0,
      hint: None,
    }
  }

  fn count(&mut self, what: &'static str) {
    *self.coverage.entry(what).or_default() += 1;
  }

  /// Count the WAL segments the database has now.
  fn count_segments(&mut self) {
    if let Some(db) = &self.db {
      let stats = wal_segment_test_stats(db);
      let most = self.coverage.entry("most live segments").or_default();
      *most = (*most).max(stats.live as u64);
      if stats.live + 1 >= db.wal_segment_capacity() {
        *self
          .coverage
          .entry("steps with the segment table full")
          .or_default() += 1;
      }
      if stats.live > 0 {
        *self.coverage.entry("steps with live segments").or_default() += 1;
      }
    }
  }

  /// A long transaction committed: count it, and whether a cut came
  /// between its begin and its commit.
  fn count_long_commit(&mut self, db: &SingleFileDB, writes: &[Write]) {
    self.count("long transactions committed");
    if checkpoint_test_cuts(db) > self.long_cuts && !writes.is_empty() {
      self.count("long transactions committed across a cut");
      // Its records begin in a segment a checkpoint kept for it: what a
      // reopen (by a drop, or of a copy) and a checkpoint after it must keep
      // track of.
      self.hint = Some(Hint::Reopen);
    }
  }

  /// Log a step (printed as it happens with `KITE_MODEL_TRACE`).
  fn note(&mut self, entry: String) {
    if std::env::var("KITE_MODEL_TRACE").is_ok() {
      eprintln!("model seed {}: {entry}", self.seed);
    }
    self.log.push(entry);
  }

  fn db(&self) -> &Arc<SingleFileDB> {
    self.db.as_ref().expect("the database is open")
  }

  fn value(&mut self, max_len: usize) -> String {
    let len = self.rng.gen_range(0..=max_len);
    let byte = b'a' + self.rng.gen_range(0..26u8);
    let mut value = format!("{}:", self.next_key);
    value.push_str(&String::from_utf8(vec![byte; len]).expect("ascii"));
    value
  }

  fn new_key(&mut self, prefix: &str) -> String {
    self.next_key += 1;
    format!("{prefix}{}", self.next_key)
  }

  /// `count` writes: creates of new keys with values up to `max_len` bytes,
  /// and (with `touch_existing`) updates and deletes of distinct committed
  /// keys.
  fn writes(&mut self, count: usize, max_len: usize, touch_existing: bool) -> Vec<Write> {
    let mut touched = std::collections::BTreeSet::new();
    let mut writes = Vec::with_capacity(count);
    for _ in 0..count {
      let existing = touch_existing && !self.state.is_empty() && self.rng.gen_bool(0.4);
      if existing {
        let index = self.rng.gen_range(0..self.state.len());
        let key = self.state.keys().nth(index).expect("in range").clone();
        if !touched.insert(key.clone()) {
          continue;
        }
        if self.rng.gen_bool(0.6) {
          let value = self.value(max_len);
          writes.push(Write::Update(key, value));
        } else {
          writes.push(Write::Delete(key));
        }
      } else {
        let key = self.new_key("k");
        let value = self.value(max_len);
        writes.push(Write::Create(key, value));
      }
    }
    writes
  }

  /// Whether a failed commit's error is one the conditions allow.
  fn allowed_commit_error(&self, error: &KiteError) -> bool {
    match error {
      // A writer fails at the segment limit only when no checkpoint can make
      // room: without automatic checkpoints, with blocking ones (the writer
      // fails, the checkpoint after it runs), with segments full of open
      // transactions' records (counted), while checkpoints fail, or for
      // records the limit barely holds.
      KiteError::WalBufferFull => {
        !self.config.auto_checkpoint
          || !self.config.background
          || self.faults_seen
          || checkpoint_test_pinned_refusals(self.db()) > self.pinned_refusals_before
          || self.step_bytes.saturating_mul(2)
            >= self.db().wal_segment_limit(&self.db().header.read())
      }
      KiteError::CheckpointFailed(_) => self.faults_seen,
      other => self.faults_seen && other.to_string().contains("injected"),
    }
  }

  /// The live database matches the oracle.
  fn check_live(&self, what: &str) -> Outcome {
    check_state(what, self.db(), &[&self.state]).map(|_| ())
  }

  /// Let the checkpoint thread answer every request, so no write is in
  /// flight while the file is copied.
  fn quiesce(&mut self) -> Outcome {
    if let Some(checkpointer) = self.checkpointer.take() {
      let unexpected = checkpointer.stop();
      if !unexpected.is_empty() {
        return Err(format!(
          "application background checkpoints failed: {unexpected:?}"
        ));
      }
    }
    self.db().wait_for_checkpoint_thread();
    Ok(())
  }

  fn clear_faults(&mut self) {
    if let Some(db) = &self.db {
      clear_checkpoint_test_db_faults(db);
    }
    self.faults_armed = false;
  }

  fn copy_path(&mut self) -> PathBuf {
    self.next_file += 1;
    self.dir.join(format!("copy-{}.kitedb", self.next_file))
  }

  /// Run one step, chosen at random.
  fn step(&mut self) -> Outcome {
    self.pinned_refusals_before = checkpoint_test_pinned_refusals(self.db());
    self.step_bytes = 0;
    let crash_images = self.config.crash_images_apply() && self.rng.gen_bool(0.06);
    // A long transaction needs MVCC (another write transaction commits
    // beside it), and no blocking automatic checkpoint: one waits for every
    // open transaction, so a commit beside it would wait for good.
    let long_transactions =
      self.config.mvcc && (self.config.background || !self.config.auto_checkpoint);
    let mut choice = self.rng.gen_range(0..100);
    match self.hint.take() {
      Some(Hint::Reopen) if self.rng.gen_bool(0.5) => {
        return if self.config.copies_hold_commits() && self.rng.gen_bool(0.5) {
          self.copy_step()
        } else {
          self.reopen_step_ending(false, crash_images, Some(true))
        };
      }
      // A background checkpoint.
      Some(Hint::Checkpoint) if self.rng.gen_bool(0.5) => choice = 59,
      // Under table pressure, full tables for checkpoints to meet.
      _ if self.config.table.is_some() && self.rng.gen_bool(0.1) => {
        return self.fill_table_step(crash_images, long_transactions);
      }
      _ => {}
    }
    match choice {
      0..=27 => {
        let count = self.rng.gen_range(1..=4);
        let writes = self.writes(count, 300, true);
        self.commit_step("small commit", writes, crash_images)
      }
      28..=33 => {
        let count = self.rng.gen_range(10..=120);
        let writes = self.writes(count, 2_000, true);
        self.commit_step("large commit", writes, crash_images)
      }
      34..=35 => {
        let count = self.rng.gen_range(1..=2);
        let mut writes = Vec::new();
        for _ in 0..count {
          let key = self.new_key("huge");
          let value = self.value(160 * 1024);
          writes.push(Write::Create(key, value));
        }
        self.commit_step("WAL-sized commit", writes, crash_images)
      }
      36..=40 => {
        let count = self.rng.gen_range(1..=60);
        let writes = self.writes(count, 2_000, true);
        self.step_bytes = write_bytes(&writes);
        self.note(format!(
          "rolled back transaction of {} writes",
          writes.len()
        ));
        let db = Arc::clone(self.db());
        db.begin(false).map_err(|error| format!("begin: {error}"))?;
        match run_writes(&db, &writes) {
          Ok(()) => {}
          Err(error) if self.allowed_commit_error(&error) => {}
          Err(error) => return Err(format!("a write before the rollback failed: {error}")),
        }
        db.rollback()
          .map_err(|error| format!("the rollback failed: {error}"))?;
        self.check_live("after a rollback")
      }
      41..=58 if long_transactions => {
        let outcome = self.long_step(crash_images);
        self.count_segments();
        outcome
      }
      59..=63 => self.background_checkpoint_step(crash_images),
      64..=66 if self.long.is_none() => {
        self.note("blocking checkpoint".to_string());
        let db = Arc::clone(self.db());
        let (result, events, base) = self.recorded(crash_images, || db.checkpoint());
        match result {
          Ok(()) => {
            if self.checkpointer.is_none() && !self.faults_seen {
              if let Some(error) = db.checkpoint_error() {
                return Err(format!(
                  "a checkpoint succeeded, but the error stays: {error}"
                ));
              }
            }
          }
          Err(error) if self.faults_seen => self.note(format!("  failed: {error}")),
          Err(error) => return Err(format!("a blocking checkpoint failed: {error}")),
        }
        drop(db);
        self.check_images("blocking checkpoint", base, &events, None)?;
        self.check_live("after a blocking checkpoint")
      }
      67..=70 => {
        self.note("forced spill".to_string());
        let db = Arc::clone(self.db());
        let (result, events, base) = self.recorded(crash_images, || force_spill(&db));
        match result {
          Ok(_) => {}
          Err(error) if self.faults_seen => self.note(format!("  failed: {error}")),
          Err(error) => return Err(format!("a forced spill failed: {error}")),
        }
        drop(db);
        self.check_images("forced spill", base, &events, None)?;
        self.check_live("after a forced spill")
      }
      71..=73 => self.reopen_step(false, crash_images),
      74..=75 => self.reopen_step(true, false),
      76..=79 if self.config.copies_hold_commits() => self.copy_step(),
      80..=82 => {
        if self.faults_armed {
          self.note("clear checkpoint faults".to_string());
          self.clear_faults();
        } else {
          let phase = FAULT_PHASES[self.rng.gen_range(0..FAULT_PHASES.len())];
          let sticky = self.rng.gen_bool(0.5);
          self.note(format!(
            "arm a checkpoint fault at {phase:?} (sticky: {sticky})"
          ));
          set_checkpoint_test_db_fault(self.db(), phase, sticky);
          self.count("faults armed");
          self.faults_armed = true;
          self.faults_seen = true;
        }
        Ok(())
      }
      83..=85 => {
        match self.checkpointer.take() {
          Some(checkpointer) => {
            self.note("stop the application checkpointer".to_string());
            let unexpected = checkpointer.stop();
            if !unexpected.is_empty() {
              return Err(format!(
                "application background checkpoints failed: {unexpected:?}"
              ));
            }
          }
          None => {
            self.note("start the application checkpointer".to_string());
            self.checkpointer = Some(Checkpointer::start(self.db()));
            self.count("application checkpointers");
          }
        }
        Ok(())
      }
      86..=87 => self.liveness_step(),
      _ => {
        let count = self.rng.gen_range(1..=8);
        let writes = self.writes(count, 300, true);
        self.commit_step("small commit", writes, crash_images)
      }
    }
  }

  /// Liveness: with no transaction open and no fault armed, the database is
  /// not wedged. After a commit (refused only as the other steps allow: it
  /// leaves the WAL some records, so the checkpoint's cut has them to place
  /// as well), a background checkpoint covers the log (it neither declines
  /// nor fails), and a write then commits. The other checks allow a write
  /// refused where no checkpoint could make room, and a checkpoint declined
  /// while a transaction is open: a database that refused writes for good
  /// would pass them.
  fn liveness_step(&mut self) -> Outcome {
    self.finish_long()?;
    if self.faults_armed {
      self.note("clear checkpoint faults".to_string());
      self.clear_faults();
    }
    let count = self.rng.gen_range(1..=4);
    let writes = self.writes(count, 300, true);
    self.commit_step("small commit", writes, false)?;
    self.note("liveness: a background checkpoint, then a commit".to_string());
    self.count("liveness checks");
    let db = Arc::clone(self.db());
    let newest_before = db
      .header
      .read()
      .wal_segments
      .entries
      .last()
      .map(|segment| segment.seq);
    if let Err(error) = db.background_checkpoint() {
      return Err(format!(
        "liveness: with no transaction open and no fault armed, a background checkpoint \
         failed: {error}"
      ));
    }
    let covered = db.header.read().wal_segments.covered;
    if newest_before.is_some_and(|newest| covered < newest) {
      return Err(format!(
        "liveness: a background checkpoint returned Ok, but covered only up to segment \
         {covered} of {newest_before:?}"
      ));
    }
    let count = self.rng.gen_range(1..=4);
    let writes = self.writes(count, 300, true);
    if let Err(error) = commit_writes(&db, &writes) {
      return Err(format!(
        "liveness: after a background checkpoint, with no transaction open and no fault \
         armed, a small commit failed: {error}"
      ));
    }
    drop(db);
    apply_writes(&mut self.state, &writes);
    self.count("commits");
    self.check_live("after the liveness check")
  }

  /// A background checkpoint on this thread (its writes recorded for crash
  /// images if `crash_images`).
  fn background_checkpoint_step(&mut self, crash_images: bool) -> Outcome {
    self.note("background checkpoint".to_string());
    let db = Arc::clone(self.db());
    let newest_before = db
      .header
      .read()
      .wal_segments
      .entries
      .last()
      .map(|segment| segment.seq);
    let (result, events, base) = self.recorded(crash_images, || db.background_checkpoint());
    match result {
      // A background checkpoint that returns Ok covered the log (at
      // least every segment there was when it was called), or found it
      // covered: none returns Ok having done nothing.
      Ok(()) => {
        let covered = db.header.read().wal_segments.covered;
        if newest_before.is_some_and(|newest| covered < newest) {
          return Err(format!(
            "a background checkpoint returned Ok, but covered only up to segment {covered} \
             of {newest_before:?}"
          ));
        }
      }
      // Only open transactions holding every segment make it decline.
      Err(KiteError::CheckpointDeclined(_)) if self.long.is_some() => {}
      Err(error) if self.faults_seen => self.note(format!("  failed: {error}")),
      Err(error) => return Err(format!("a background checkpoint failed: {error}")),
    }
    drop(db);
    self.check_images("background checkpoint", base, &events, None)?;
    self.check_live("after a background checkpoint")
  }

  /// Under table pressure: fill the segment table (large commits until it
  /// is full, or a write is refused), mostly with a long transaction begun
  /// part-way and spilled into it (so a late segment is pinned, and a
  /// checkpoint has earlier ones to cover: one open from before ends first),
  /// then a background checkpoint: it meets a full table it must still cut.
  /// Half the time that checkpoint fails in its install, keeping its cut's
  /// seal, and a liveness check follows (`liveness_step`), as it does half
  /// the time otherwise.
  fn fill_table_step(&mut self, crash_images: bool, long_transactions: bool) -> Outcome {
    self.note("fill the segment table".to_string());
    self.count("segment tables filled");
    let capacity = self.db().wal_segment_capacity();
    let pin = long_transactions && self.rng.gen_bool(0.8);
    if pin {
      self.finish_long()?;
    }
    let pin_at = self.rng.gen_range(1..capacity - 1);
    let mut pinned = false;
    let mut imaged = false;
    for _ in 0..40 {
      let live = wal_segment_test_stats(self.db()).live;
      if live + 1 >= capacity {
        break;
      }
      if pin && !pinned && live >= pin_at {
        pinned = true;
        let db = Arc::clone(self.db());
        self.note("long transaction: begin".to_string());
        let (result, _) = self.long_thread.call(&db, LongRequest::Begin, false);
        result.map_err(|error| format!("the long transaction's begin failed: {error}"))?;
        self.long = Some(Vec::new());
        self.long_cuts = checkpoint_test_cuts(&db);
        // More than it keeps back: its records go to the WAL, and the next
        // spill moves them into a segment it then pins.
        let mut more = Vec::new();
        for _ in 0..30 {
          let key = self.new_key("long");
          let value = self.value(3_000);
          more.push(Write::Create(key, value));
        }
        self.long_write(more)?;
      }
      let count = self.rng.gen_range(20..=60);
      let writes = self.writes(count, 2_000, false);
      let refused_before = self.coverage.get("commits refused: log full").copied();
      // Once the table reaches past the header page's first sector, one
      // commit's crash images (where they apply): its spill's header writes
      // change entries a sector tear splits from the fixed fields.
      let record = !imaged && live >= 10 && self.config.crash_images_apply();
      if record {
        imaged = true;
        self.count("fill commits imaged past the first sector");
      }
      self.commit_step("large commit", writes, record)?;
      if self.coverage.get("commits refused: log full").copied() != refused_before {
        break;
      }
    }
    if pinned {
      self.count("segment tables filled past a pin");
    }
    let count = self.rng.gen_range(1..=20);
    let writes = self.writes(count, 2_000, false);
    self.commit_step("large commit", writes, false)?;
    // Half the time the checkpoint fails in its install (a fault there),
    // which keeps its cut's seal: the table stays full, its newest segment
    // sealed, so the next cut has no entry to spill the WAL into.
    let fail_install = !self.faults_armed && self.rng.gen_bool(0.5);
    if fail_install {
      self.note("arm a checkpoint fault at HeaderWritten (sticky: false)".to_string());
      set_checkpoint_test_db_fault(self.db(), CheckpointPhase::HeaderWritten, false);
      self.count("installs failed on a full table");
      self.faults_armed = true;
      self.faults_seen = true;
    }
    self.background_checkpoint_step(crash_images)?;
    // Then (and half the time otherwise) the pin, if any, ends, and the
    // full table must not wedge the database.
    if fail_install || self.rng.gen_bool(0.5) {
      self.liveness_step()?;
    }
    Ok(())
  }

  /// Run `run`, recording its pager writes and syncs with the file before
  /// them if `record`.
  fn recorded<R>(
    &mut self,
    record: bool,
    run: impl FnOnce() -> R,
  ) -> (R, Vec<IoEvent>, Option<Vec<u8>>) {
    if !record || self.quiesce().is_err() {
      return (run(), Vec::new(), None);
    }
    let base = std::fs::read(&self.path).expect("read the file");
    let (result, events) = io_hooks::record_io_during(run);
    (result, events, Some(base))
  }

  fn commit_step(&mut self, what: &str, writes: Vec<Write>, record: bool) -> Outcome {
    self.step_bytes = write_bytes(&writes);
    self.note(format!("{what} of {} writes", writes.len()));
    let db = Arc::clone(self.db());
    let (result, events, base) = self.recorded(record, || commit_writes(&db, &writes));
    let before = self.state.clone();
    let mut with = self.state.clone();
    apply_writes(&mut with, &writes);
    match result {
      Ok(()) => {
        self.state = with.clone();
        self.count("commits");
      }
      Err(error) if self.allowed_commit_error(&error) => {
        self.count(if matches!(error, KiteError::WalBufferFull) {
          "commits refused: log full"
        } else {
          "commits failed: checkpoint faults"
        });
        self.note(format!("  failed: {error}"));
      }
      Err(error) => return Err(format!("a {what} failed: {error}")),
    }
    drop(db);
    self.count_segments();
    self.check_images(what, base, &events, Some((&before, &with)))?;
    self.check_live(&format!("after a {what}"))
  }

  fn long_step(&mut self, crash_images: bool) -> Outcome {
    let db = Arc::clone(self.db());
    let Some(writes) = self.long.clone() else {
      self.note("long transaction: begin".to_string());
      let (result, _) = self.long_thread.call(&db, LongRequest::Begin, false);
      result.map_err(|error| format!("the long transaction's begin failed: {error}"))?;
      self.long = Some(Vec::new());
      self.long_cuts = checkpoint_test_cuts(&db);
      return Ok(());
    };
    match self.rng.gen_range(0..10) {
      0..=5 => {
        let count = self.rng.gen_range(1..=40);
        let max_len = if self.rng.gen_bool(0.2) { 6_000 } else { 400 };
        let mut more = Vec::with_capacity(count);
        for _ in 0..count {
          let key = self.new_key("long");
          let value = self.value(max_len);
          more.push(Write::Create(key, value));
        }
        drop(db);
        self.long_write(more)
      }
      6..=8 => {
        self.note(format!("long transaction: commit {} writes", writes.len()));
        self.step_bytes = write_bytes(&writes);
        self.long = None;
        let record = crash_images && self.quiesce().is_ok();
        let base = record.then(|| std::fs::read(&self.path).expect("read the file"));
        let (result, events) = self.long_thread.call(&db, LongRequest::Commit, record);
        let before = self.state.clone();
        let mut with = self.state.clone();
        apply_writes(&mut with, &writes);
        match result {
          Ok(()) => {
            self.state = with.clone();
            self.count_long_commit(&db, &writes);
          }
          Err(error) if self.allowed_commit_error(&error) => {
            self.note(format!("  failed: {error}"));
          }
          Err(error) => return Err(format!("the long transaction's commit failed: {error}")),
        }
        drop(db);
        self.check_images(
          "long transaction's commit",
          base,
          &events,
          Some((&before, &with)),
        )?;
        self.check_live("after the long transaction's commit")
      }
      _ => {
        self.note("long transaction: rollback".to_string());
        self.long = None;
        let (result, _) = self.long_thread.call(&db, LongRequest::Rollback, false);
        result.map_err(|error| format!("the long transaction's rollback failed: {error}"))?;
        self.check_live("after the long transaction's rollback")
      }
    }
  }

  /// Make `more` writes in the open long transaction (rolled back on its
  /// thread if one fails).
  fn long_write(&mut self, more: Vec<Write>) -> Outcome {
    let db = Arc::clone(self.db());
    let writes = self.long.clone().unwrap_or_default();
    self.note(format!("long transaction: {} writes", more.len()));
    self.step_bytes = write_bytes(&writes) + write_bytes(&more);
    let (result, _) = self
      .long_thread
      .call(&db, LongRequest::Write(more.clone()), false);
    match result {
      Ok(()) => {
        let mut writes = writes;
        writes.extend(more);
        self.long = Some(writes);
      }
      // Rolled back on its thread.
      Err(error) if self.allowed_commit_error(&error) => {
        self.note(format!("  failed, rolled back: {error}"));
        self.long = None;
      }
      Err(error) => return Err(format!("a long transaction's write failed: {error}")),
    }
    self.check_live("after a long transaction's writes")
  }

  /// End the long transaction, committing or rolling it back.
  fn finish_long(&mut self) -> Outcome {
    let Some(writes) = self.long.take() else {
      return Ok(());
    };
    let db = Arc::clone(self.db());
    if self.rng.gen_bool(0.5) {
      self.note(format!("long transaction: commit {} writes", writes.len()));
      self.step_bytes = write_bytes(&writes);
      match self.long_thread.call(&db, LongRequest::Commit, false).0 {
        Ok(()) => {
          apply_writes(&mut self.state, &writes);
          self.count_long_commit(&db, &writes);
        }
        Err(error) if self.allowed_commit_error(&error) => {
          self.note(format!("  failed: {error}"));
        }
        Err(error) => return Err(format!("the long transaction's commit failed: {error}")),
      }
    } else {
      self.note("long transaction: rollback".to_string());
      self
        .long_thread
        .call(&db, LongRequest::Rollback, false)
        .0
        .map_err(|error| format!("the long transaction's rollback failed: {error}"))?;
    }
    Ok(())
  }

  /// Close the database (checking crash images of the close if `record`),
  /// or drop it (which keeps its WAL segments), and open it again,
  /// read-only first if `read_only`.
  fn reopen_step(&mut self, read_only: bool, record: bool) -> Outcome {
    self.reopen_step_ending(read_only, record, None)
  }

  /// `reopen_step`, ending the database by a drop if `by_drop` says so (or
  /// at random).
  fn reopen_step_ending(
    &mut self,
    read_only: bool,
    record: bool,
    by_drop: Option<bool>,
  ) -> Outcome {
    self.finish_long()?;
    self.quiesce()?;
    self.clear_faults();
    let db = self.db.take().expect("open");
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("the database is still shared"));
    // Sometimes close with a checkpoint thread's run in flight.
    let in_flight = self.config.thread && self.config.auto_checkpoint && self.rng.gen_bool(0.4);
    let by_drop = by_drop.unwrap_or_else(|| self.rng.gen_bool(0.4));
    self.note(format!(
      "{}{}, then reopen{}",
      if by_drop { "drop" } else { "close" },
      if in_flight {
        " with a run in flight"
      } else {
        ""
      },
      if read_only { " read-only first" } else { "" }
    ));
    self.count(if read_only {
      "read-only reopens"
    } else {
      "reopens"
    });
    if by_drop {
      self.count("reopens after a drop");
    }
    if in_flight {
      self.count("closes with a run in flight");
      db.request_background_checkpoint();
      std::thread::sleep(Duration::from_micros(self.rng.gen_range(0..2_000)));
    }
    let record = record && !in_flight;
    let base = record.then(|| std::fs::read(&self.path).expect("read the file"));
    let end = || {
      if by_drop {
        drop(db);
        Ok(())
      } else {
        close_single_file(db)
      }
    };
    let (closed, events) = if record {
      io_hooks::record_io_during(end)
    } else {
      (end(), Vec::new())
    };
    closed.map_err(|error| format!("the close failed: {error}"))?;
    self.check_images("close", base, &events, None)?;
    if read_only {
      let options = self.config.options();
      let mut rng = StdRng::seed_from_u64(self.rng.gen());
      reopen_and_check(
        "the read-only reopen",
        &self.path,
        &options,
        true,
        false,
        &[&self.state],
        move || rng.gen_bool(0.5),
      )?;
    }
    let db = open_model_db(&self.path, &self.config)
      .map_err(|error| format!("the reopen failed: {error}"))?;
    self.faults_seen = false;
    self.db = Some(Arc::new(db));
    self.hint = Some(Hint::Checkpoint);
    self.check_live("after the reopen")
  }

  /// Copy the file between steps (a crash of the process now), and open
  /// the copy; sometimes go on with the copy as the database.
  fn copy_step(&mut self) -> Outcome {
    self.quiesce()?;
    let copy = self.copy_path();
    let read_only = self.rng.gen_bool(0.3);
    self.note(format!(
      "copy the file{}",
      if read_only { ", open it read-only" } else { "" }
    ));
    std::fs::copy(&self.path, &copy).expect("copy the file");
    self.count("copies");
    let twice = self.rng.gen_bool(0.5);
    let mut rng = StdRng::seed_from_u64(self.rng.gen());
    let outcome = reopen_and_check(
      "a copy of the file",
      &copy,
      &self.config.options(),
      read_only,
      twice,
      &[&self.state],
      move || rng.gen_bool(0.5),
    );
    if outcome.is_ok() && self.rng.gen_bool(0.3) {
      let state = self.state.clone();
      return self.adopt("the copy", copy, state);
    }
    let _ = std::fs::remove_file(&copy);
    outcome.map(|_| ())
  }

  /// Go on with the file at `path` (a copy of the database, or a crash
  /// image of it, opened and checked already) as the database, holding
  /// `state`: what a process that crashed and reopened its file would do.
  /// The long transaction rolls back (a crash ends it), and the current
  /// database is dropped and its file removed.
  fn adopt(&mut self, what: &str, path: PathBuf, state: State) -> Outcome {
    if self.long.take().is_some() {
      let db = Arc::clone(self.db());
      self
        .long_thread
        .call(&db, LongRequest::Rollback, false)
        .0
        .map_err(|error| format!("the long transaction's rollback failed: {error}"))?;
    }
    self.quiesce()?;
    self.clear_faults();
    let db = self.db.take().expect("open");
    *self.coverage.entry("cuts").or_default() += checkpoint_test_cuts(&db) as u64;
    *self
      .coverage
      .entry("cuts covering segments without a spill")
      .or_default() += checkpoint_test_covers_without_spill(&db);
    drop(Arc::try_unwrap(db).unwrap_or_else(|_| panic!("the database is still shared")));
    let _ = std::fs::remove_file(&self.path);
    self.note(format!("go on with {what} as the database"));
    self.count(if what == "the copy" {
      "copies gone on with"
    } else {
      "crash images gone on with"
    });
    if !self.state.eq(&state) {
      self.count("crash images gone on with, without the step's commit");
    }
    let db = open_model_db(&path, &self.config)
      .map_err(|error| format!("{what}: the open to go on with it failed: {error}"))?;
    self.path = path;
    self.state = state;
    self.faults_seen = false;
    self.db = Some(Arc::new(db));
    self.hint = Some(Hint::Checkpoint);
    self.check_live(&format!("after going on with {what}"))
  }

  /// Open crash images of a step's writes and syncs `events` from `base`:
  /// some points in the step, and its end. A crash before the end may keep
  /// the step's commit (`commit`: the states before and with it) or not; at
  /// the end the oracle's state stands.
  fn check_images(
    &mut self,
    what: &str,
    base: Option<Vec<u8>>,
    events: &[IoEvent],
    commit: Option<(&State, &State)>,
  ) -> Outcome {
    let Some(base) = base else {
      return Ok(());
    };
    let header_end = 2
      * self
        .db
        .as_ref()
        .map_or(4096, |db| db.header.read().page_size as u64);
    // Every crash model, at the end of the step, and six more images at
    // random points (some with writes after the last sync landing
    // independently). OS crashes only in `Full` mode: `Normal` keeps, through
    // one, only what a sync made durable, and the model tracks no syncs
    // outside the recorded step.
    let full = self.config.sync == SyncMode::Full;
    let mut models: Vec<CrashModel> = CrashModel::FIXED
      .into_iter()
      .filter(|model| full || *model == CrashModel::InOrder)
      .collect();
    let mut chosen: Vec<(usize, CrashModel)> =
      models.iter().map(|&model| (events.len(), model)).collect();
    if full {
      models.push(CrashModel::Independent(0));
    }
    for _ in 0..6 {
      let cut = self.rng.gen_range(0..=events.len());
      let model = match models[self.rng.gen_range(0..models.len())] {
        CrashModel::Independent(_) => CrashModel::Independent(self.rng.gen()),
        model => model,
      };
      chosen.push((cut, model));
    }
    // Sector tears, in `Full` mode (a disk writes 512-byte sectors whole,
    // not pages): of up to two header writes that change the segment table,
    // at each of their sector boundaries, and of one other write over
    // several sectors at one of its boundaries; each both ways.
    if full {
      let mut tables = table_header_writes(&base, events, header_end);
      while tables.len() > 2 {
        tables.swap_remove(self.rng.gen_range(0..tables.len()));
      }
      for cut in tables {
        chosen.extend(
          sector_tears(events, cut)
            .into_iter()
            .map(|model| (cut, model)),
        );
      }
      let data: Vec<(usize, Vec<CrashModel>)> = (1..=events.len())
        .filter(|&cut| {
          matches!(&events[cut - 1], IoEvent::Write { offset, data }
            if *offset >= header_end && data.len() as u64 > SECTOR)
        })
        .map(|cut| (cut, sector_tears(events, cut)))
        .filter(|(_, tears)| !tears.is_empty())
        .collect();
      if !data.is_empty() {
        let (cut, tears) = &data[self.rng.gen_range(0..data.len())];
        // Both ways of one boundary (`sector_tears` lists them in pairs).
        let boundary = 2 * self.rng.gen_range(0..tears.len() / 2);
        chosen.extend(
          tears[boundary..boundary + 2]
            .iter()
            .map(|&model| (*cut, model)),
        );
      }
    }
    chosen.sort_unstable_by_key(|(cut, model)| (*cut, format!("{model:?}")));
    chosen.dedup();
    let tears = chosen
      .iter()
      .filter(|(_, model)| matches!(model, CrashModel::SectorTear { .. }))
      .count() as u64;
    *self.coverage.entry("sector tears").or_default() += tears;
    self.note(format!(
      "  crash images of the {what}: {} events, images {chosen:?}",
      events.len()
    ));
    // Sometimes go on with the last image as the database (not a close's:
    // the database is closed then, and opens again from its own file).
    let go_on = self.rng.gen_bool(0.3) && self.db.is_some();
    let images = chosen.len();
    let mut adopt = None;
    for (index, (cut, model)) in chosen.into_iter().enumerate() {
      let Some(image) = crash_image(&base, events, header_end, cut, model) else {
        continue;
      };
      self.count("crash images");
      let copy = self.copy_path();
      std::fs::write(&copy, &image).expect("write the image");
      // The step's commit, if any, may be there or not, unless the crash
      // comes after it was acknowledged.
      let allowed: Vec<&State> = match commit {
        Some((before, with)) if cut < events.len() => vec![before, with],
        _ => vec![&self.state],
      };
      let read_only = self.rng.gen_bool(0.25);
      let twice = self.rng.gen_bool(0.3);
      let mut rng = StdRng::seed_from_u64(self.rng.gen());
      let matched = reopen_and_check(
        &format!("{what}, crash after {cut} events, {model:?}"),
        &copy,
        &self.config.options(),
        read_only,
        twice,
        &allowed,
        move || rng.gen_bool(0.5),
      );
      match matched {
        Ok(matched) if go_on && index + 1 == images => {
          adopt = Some((
            copy,
            allowed[matched].clone(),
            format!("{cut} events, {model:?}"),
          ));
        }
        outcome => {
          let _ = std::fs::remove_file(&copy);
          outcome?;
        }
      }
    }
    if let Some((path, state, at)) = adopt {
      self.adopt(&format!("a crash image of the {what} ({at})"), path, state)?;
    }
    Ok(())
  }

  fn finish(&mut self) -> Outcome {
    self.liveness_step()?;
    self.quiesce()?;
    self.clear_faults();
    let db = self.db.take().expect("open");
    *self.coverage.entry("cuts").or_default() += checkpoint_test_cuts(&db) as u64;
    *self
      .coverage
      .entry("cuts covering segments without a spill")
      .or_default() += checkpoint_test_covers_without_spill(&db);
    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("the database is still shared"));
    close_single_file(db).map_err(|error| format!("the final close failed: {error}"))?;
    reopen_and_check(
      "the final reopen",
      &self.path,
      &self.config.options(),
      false,
      true,
      &[&self.state],
      || false,
    )
    .map(|_| ())
  }
}

/// The cuts of `events` (recorded from `base`; the header pages are the
/// bytes below `header_end`) whose last event is a header write that
/// changes the WAL segment table: the page it overwrites names another, or
/// is no header.
fn table_header_writes(base: &[u8], events: &[IoEvent], header_end: u64) -> Vec<usize> {
  let table = |page: &[u8]| {
    DbHeaderV1::parse(page)
      .ok()
      .map(|header| header.wal_segments)
  };
  let mut pages: HashMap<u64, Vec<u8>> = HashMap::new();
  let mut cuts = Vec::new();
  for (index, event) in events.iter().enumerate() {
    let IoEvent::Write { offset, data } = event else {
      continue;
    };
    if *offset >= header_end {
      continue;
    }
    let old = pages.remove(offset).unwrap_or_else(|| {
      let start = *offset as usize;
      base
        .get(start..start + data.len())
        .map_or_else(Vec::new, <[u8]>::to_vec)
    });
    if table(&old) != table(data) {
      cuts.push(index + 1);
    }
    pages.insert(*offset, data.clone());
  }
  cuts
}

/// Open the model's database at `path` with `config`'s options and test
/// hooks: failed runs (injected faults) back off for milliseconds (steps
/// wait for the checkpoint thread to answer), and the segment table's size.
fn open_model_db(path: &Path, config: &Config) -> Result<SingleFileDB> {
  let db = open_single_file(path, config.options())?;
  super::super::checkpoint_thread::set_checkpoint_test_backoff(
    &db,
    Duration::from_millis(1),
    Duration::from_millis(8),
  );
  if let Some(entries) = config.table {
    set_wal_segment_test_capacity(&db, entries);
  }
  Ok(db)
}

/// Spill the WAL now if it holds records and the segments have room, as a
/// writer whose records the WAL refused would.
fn force_spill(db: &SingleFileDB) -> Result<bool> {
  let _commit_guard = db.lock_commits();
  db.ensure_writes_allowed()?;
  let mut pager = db.pager.lock();
  let mut wal = db.wal_buffer.lock();
  let mut header = db.header.write();
  if wal.is_empty() {
    return Ok(false);
  }
  let unneeded = db.unneeded_wal_segments(&header, false);
  if !db.can_spill(&header, &unneeded) {
    return Ok(false);
  }
  db.spill_wal(&mut pager, &mut wal, &mut header, &[], &unneeded)?;
  Ok(true)
}

/// Run seed `seed` for `steps` steps, returning what it covered; a failure
/// names the seed, its options and the steps that led to it.
fn run_seed(seed: u64, steps: u64) -> std::result::Result<Coverage, String> {
  let caught = std::panic::catch_unwind(|| {
    let mut model = Model::new(seed);
    for step in 0..steps {
      if let Err(error) = model.step() {
        let config = model.config.clone();
        let recent: Vec<String> = model.log.iter().rev().take(40).rev().cloned().collect();
        model.clear_faults();
        return Err(format!(
          "step {step}: {error}\n  options: {config:?}\n  steps:\n    {}",
          recent.join("\n    ")
        ));
      }
    }
    let config = model.config.clone();
    model
      .finish()
      .map_err(|error| format!("at the end: {error}\n  options: {config:?}"))?;
    let mut coverage = std::mem::take(&mut model.coverage);
    coverage.insert("seeds", 1);
    if config.thread {
      coverage.insert("seeds with the checkpoint thread", 1);
    }
    if !config.auto_checkpoint {
      coverage.insert("seeds without automatic checkpoints", 1);
    }
    if !config.background {
      coverage.insert("seeds without background checkpoints", 1);
    }
    Ok(coverage)
  });
  caught
    .unwrap_or_else(|panic| {
      let message = panic
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default();
      Err(format!("panicked: {message}"))
    })
    .map_err(|error| format!("seed {seed} (KITE_MODEL_SEED={seed}): {error}"))
}

/// The seeds the environment asks for (see the module docs), else the
/// first `default_seeds`.
fn env_seeds(default_seeds: u64) -> Vec<u64> {
  match std::env::var("KITE_MODEL_SEED") {
    Ok(seeds) => seeds
      .split(',')
      .map(|seed| {
        seed
          .trim()
          .parse()
          .unwrap_or_else(|_| panic!("KITE_MODEL_SEED={seeds:?} is not a list of numbers"))
      })
      .collect(),
    Err(_) => {
      let first = env_number("KITE_MODEL_FIRST_SEED", 0);
      let count = env_number("KITE_MODEL_SEEDS", default_seeds);
      (first..first + count).collect()
    }
  }
}

/// Run `seeds` on several threads, and return every failing seed's
/// failure. A seed that runs longer than `KITE_MODEL_SEED_TIMEOUT` seconds
/// (default 120) fails as hung; its thread is left behind and another takes
/// its place.
fn run_seeds(seeds: Vec<u64>) -> Vec<(u64, String)> {
  let steps = env_number("KITE_MODEL_STEPS", 60);
  let cpus = std::thread::available_parallelism().map_or(4, |count| count.get()) as u64;
  // A few by default: the quick run shares the machine with the other tests.
  let threads = env_number("KITE_MODEL_THREADS", cpus.min(4)).max(1) as usize;
  let timeout = Duration::from_secs(env_number("KITE_MODEL_SEED_TIMEOUT", 120));
  let verbose = std::env::var("KITE_MODEL_VERBOSE").is_ok();

  struct Shared {
    seeds: Vec<u64>,
    next: std::sync::atomic::AtomicUsize,
    /// Each worker's seed in progress, and when it started.
    running: std::sync::Mutex<HashMap<usize, (u64, Instant)>>,
  }
  let shared = Arc::new(Shared {
    seeds,
    next: std::sync::atomic::AtomicUsize::new(0),
    running: std::sync::Mutex::new(HashMap::new()),
  });
  let (results_tx, results) = mpsc::channel::<(u64, std::result::Result<Coverage, String>)>();
  let spawn_worker = |worker: usize| {
    let (shared, results_tx) = (Arc::clone(&shared), results_tx.clone());
    std::thread::Builder::new()
      .name(format!("model-worker-{worker}"))
      .spawn(move || loop {
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        let Some(&seed) = shared.seeds.get(index) else {
          break;
        };
        shared
          .running
          .lock()
          .expect("running")
          .insert(worker, (seed, Instant::now()));
        let outcome = run_seed(seed, steps);
        shared.running.lock().expect("running").remove(&worker);
        if results_tx.send((seed, outcome)).is_err() {
          break;
        }
      })
      .expect("spawn a model worker");
  };
  let mut workers = threads.min(shared.seeds.len());
  for worker in 0..workers {
    spawn_worker(worker);
  }

  let started = Instant::now();
  let mut coverage = Coverage::new();
  let mut failures = Vec::new();
  let mut finished = 0;
  while finished < shared.seeds.len() {
    if let Ok((seed, outcome)) = results.recv_timeout(Duration::from_secs(1)) {
      finished += 1;
      match outcome {
        Ok(covered) => {
          for (what, count) in covered {
            let total = coverage.entry(what).or_default();
            *total = if what == "most live segments" {
              (*total).max(count)
            } else {
              *total + count
            };
          }
        }
        Err(failure) => {
          eprintln!("model test: {failure}");
          failures.push((seed, failure));
        }
      }
      if verbose {
        eprintln!(
          "model test: seed {seed} done ({finished} of {})",
          shared.seeds.len()
        );
      }
    }
    let hung: Vec<(usize, u64, Duration)> = shared
      .running
      .lock()
      .expect("running")
      .iter()
      .filter(|(_, (_, since))| since.elapsed() > timeout)
      .map(|(worker, (seed, since))| (*worker, *seed, since.elapsed()))
      .collect();
    for (worker, seed, ran) in hung {
      shared.running.lock().expect("running").remove(&worker);
      let failure = format!(
        "seed {seed} (KITE_MODEL_SEED={seed}): hung: still running after {ran:?} (trace it with \
         KITE_MODEL_TRACE=1)"
      );
      eprintln!("model test: {failure}");
      failures.push((seed, failure));
      finished += 1;
      spawn_worker(workers);
      workers += 1;
    }
  }
  eprintln!(
    "model test: {} seeds of {steps} steps in {:?}, {} failed; covered {coverage:?}",
    shared.seeds.len(),
    started.elapsed(),
    failures.len()
  );
  failures
}

/// The model test's quick run (see the module docs for the long one).
#[test]
fn wal_segment_and_checkpoint_model() {
  let seeds = env_seeds(100);
  let count = seeds.len();
  let failures = run_seeds(seeds);
  assert!(
    failures.is_empty(),
    "{} of {count} seeds failed:\n{}",
    failures.len(),
    failures
      .iter()
      .map(|(_, failure)| failure.as_str())
      .collect::<Vec<_>>()
      .join("\n")
  );
}

/// Seeds that catch the bugs reviews found here, each re-made in the code
/// (a mutant): one or more per mutant, every one of which failed its mutant
/// on every run when derived, run alone and all together. The quick run's
/// seeds catch most of them, but only by chance: any change to the model's
/// random draws moves what each seed does. Re-derive the list whenever the
/// model changes, with `scripts/model-regression-seeds.py`, which holds the
/// mutants, applies each in turn, and prints the list. The mutants:
///
/// - `r7`: open forgets the transactions whose records a spill moved (R7).
/// - `f3-ok`, `f3-declined`: a cut with the segment table full covers
///   nothing, and returns `Ok`, or declines (F3, reverted two ways).
/// - `needed-after`: `wal_segments_needed_after` ignores the transactions
///   that commit after the cut.
/// - `unsealed`: a cut does not seal the newest segment.
/// - `forget-spilled`: an install forgets every spilled transaction.
/// - `r12`: a header page's footer checksum covers its fixed fields'
///   checksum, so not them (R12).
/// - `spill-slot`: a spill does not sync its second header slot.
const REGRESSION_SEEDS: &[(u64, &str)] = &[
  (1, "r7"),
  (100, "r7"),
  (32, "f3-ok"),
  (207, "f3-ok"),
  (32, "f3-declined"),
  (207, "f3-declined"),
  (1, "needed-after"),
  (6, "needed-after"),
  (13, "unsealed"),
  (22, "unsealed"),
  (1, "forget-spilled"),
  (6, "forget-spilled"),
  (13, "r12"),
  (86, "r12"),
  (86, "spill-slot"),
  (126, "spill-slot"),
];

/// The pinned regression seeds (`REGRESSION_SEEDS`), each once, with the
/// model's default steps.
#[test]
fn wal_segment_and_checkpoint_model_regression_seeds() {
  let mut seeds: Vec<u64> = REGRESSION_SEEDS.iter().map(|(seed, _)| *seed).collect();
  seeds.sort_unstable();
  seeds.dedup();
  let count = seeds.len();
  let failures = run_seeds(seeds);
  assert!(
    failures.is_empty(),
    "{} of {count} regression seeds failed:\n{}",
    failures.len(),
    failures
      .iter()
      .map(|(seed, failure)| {
        let mutants: Vec<&str> = REGRESSION_SEEDS
          .iter()
          .filter(|(pinned, _)| pinned == seed)
          .map(|(_, mutant)| *mutant)
          .collect();
        format!("(pinned for {mutants:?}) {failure}")
      })
      .collect::<Vec<_>>()
      .join("\n")
  );
}
