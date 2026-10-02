//! MVCC Manager - coordinates MVCC components
//!
//! Mirrors src/mvcc/index.ts (MvccManager)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use parking_lot::Mutex;

#[cfg(not(target_arch = "wasm32"))]
use crate::mvcc::gc::GcSleeper;
use crate::mvcc::{ConflictDetector, GarbageCollector, GcConfig, TxManager, VersionChainManager};
use crate::types::{Timestamp, TxId};

/// MVCC Manager - coordinates all MVCC components
///
/// Lock order: `tx_manager` -> `version_chain` -> `gc`, nested inside the database-wide order
/// documented in `core/single_file/read.rs`.
pub struct MvccManager {
  pub tx_manager: Arc<Mutex<TxManager>>,
  pub version_chain: Arc<Mutex<VersionChainManager>>,
  pub conflict_detector: ConflictDetector,
  pub gc: Arc<Mutex<GarbageCollector>>,
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
      version_chain: Arc::new(Mutex::new(VersionChainManager::new())),
      conflict_detector: ConflictDetector::new(),
      gc: Arc::new(Mutex::new(GarbageCollector::with_config(gc_config))),
      gc_stop: Arc::new(AtomicBool::new(false)),
      #[cfg(not(target_arch = "wasm32"))]
      gc_sleeper: Arc::new(GcSleeper::default()),
      #[cfg(not(target_arch = "wasm32"))]
      gc_handle: Mutex::new(None),
      #[cfg(target_arch = "wasm32")]
      gc_handle: Mutex::new(()),
    }
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
    {
      let mut tx_mgr = self.tx_manager.lock();
      let mut vc = self.version_chain.lock();
      let mut gc = self.gc.lock();
      let _ = gc.run_gc(&mut tx_mgr, &mut vc);
    }

    let tx_mgr = self.tx_manager.clone();
    let vc = self.version_chain.clone();
    let gc = self.gc.clone();
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

      let mut tx_mgr = tx_mgr.lock();
      let mut vc = vc.lock();
      let mut gc = gc.lock();
      let _ = gc.run_gc(&mut tx_mgr, &mut vc);
    });

    *handle_guard = Some(handle);
  }

  #[cfg(target_arch = "wasm32")]
  pub fn start(&self) {
    // No background threads on wasm; run one GC cycle and return.
    let mut tx_mgr = self.tx_manager.lock();
    let mut vc = self.version_chain.lock();
    let mut gc = self.gc.lock();
    let _ = gc.run_gc(&mut tx_mgr, &mut vc);
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
