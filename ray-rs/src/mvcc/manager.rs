//! MVCC Manager - coordinates MVCC components
//!
//! Mirrors src/mvcc/index.ts (MvccManager)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

#[cfg(not(target_arch = "wasm32"))]
use crate::mvcc::gc::GcSleeper;
use crate::mvcc::{
  ConflictDetector, GarbageCollector, GcConfig, GcResult, TxManager, VersionChainManager,
};
use crate::types::{Timestamp, TxId};

/// MVCC Manager - coordinates all MVCC components
///
/// Lock order: `tx_manager` -> `version_chain` -> `gc`, nested inside the database-wide order
/// documented in `core/single_file/read.rs`. GC takes them one at a time.
pub struct MvccManager {
  pub tx_manager: Arc<Mutex<TxManager>>,
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
    Self {
      tx_manager: Arc::new(Mutex::new(TxManager::with_initial(
        initial_tx_id,
        initial_commit_ts,
      ))),
      version_chain: Arc::new(RwLock::new(VersionChainManager::new())),
      conflict_detector: ConflictDetector::new(),
      gc: Arc::new(Mutex::new(GarbageCollector::with_config(gc_config))),
      history_ts: Arc::new(AtomicU64::new(0)),
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
    let mut vc = self.version_chain.write();
    record(&mut vc);
    self.history_ts.fetch_max(commit_ts, Ordering::Release);
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

impl Drop for MvccManager {
  fn drop(&mut self) {
    self.stop();
  }
}
