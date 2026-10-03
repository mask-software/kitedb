//! MVCC Manager - coordinates MVCC components
//!
//! Mirrors src/mvcc/index.ts (MvccManager)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use parking_lot::{Mutex, RwLock, RwLockWriteGuard};

#[cfg(not(target_arch = "wasm32"))]
use crate::mvcc::gc::GcSleeper;
use crate::mvcc::{
  ConflictDetector, GarbageCollector, GcConfig, GcResult, OpenTransactions, TxManager,
  VersionChainManager,
};
use crate::types::{Timestamp, TxId};

/// MVCC Manager - coordinates all MVCC components
///
/// Lock order: `tx_manager` -> `version_chain` -> `gc`, nested inside the database-wide order
/// documented in `core/single_file/read.rs`. GC takes them one at a time.
pub struct MvccManager {
  pub tx_manager: Arc<Mutex<TxManager>>,
  /// The open transactions, which begin and end (unless they commit)
  /// without `tx_manager`'s lock; the transaction manager holds them too
  /// (`TxManager::open_transactions`).
  pub open: Arc<OpenTransactions>,
  /// Readers share it; commits recording history and GC take it exclusively.
  pub version_chain: Arc<RwLock<VersionChainManager>>,
  pub conflict_detector: ConflictDetector,
  pub gc: Arc<Mutex<GarbageCollector>>,
  /// The newest commit timestamp the version chains hold a version for (0
  /// when they hold none), or more. A reader whose snapshot is newer sees the
  /// newest version of every chain, so the chains cannot answer for it (see
  /// `mvcc::version_chain`) and it skips them. Raised by commits that record
  /// history, lowered by GC, both under the version chain write lock.
  history_ts: Arc<AtomicU64>,
  /// GC's retention period (`GcConfig::retention_ms`), for `history_horizon`.
  /// Fixed when the manager is made: commits note their times only if it is
  /// above 0 (`TxManager::set_wall_clock_tracking`).
  retention_ms: u64,
  gc_stop: Arc<AtomicBool>,
  /// Wakes the GC thread from its sleep between runs on stop
  #[cfg(not(target_arch = "wasm32"))]
  gc_sleeper: Arc<GcSleeper>,
  #[cfg(not(target_arch = "wasm32"))]
  gc_handle: Mutex<Option<thread::JoinHandle<()>>>,
  #[cfg(target_arch = "wasm32")]
  gc_handle: Mutex<()>,
}

impl MvccManager {
  /// Create a new MVCC manager
  pub fn new(initial_tx_id: TxId, initial_commit_ts: Timestamp, gc_config: GcConfig) -> Self {
    let retention_ms = gc_config.retention_ms;
    let mut tx_manager = TxManager::with_initial(initial_tx_id, initial_commit_ts);
    // Without a retention period no horizon needs commit times, and a
    // commit reads no clock.
    tx_manager.set_wall_clock_tracking(retention_ms > 0);
    let open = Arc::clone(tx_manager.open_transactions());
    Self {
      tx_manager: Arc::new(Mutex::new(tx_manager)),
      open,
      version_chain: Arc::new(RwLock::new(VersionChainManager::new())),
      conflict_detector: ConflictDetector::new(),
      gc: Arc::new(Mutex::new(GarbageCollector::with_config(gc_config))),
      history_ts: Arc::new(AtomicU64::new(0)),
      retention_ms,
      gc_stop: Arc::new(AtomicBool::new(false)),
      #[cfg(not(target_arch = "wasm32"))]
      gc_sleeper: Arc::new(GcSleeper::default()),
      #[cfg(not(target_arch = "wasm32"))]
      gc_handle: Mutex::new(None),
      #[cfg(target_arch = "wasm32")]
      gc_handle: Mutex::new(()),
    }
  }

  /// See `history_ts` (the field).
  pub fn history_ts(&self) -> Timestamp {
    self.history_ts.load(Ordering::Acquire)
  }

  /// Record a commit's history: run `record` under the version chain write
  /// lock and raise `history_ts` to `commit_ts`.
  pub fn record_history(
    &self,
    commit_ts: Timestamp,
    record: impl FnOnce(&mut VersionChainManager),
  ) {
    self.history_writer().record(commit_ts, record);
  }

  /// The version chains, held (write-locked) to record the history of
  /// several commits, in commit order, under one lock (see `HistoryWriter`).
  pub fn history_writer(&self) -> HistoryWriter<'_> {
    HistoryWriter {
      chains: self.version_chain.write(),
      history_ts: &self.history_ts,
      newest: 0,
    }
  }

  /// The oldest snapshot the version history must still answer for: GC's
  /// horizon (see `GarbageCollector::run_scoped`), the oldest open
  /// transaction's snapshot, or older while the retention period keeps
  /// commits; at most a few commits older (`TxManager::min_active_ts_bound`).
  /// `tx_manager` is the transaction manager, locked.
  pub fn history_horizon(&self, tx_manager: &TxManager) -> Timestamp {
    let min_active_ts = tx_manager.min_active_ts_bound();
    if self.retention_ms == 0 {
      // The retention horizon is then the next commit's, no older.
      return min_active_ts;
    }
    min_active_ts.min(tx_manager.retention_horizon_ts(self.retention_ms))
  }

  /// Run one GC cycle now.
  pub fn run_gc(&self) -> GcResult {
    Self::gc_pass(
      &self.gc,
      &self.tx_manager,
      &self.version_chain,
      &self.history_ts,
    )
  }

  /// One GC cycle (`GarbageCollector::run_scoped`), then `history_ts` lowered
  /// to what the chains still hold.
  fn gc_pass(
    gc: &Mutex<GarbageCollector>,
    tx_manager: &Mutex<TxManager>,
    version_chain: &RwLock<VersionChainManager>,
    history_ts: &AtomicU64,
  ) -> GcResult {
    GarbageCollector::run_scoped(gc, tx_manager, version_chain, |vc| {
      history_ts.store(vc.newest_commit_ts(), Ordering::Release);
    })
  }

  /// Initialize MVCC (starts background GC)
  #[cfg(not(target_arch = "wasm32"))]
  pub fn start(&self) {
    let mut handle_guard = self.gc_handle.lock();
    if handle_guard.is_some() {
      return;
    }

    self.gc_stop.store(false, Ordering::SeqCst);

    // Run immediately on start
    let _ = self.run_gc();

    let tx_mgr = self.tx_manager.clone();
    let vc = self.version_chain.clone();
    let gc = self.gc.clone();
    let history_ts = self.history_ts.clone();
    let stop_flag = self.gc_stop.clone();
    let sleeper = self.gc_sleeper.clone();

    let handle = thread::spawn(move || loop {
      let interval_ms = {
        let gc = gc.lock();
        gc.config().interval_ms
      };

      if sleeper.sleep(&stop_flag, Duration::from_millis(interval_ms)) {
        break;
      }

      let _ = Self::gc_pass(&gc, &tx_mgr, &vc, &history_ts);
    });

    *handle_guard = Some(handle);
  }

  /// Whether the background GC thread runs (test instrumentation).
  #[cfg(all(test, not(target_arch = "wasm32")))]
  pub(crate) fn gc_thread_running(&self) -> bool {
    self.gc_handle.lock().is_some()
  }

  #[cfg(target_arch = "wasm32")]
  pub fn start(&self) {
    // No background threads on wasm; run one GC cycle and return.
    let _ = self.run_gc();
  }

  /// Shutdown MVCC (stop background GC)
  pub fn stop(&self) {
    self.gc_stop.store(true, Ordering::SeqCst);
    #[cfg(not(target_arch = "wasm32"))]
    {
      self.gc_sleeper.wake();
      if let Some(handle) = self.gc_handle.lock().take() {
        let _ = handle.join();
      }
    }
  }
}

/// The version chains, write-locked, for recording commits' history
/// (`MvccManager::history_writer`). Raises `history_ts` to the newest commit
/// it recorded before it releases the lock.
pub struct HistoryWriter<'a> {
  chains: RwLockWriteGuard<'a, VersionChainManager>,
  history_ts: &'a AtomicU64,
  newest: Timestamp,
}

impl HistoryWriter<'_> {
  /// Record the history of the commit at `commit_ts` with `record`.
  pub fn record<R>(
    &mut self,
    commit_ts: Timestamp,
    record: impl FnOnce(&mut VersionChainManager) -> R,
  ) -> R {
    self.newest = self.newest.max(commit_ts);
    record(&mut self.chains)
  }

  /// The chains as they are, for reads that plan a recording.
  pub fn chains(&self) -> &VersionChainManager {
    &self.chains
  }
}

impl Drop for HistoryWriter<'_> {
  fn drop(&mut self) {
    // The chains are still held: GC lowers `history_ts` under them.
    self.history_ts.fetch_max(self.newest, Ordering::Release);
  }
}

impl Drop for MvccManager {
  fn drop(&mut self) {
    self.stop();
  }
}
