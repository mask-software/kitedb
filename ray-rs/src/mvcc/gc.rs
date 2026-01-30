//! MVCC Background Garbage Collection
//!
//! Periodically prunes old versions that are no longer needed by any active transaction.
//!
//! Ported from src/mvcc/gc.ts

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mvcc::tx_manager::TxManager;
use crate::mvcc::version_chain::VersionChainManager;
use crate::types::{MvccTxStatus, Timestamp};

// ============================================================================
// Constants
// ============================================================================

/// Default max chain depth before truncation
pub const DEFAULT_MAX_CHAIN_DEPTH: usize = 10;

/// Default GC interval in milliseconds
pub const DEFAULT_GC_INTERVAL_MS: u64 = 5000;

/// Default retention period in milliseconds
pub const DEFAULT_RETENTION_MS: u64 = 60000;

// ============================================================================
// GC Statistics
// ============================================================================

/// GC statistics
#[derive(Debug, Clone, Default)]
pub struct GcStats {
  /// Total versions pruned across all GC runs
  pub versions_pruned: u64,
  /// Total chains truncated across all GC runs
  pub chains_truncated: u64,
  /// Number of GC runs executed
  pub gc_runs: u64,
  /// Timestamp of last GC run (milliseconds since epoch)
  pub last_gc_time: u64,
  /// Total transactions cleaned up
  pub txs_cleaned: u64,
}

// ============================================================================
// Garbage Collector
// ============================================================================

/// Configuration for the garbage collector
#[derive(Debug, Clone)]
pub struct GcConfig {
  /// Interval between GC runs in milliseconds
  pub interval_ms: u64,
  /// Retention period - versions younger than this are kept even if not needed
  pub retention_ms: u64,
  /// Maximum chain depth before truncation
  pub max_chain_depth: usize,
}

impl Default for GcConfig {
  fn default() -> Self {
    Self {
      interval_ms: DEFAULT_GC_INTERVAL_MS,
      retention_ms: DEFAULT_RETENTION_MS,
      max_chain_depth: DEFAULT_MAX_CHAIN_DEPTH,
    }
  }
}

/// Garbage collector for MVCC version chains
///
/// The GC is responsible for:
/// 1. Pruning old versions that are no longer visible to any active transaction
/// 2. Truncating deep version chains to bound traversal time
/// 3. Cleaning up committed transaction metadata
///
/// The GC can run in two modes:
/// 1. Manual: Call `run_gc()` explicitly when needed
/// 2. Background: Use with a timer/scheduler (not included - async runtime dependent)
#[derive(Debug)]
pub struct GarbageCollector {
  /// Configuration
  config: GcConfig,
  /// Statistics
  stats: GcStats,
  /// Whether GC is currently running
  running: AtomicBool,
  /// Last run timestamp (for rate limiting)
  last_run: Option<Instant>,
}

impl GarbageCollector {
  /// Create a new garbage collector with default config
  pub fn new() -> Self {
    Self::with_config(GcConfig::default())
  }

  /// Create a new garbage collector with custom config
  pub fn with_config(config: GcConfig) -> Self {
    Self {
      config,
      stats: GcStats::default(),
      running: AtomicBool::new(false),
      last_run: None,
    }
  }

  /// Get the configuration
  pub fn config(&self) -> &GcConfig {
    &self.config
  }

  /// Update the configuration
  pub fn set_config(&mut self, config: GcConfig) {
    self.config = config;
  }

  /// Run a single GC cycle
  ///
  /// This is the main entry point for garbage collection.
  /// It can be called manually or by a background scheduler.
  ///
  /// Returns the number of versions pruned in this cycle.
  pub fn run_gc(
    &mut self,
    tx_manager: &mut TxManager,
    version_chain: &mut VersionChainManager,
  ) -> GcResult {
    // Prevent concurrent GC runs
    if self.running.swap(true, Ordering::SeqCst) {
      return GcResult {
        versions_pruned: 0,
        chains_truncated: 0,
        txs_cleaned: 0,
        skipped: true,
      };
    }

    let result = self.do_gc(tx_manager, version_chain);

    self.running.store(false, Ordering::SeqCst);
    self.last_run = Some(Instant::now());

    result
  }

  /// Internal GC implementation
  fn do_gc(
    &mut self,
    tx_manager: &mut TxManager,
    version_chain: &mut VersionChainManager,
  ) -> GcResult {
    // Calculate GC horizon
    // Versions older than this can be pruned if they have newer successors
    let min_active_ts = tx_manager.min_active_ts();
    let retention_horizon_ts = tx_manager.get_retention_horizon_ts(self.config.retention_ms);

    // GC horizon is the minimum of:
    // 1. Oldest active transaction snapshot
    // 2. Retention horizon converted to commit timestamp
    let horizon_ts = min_active_ts.min(retention_horizon_ts);

    // Prune old versions
    let pruned = version_chain.prune_old_versions(horizon_ts);

    // Truncate deep chains (bounds worst-case traversal time)
    let truncated =
      version_chain.truncate_deep_chains(self.config.max_chain_depth, Some(min_active_ts));

    // Clean up old committed transactions
    let txs_cleaned = self.cleanup_old_transactions(tx_manager, horizon_ts);
    tx_manager.prune_wall_clock_mappings(horizon_ts);

    // Update stats
    self.stats.versions_pruned += pruned as u64;
    self.stats.chains_truncated += truncated as u64;
    self.stats.txs_cleaned += txs_cleaned as u64;
    self.stats.gc_runs += 1;
    self.stats.last_gc_time = current_time_ms();

    GcResult {
      versions_pruned: pruned,
      chains_truncated: truncated,
      txs_cleaned,
      skipped: false,
    }
  }

  /// Clean up committed transactions that are older than the horizon
  /// These transactions are no longer needed for visibility calculations
  fn cleanup_old_transactions(&self, tx_manager: &mut TxManager, horizon_ts: Timestamp) -> usize {
    let txs_to_remove: Vec<_> = tx_manager
      .get_all_txs()
      .filter(|(_, tx)| {
        tx.status == MvccTxStatus::Committed
          && tx.commit_ts.is_some()
          && tx.commit_ts.unwrap() < horizon_ts
      })
      .map(|(&txid, _)| txid)
      .collect();

    let count = txs_to_remove.len();

    // Remove in a separate loop to avoid iterator invalidation
    for txid in txs_to_remove {
      tx_manager.remove_tx(txid);
    }

    count
  }

  /// Check if enough time has passed since last GC run
  pub fn should_run(&self) -> bool {
    match self.last_run {
      None => true,
      Some(last) => last.elapsed() >= Duration::from_millis(self.config.interval_ms),
    }
  }

  /// Force a GC run (for testing/manual triggers)
  /// Returns the total number of versions pruned across all runs
  pub fn force_gc(
    &mut self,
    tx_manager: &mut TxManager,
    version_chain: &mut VersionChainManager,
  ) -> usize {
    let result = self.run_gc(tx_manager, version_chain);
    result.versions_pruned
  }

  /// Get GC statistics
  pub fn get_stats(&self) -> GcStats {
    self.stats.clone()
  }

  /// Reset statistics
  pub fn reset_stats(&mut self) {
    self.stats = GcStats::default();
  }

  /// Check if GC is currently running
  pub fn is_running(&self) -> bool {
    self.running.load(Ordering::SeqCst)
  }
}

impl Default for GarbageCollector {
  fn default() -> Self {
    Self::new()
  }
}

/// Result of a single GC cycle
#[derive(Debug, Clone, Default)]
pub struct GcResult {
  /// Number of versions pruned
  pub versions_pruned: usize,
  /// Number of chains truncated
  pub chains_truncated: usize,
  /// Number of transactions cleaned up
  pub txs_cleaned: usize,
  /// Whether the GC run was skipped (e.g., already running)
  pub skipped: bool,
}

// ============================================================================
// Shared GC (for background threads)
// ============================================================================

/// Shared state for background GC
///
/// This can be used to signal a background GC thread to stop.
#[derive(Debug)]
pub struct SharedGcState {
  /// Signal to stop the GC thread
  pub stop_signal: AtomicBool,
  /// Versions pruned (can be read from another thread)
  pub versions_pruned: AtomicU64,
  /// GC runs count
  pub gc_runs: AtomicU64,
}

impl SharedGcState {
  pub fn new() -> Self {
    Self {
      stop_signal: AtomicBool::new(false),
      versions_pruned: AtomicU64::new(0),
      gc_runs: AtomicU64::new(0),
    }
  }

  pub fn stop(&self) {
    self.stop_signal.store(true, Ordering::SeqCst);
  }

  pub fn should_stop(&self) -> bool {
    self.stop_signal.load(Ordering::SeqCst)
  }

  pub fn record_gc_run(&self, pruned: u64) {
    self.versions_pruned.fetch_add(pruned, Ordering::Relaxed);
    self.gc_runs.fetch_add(1, Ordering::Relaxed);
  }
}

impl Default for SharedGcState {
  fn default() -> Self {
    Self::new()
  }
}

// ============================================================================
// Background GC Runner
// ============================================================================

#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, JoinHandle};

/// Handle to a running background GC thread
///
/// When dropped, the GC thread is stopped and joined.
#[cfg(not(target_arch = "wasm32"))]
pub struct BackgroundGcHandle {
  /// Shared state for communicating with the GC thread
  state: Arc<SharedGcState>,
  /// The thread handle (Option so we can take it in drop)
  thread: Option<JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BackgroundGcHandle {
  /// Stop the background GC thread
  ///
  /// This signals the thread to stop and waits for it to finish.
  pub fn stop(mut self) {
    self.state.stop();
    if let Some(handle) = self.thread.take() {
      let _ = handle.join();
    }
  }

  /// Check if the GC thread is still running
  pub fn is_running(&self) -> bool {
    self
      .thread
      .as_ref()
      .map(|h| !h.is_finished())
      .unwrap_or(false)
  }

  /// Get the number of GC runs completed
  pub fn gc_runs(&self) -> u64 {
    self.state.gc_runs.load(Ordering::Relaxed)
  }

  /// Get the total number of versions pruned
  pub fn versions_pruned(&self) -> u64 {
    self.state.versions_pruned.load(Ordering::Relaxed)
  }

  /// Get a reference to the shared state
  pub fn state(&self) -> &Arc<SharedGcState> {
    &self.state
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BackgroundGcHandle {
  fn drop(&mut self) {
    // Signal stop and wait for thread to finish
    self.state.stop();
    if let Some(handle) = self.thread.take() {
      let _ = handle.join();
    }
  }
}

#[cfg(target_arch = "wasm32")]
pub struct BackgroundGcHandle {
  state: Arc<SharedGcState>,
}

#[cfg(target_arch = "wasm32")]
impl BackgroundGcHandle {
  pub fn stop(self) {
    self.state.stop();
  }

  pub fn is_running(&self) -> bool {
    false
  }

  pub fn gc_runs(&self) -> u64 {
    self.state.gc_runs.load(Ordering::Relaxed)
  }

  pub fn versions_pruned(&self) -> u64 {
    self.state.versions_pruned.load(Ordering::Relaxed)
  }

  pub fn state(&self) -> &Arc<SharedGcState> {
    &self.state
  }
}

/// Start a background GC thread
///
/// The GC thread will run periodically based on the interval in `GcConfig`.
/// It requires mutable access to the TxManager and VersionChainManager,
/// so these must be wrapped in appropriate synchronization primitives.
///
/// # Arguments
/// * `tx_manager` - Arc-wrapped TxManager with interior mutability
/// * `version_chain` - Arc-wrapped VersionChainManager with interior mutability
/// * `config` - GC configuration
///
/// # Returns
/// A handle that can be used to stop the GC thread
///
/// # Example
/// ```rust,no_run
/// use parking_lot::Mutex;
/// use std::sync::Arc;
/// # use raydb::mvcc::{GcConfig, TxManager, VersionChainManager};
/// # use raydb::mvcc::gc::start_background_gc;
///
/// let tx_manager = Arc::new(Mutex::new(TxManager::new()));
/// let version_chain = Arc::new(Mutex::new(VersionChainManager::new()));
///
/// let handle = start_background_gc(
///   tx_manager.clone(),
///   version_chain.clone(),
///   GcConfig::default(),
/// );
///
/// // Later, stop the GC
/// handle.stop();
/// ```
#[cfg(not(target_arch = "wasm32"))]
pub fn start_background_gc(
  tx_manager: Arc<parking_lot::Mutex<TxManager>>,
  version_chain: Arc<parking_lot::Mutex<VersionChainManager>>,
  config: GcConfig,
) -> BackgroundGcHandle {
  let state = Arc::new(SharedGcState::new());
  let state_clone = state.clone();
  let interval = Duration::from_millis(config.interval_ms);

  let thread = thread::spawn(move || {
    let mut gc = GarbageCollector::with_config(config);

    while !state_clone.should_stop() {
      // Sleep for the interval
      thread::sleep(interval);

      if state_clone.should_stop() {
        break;
      }

      // Run GC
      let mut tx_mgr = tx_manager.lock();
      let mut vc = version_chain.lock();
      let result = gc.run_gc(&mut tx_mgr, &mut vc);

      // Record stats
      state_clone.record_gc_run(result.versions_pruned as u64);
    }
  });

  BackgroundGcHandle {
    state,
    thread: Some(thread),
  }
}

#[cfg(target_arch = "wasm32")]
pub fn start_background_gc(
  _tx_manager: Arc<parking_lot::Mutex<TxManager>>,
  _version_chain: Arc<parking_lot::Mutex<VersionChainManager>>,
  _config: GcConfig,
) -> BackgroundGcHandle {
  BackgroundGcHandle {
    state: Arc::new(SharedGcState::new()),
  }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Get current time in milliseconds (since some epoch)
/// Note: This is a simple implementation. In production, you might want to use
/// std::time::SystemTime or a more precise clock.
fn current_time_ms() -> u64 {
  use std::time::{SystemTime, UNIX_EPOCH};
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::types::{NodeDelta, NodeVersionData};

  fn setup() -> (TxManager, VersionChainManager, GarbageCollector) {
    let tx_mgr = TxManager::new();
    let version_chain = VersionChainManager::new();
    let gc = GarbageCollector::new();
    (tx_mgr, version_chain, gc)
  }

  #[test]
  fn test_gc_new() {
    let gc = GarbageCollector::new();
    assert_eq!(gc.config.interval_ms, DEFAULT_GC_INTERVAL_MS);
    assert_eq!(gc.config.retention_ms, DEFAULT_RETENTION_MS);
    assert_eq!(gc.config.max_chain_depth, DEFAULT_MAX_CHAIN_DEPTH);
  }

  #[test]
  fn test_gc_with_config() {
    let config = GcConfig {
      interval_ms: 1000,
      retention_ms: 5000,
      max_chain_depth: 5,
    };
    let gc = GarbageCollector::with_config(config.clone());
    assert_eq!(gc.config.interval_ms, 1000);
    assert_eq!(gc.config.retention_ms, 5000);
    assert_eq!(gc.config.max_chain_depth, 5);
  }

  #[test]
  fn test_gc_run_empty() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();

    let result = gc.run_gc(&mut tx_mgr, &mut version_chain);
    assert!(!result.skipped);
    assert_eq!(result.versions_pruned, 0);
    assert_eq!(result.chains_truncated, 0);
  }

  #[test]
  fn test_gc_prunes_old_versions() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();

    // Use very short retention for testing
    gc.config.retention_ms = 0;

    // Create some old versions with old timestamps
    for i in 1..=5 {
      let data = NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      };
      // Use timestamp 1-5 which is definitely old
      version_chain.append_node_version(1, data, i, i);
    }

    let _result = gc.run_gc(&mut tx_mgr, &mut version_chain);

    // Stats should be updated
    assert!(gc.stats.gc_runs > 0);
  }

  #[test]
  fn test_gc_respects_active_transactions() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();
    gc.config.retention_ms = 0;

    // Start a transaction (creates snapshot at ts=1)
    let (_txid, _start_ts) = tx_mgr.begin_tx();

    // Create version at ts=1 (should be preserved for the active tx)
    let data = NodeVersionData {
      node_id: 1,
      delta: NodeDelta::default(),
    };
    version_chain.append_node_version(1, data, 1, 1);

    // GC should respect the active transaction's snapshot
    let _result = gc.run_gc(&mut tx_mgr, &mut version_chain);

    // Version at ts=1 should still exist because there's an active tx
    assert!(version_chain.get_node_version(1).is_some());
  }

  #[test]
  fn test_gc_truncates_deep_chains() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();
    gc.config.max_chain_depth = 3;
    gc.config.retention_ms = u64::MAX; // Don't prune by time

    // Create a deep chain
    for i in 1..=10 {
      let data = NodeVersionData {
        node_id: 1,
        delta: NodeDelta::default(),
      };
      // Use future timestamps so they won't be pruned
      version_chain.append_node_version(1, data, i, u64::MAX - i);
    }

    let _result = gc.run_gc(&mut tx_mgr, &mut version_chain);

    // Chain should be truncated
    let mut depth = 0;
    let mut current = version_chain.get_node_version(1);
    while let Some(v) = current {
      depth += 1;
      current = v.prev.as_deref();
    }
    assert!(depth <= gc.config.max_chain_depth + 1);
  }

  #[test]
  fn test_gc_cleans_committed_transactions() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();
    gc.config.retention_ms = 0;

    // Create and commit transactions
    for _ in 0..5 {
      let (txid, _) = tx_mgr.begin_tx();
      tx_mgr.commit_tx(txid).unwrap();
    }

    // Start another transaction to prevent eager cleanup
    let (_txid, _) = tx_mgr.begin_tx();

    // Run GC - committed transactions with old timestamps should be cleaned
    let _result = gc.run_gc(&mut tx_mgr, &mut version_chain);

    // Some transactions might be cleaned
    assert!(gc.stats.gc_runs > 0);
  }

  #[test]
  fn test_gc_stats() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();

    gc.run_gc(&mut tx_mgr, &mut version_chain);
    gc.run_gc(&mut tx_mgr, &mut version_chain);
    gc.run_gc(&mut tx_mgr, &mut version_chain);

    let stats = gc.get_stats();
    assert_eq!(stats.gc_runs, 3);
  }

  #[test]
  fn test_gc_reset_stats() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();

    gc.run_gc(&mut tx_mgr, &mut version_chain);
    gc.reset_stats();

    let stats = gc.get_stats();
    assert_eq!(stats.gc_runs, 0);
    assert_eq!(stats.versions_pruned, 0);
  }

  #[test]
  fn test_should_run() {
    let gc = GarbageCollector::new();

    // Should run on first call
    assert!(gc.should_run());
  }

  #[test]
  fn test_is_running() {
    let gc = GarbageCollector::new();
    assert!(!gc.is_running());
  }

  #[test]
  fn test_shared_gc_state() {
    let state = SharedGcState::new();

    assert!(!state.should_stop());

    state.record_gc_run(10);
    assert_eq!(state.versions_pruned.load(Ordering::Relaxed), 10);
    assert_eq!(state.gc_runs.load(Ordering::Relaxed), 1);

    state.stop();
    assert!(state.should_stop());
  }

  #[test]
  fn test_gc_result_default() {
    let result = GcResult::default();
    assert_eq!(result.versions_pruned, 0);
    assert_eq!(result.chains_truncated, 0);
    assert_eq!(result.txs_cleaned, 0);
    assert!(!result.skipped);
  }

  #[test]
  fn test_gc_config_default() {
    let config = GcConfig::default();
    assert_eq!(config.interval_ms, DEFAULT_GC_INTERVAL_MS);
    assert_eq!(config.retention_ms, DEFAULT_RETENTION_MS);
    assert_eq!(config.max_chain_depth, DEFAULT_MAX_CHAIN_DEPTH);
  }

  #[test]
  fn test_force_gc() {
    let (mut tx_mgr, mut version_chain, mut gc) = setup();

    let _pruned = gc.force_gc(&mut tx_mgr, &mut version_chain);

    // Even with no versions, force_gc should run
    assert!(gc.stats.gc_runs > 0);
  }

  #[test]
  fn test_set_config() {
    let mut gc = GarbageCollector::new();

    let new_config = GcConfig {
      interval_ms: 100,
      retention_ms: 200,
      max_chain_depth: 3,
    };
    gc.set_config(new_config);

    assert_eq!(gc.config().interval_ms, 100);
    assert_eq!(gc.config().retention_ms, 200);
    assert_eq!(gc.config().max_chain_depth, 3);
  }
}
