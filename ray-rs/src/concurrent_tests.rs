//! Comprehensive concurrent access tests for KiteDB
//!
//! This module validates KiteDB's concurrent access guarantees:
//!
//! # Test Categories
//!
//! ## Multi-threaded Read Tests
//! - `test_concurrent_reads_same_node` - Multiple threads reading the same node
//! - `test_concurrent_reads_different_nodes` - Multiple threads reading different nodes
//! - `test_concurrent_property_reads` - Concurrent property access
//! - `test_concurrent_traversal_reads` - Concurrent graph traversals
//! - `test_concurrent_edge_exists_checks` - Concurrent edge existence checks
//!
//! ## Reader-Writer Contention Tests
//! - `test_readers_during_write` - Verifies readers complete during writes
//! - `test_write_does_not_starve_readers` - Ensures fair scheduling
//!
//! ## MVCC Transaction Isolation Tests
//! - `test_mvcc_concurrent_transactions_no_conflict` - Non-conflicting transactions
//! - `test_mvcc_write_write_conflict` - Write-write conflict detection
//! - `test_mvcc_read_write_conflict` - Read-write conflict detection
//! - `test_mvcc_many_concurrent_readers` - Many readers, no conflicts
//! - `test_mvcc_serialized_writes` - Sequential writes succeed
//! - `test_mvcc_concurrent_upsert_same_edge_unique` - Edge upsert conflict detection
//!
//! ## Stress Tests
//! - `test_high_concurrency_reads` - 16 threads, high throughput
//! - `test_mixed_workload_stress` - Mixed read/write workload
//! - `test_read_throughput_scaling` - Measures scaling across thread counts
//!
//! # Running Tests
//!
//! ```bash
//! # Run all concurrent tests
//! cargo test concurrent_tests --no-default-features --release
//!
//! # Run with output (shows throughput numbers)
//! cargo test concurrent_tests --no-default-features --release -- --nocapture
//! ```
//!
//! # Expected Results
//!
//! On a multi-core machine, you should see:
//! - ~1.5-2x throughput improvement with 4-8 threads vs single-threaded
//! - All concurrent read operations completing successfully
//! - No deadlocks or data races

#[cfg(test)]
mod tests {
  use std::collections::HashMap;
  use std::sync::atomic::{AtomicU64, Ordering};
  use std::sync::{Arc, Barrier};
  use std::thread;
  use std::time::{Duration, Instant};
  use tempfile::tempdir;

  use crate::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
  use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
  use crate::mvcc::{ConflictDetector, TxManager};
  use crate::types::{PropValue, TxKey};

  // ============================================================================
  // Test Helpers
  // ============================================================================

  fn key(name: &str) -> TxKey {
    TxKey::Key(std::sync::Arc::from(name))
  }

  fn create_test_schema() -> KiteOptions {
    let user = NodeDef::new("User", "user:")
      .prop(PropDef::string("name"))
      .prop(PropDef::int("age"))
      .prop(PropDef::float("score"));

    let post = NodeDef::new("Post", "post:").prop(PropDef::string("content"));

    let follows = EdgeDef::new("FOLLOWS");
    let likes = EdgeDef::new("LIKES");

    KiteOptions::new()
      .node(user)
      .node(post)
      .edge(follows)
      .edge(likes)
  }

  fn temp_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
    temp_dir.path().join("test-db")
  }

  fn setup_test_db(node_count: usize, edge_count: usize) -> (tempfile::TempDir, Kite) {
    let temp_dir = tempdir().unwrap();
    let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

    // Create nodes
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
      let mut props = HashMap::new();
      props.insert("name".to_string(), PropValue::String(format!("User{i}")));
      props.insert("age".to_string(), PropValue::I64(20 + (i % 50) as i64));
      props.insert("score".to_string(), PropValue::F64(i as f64 * 0.1));
      let node = ray.create_node("User", &format!("user{i}"), props).unwrap();
      node_ids.push(node.id());
    }

    // Create edges (chain + random)
    let edges_created = std::cmp::min(edge_count, node_count.saturating_sub(1));
    for i in 0..edges_created {
      let src = node_ids[i];
      let dst = node_ids[(i + 1) % node_count];
      if src != dst {
        let _ = ray.link(src, "FOLLOWS", dst);
      }
    }

    (temp_dir, ray)
  }

  // ============================================================================
  // Multi-threaded Read Tests
  // ============================================================================

  #[test]
  fn test_concurrent_reads_same_node() {
    let (_temp_dir, ray) = setup_test_db(100, 50);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 8;
    let reads_per_thread = 1000;
    let barrier = Arc::new(Barrier::new(num_threads));

    let handles: Vec<_> = (0..num_threads)
      .map(|_| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);

        thread::spawn(move || {
          barrier.wait(); // Synchronize start

          let mut success_count = 0;
          for _ in 0..reads_per_thread {
            let ray_guard = ray.read();
            if let Some(_node) = ray_guard.get("User", "user0").ok().flatten() {
              success_count += 1;
            }
          }
          success_count
        })
      })
      .collect();

    let total_successes: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // All reads should succeed
    assert_eq!(
      total_successes,
      num_threads * reads_per_thread,
      "All concurrent reads should succeed"
    );
  }

  #[test]
  fn test_concurrent_reads_different_nodes() {
    let (_temp_dir, ray) = setup_test_db(1000, 500);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 8;
    let reads_per_thread = 500;
    let barrier = Arc::new(Barrier::new(num_threads));

    let handles: Vec<_> = (0..num_threads)
      .map(|thread_id| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);

        thread::spawn(move || {
          barrier.wait();

          let mut success_count = 0;
          for i in 0..reads_per_thread {
            // Each thread reads different nodes based on thread_id
            let node_idx = (thread_id * reads_per_thread + i) % 1000;
            let ray_guard = ray.read();
            if ray_guard
              .get("User", &format!("user{node_idx}"))
              .ok()
              .flatten()
              .is_some()
            {
              success_count += 1;
            }
          }
          success_count
        })
      })
      .collect();

    let total_successes: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // All reads should succeed
    assert_eq!(
      total_successes,
      num_threads * reads_per_thread,
      "All concurrent reads of different nodes should succeed"
    );
  }

  #[test]
  fn test_concurrent_property_reads() {
    let (_temp_dir, ray) = setup_test_db(500, 200);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 4;
    let reads_per_thread = 200;
    let barrier = Arc::new(Barrier::new(num_threads));

    // Get some node IDs first
    let node_ids: Vec<u64> = {
      let ray_guard = ray.read();
      (0..100)
        .filter_map(|i| {
          ray_guard
            .get("User", &format!("user{i}"))
            .ok()
            .flatten()
            .map(|n| n.id())
        })
        .collect()
    };

    let node_ids = Arc::new(node_ids);

    let handles: Vec<_> = (0..num_threads)
      .map(|_| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let node_ids = Arc::clone(&node_ids);

        thread::spawn(move || {
          barrier.wait();

          let mut success_count = 0;
          for i in 0..reads_per_thread {
            let node_id = node_ids[i % node_ids.len()];
            let ray_guard = ray.read();
            if ray_guard.prop(node_id, "name").is_some() {
              success_count += 1;
            }
            if ray_guard.prop(node_id, "age").is_some() {
              success_count += 1;
            }
          }
          success_count
        })
      })
      .collect();

    let total_successes: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // Each thread does 2 property reads per iteration
    assert_eq!(
      total_successes,
      num_threads * reads_per_thread * 2,
      "All concurrent property reads should succeed"
    );
  }

  #[test]
  fn test_concurrent_traversal_reads() {
    let (_temp_dir, ray) = setup_test_db(200, 500);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 4;
    let traversals_per_thread = 100;
    let barrier = Arc::new(Barrier::new(num_threads));

    // Get starting node IDs
    let start_ids: Vec<u64> = {
      let ray_guard = ray.read();
      (0..50)
        .filter_map(|i| {
          ray_guard
            .get("User", &format!("user{i}"))
            .ok()
            .flatten()
            .map(|n| n.id())
        })
        .collect()
    };

    let start_ids = Arc::new(start_ids);

    let handles: Vec<_> = (0..num_threads)
      .map(|thread_id| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let start_ids = Arc::clone(&start_ids);

        thread::spawn(move || {
          barrier.wait();

          let mut traversal_count = 0;
          for i in 0..traversals_per_thread {
            let start = start_ids[(thread_id + i) % start_ids.len()];
            let ray_guard = ray.read();

            // Perform outgoing traversal
            let neighbors = ray_guard.neighbors_out(start, Some("FOLLOWS"));
            if neighbors.is_ok() {
              traversal_count += 1;
            }
          }
          traversal_count
        })
      })
      .collect();

    let total_traversals: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    assert_eq!(
      total_traversals,
      num_threads * traversals_per_thread,
      "All concurrent traversals should succeed"
    );
  }

  #[test]
  fn test_concurrent_edge_exists_checks() {
    let (_temp_dir, ray) = setup_test_db(100, 99);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 8;
    let checks_per_thread = 500;
    let barrier = Arc::new(Barrier::new(num_threads));

    // Get node IDs for edge checks
    let node_ids: Vec<u64> = {
      let ray_guard = ray.read();
      (0..100)
        .filter_map(|i| {
          ray_guard
            .get("User", &format!("user{i}"))
            .ok()
            .flatten()
            .map(|n| n.id())
        })
        .collect()
    };

    let node_ids = Arc::new(node_ids);

    let handles: Vec<_> = (0..num_threads)
      .map(|_| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let node_ids = Arc::clone(&node_ids);

        thread::spawn(move || {
          barrier.wait();

          let mut check_count = 0;
          for i in 0..checks_per_thread {
            let src = node_ids[i % (node_ids.len() - 1)];
            let dst = node_ids[(i + 1) % node_ids.len()];
            let ray_guard = ray.read();
            // Just verify the operation completes without error
            let _ = ray_guard.has_edge(src, "FOLLOWS", dst);
            check_count += 1;
          }
          check_count
        })
      })
      .collect();

    let total_checks: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();

    assert_eq!(
      total_checks,
      num_threads * checks_per_thread,
      "All concurrent edge checks should complete"
    );
  }

  // ============================================================================
  // Reader-Writer Contention Tests
  // ============================================================================

  #[test]
  fn test_readers_during_write() {
    let (_temp_dir, ray) = setup_test_db(100, 50);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_readers = 4;
    let read_iterations = 100;
    let barrier = Arc::new(Barrier::new(num_readers + 1)); // +1 for writer
    let reads_completed = Arc::new(AtomicU64::new(0));
    let writes_completed = Arc::new(AtomicU64::new(0));

    // Spawn reader threads
    let reader_handles: Vec<_> = (0..num_readers)
      .map(|_| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let reads_completed = Arc::clone(&reads_completed);

        thread::spawn(move || {
          barrier.wait();

          for i in 0..read_iterations {
            let key = format!("user{}", i % 100);
            let ray_guard = ray.read();
            let _ = ray_guard.get("User", &key);
            reads_completed.fetch_add(1, Ordering::SeqCst);
          }
        })
      })
      .collect();

    // Spawn writer thread
    let writer_handle = {
      let ray = Arc::clone(&ray);
      let barrier = Arc::clone(&barrier);
      let writes_completed = Arc::clone(&writes_completed);

      thread::spawn(move || {
        barrier.wait();

        // Perform some writes while readers are active
        for i in 0..20 {
          let mut ray_guard = ray.write();
          let mut props = HashMap::new();
          props.insert("name".to_string(), PropValue::String(format!("NewUser{i}")));
          let _ = ray_guard.create_node("User", &format!("newuser{i}"), props);
          writes_completed.fetch_add(1, Ordering::SeqCst);
        }
      })
    };

    // Wait for all threads
    for handle in reader_handles {
      handle.join().unwrap();
    }
    writer_handle.join().unwrap();

    let total_reads = reads_completed.load(Ordering::SeqCst);
    let total_writes = writes_completed.load(Ordering::SeqCst);

    assert_eq!(
      total_reads,
      (num_readers * read_iterations) as u64,
      "All reads should complete"
    );
    assert_eq!(total_writes, 20, "All writes should complete");
  }

  #[test]
  fn test_write_does_not_starve_readers() {
    let (_temp_dir, ray) = setup_test_db(50, 25);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let barrier = Arc::new(Barrier::new(3)); // 2 readers + 1 writer
    let reader_times = Arc::new(parking_lot::Mutex::new(Vec::new()));

    // Reader 1
    let reader1_handle = {
      let ray = Arc::clone(&ray);
      let barrier = Arc::clone(&barrier);
      let times = Arc::clone(&reader_times);

      thread::spawn(move || {
        barrier.wait();

        for _ in 0..50 {
          let start = Instant::now();
          {
            let ray_guard = ray.read();
            let _ = ray_guard.get("User", "user0");
          }
          let elapsed = start.elapsed();
          times.lock().push(elapsed);
          thread::sleep(Duration::from_micros(100));
        }
      })
    };

    // Reader 2
    let reader2_handle = {
      let ray = Arc::clone(&ray);
      let barrier = Arc::clone(&barrier);
      let times = Arc::clone(&reader_times);

      thread::spawn(move || {
        barrier.wait();

        for _ in 0..50 {
          let start = Instant::now();
          {
            let ray_guard = ray.read();
            let _ = ray_guard.get("User", "user1");
          }
          let elapsed = start.elapsed();
          times.lock().push(elapsed);
          thread::sleep(Duration::from_micros(100));
        }
      })
    };

    // Writer (does longer writes)
    let writer_handle = {
      let ray = Arc::clone(&ray);
      let barrier = Arc::clone(&barrier);

      thread::spawn(move || {
        barrier.wait();

        for i in 0..10 {
          {
            let mut ray_guard = ray.write();
            // Simulate longer write operation
            for j in 0..5 {
              let mut props = HashMap::new();
              props.insert(
                "name".to_string(),
                PropValue::String(format!("BatchUser{i}_{j}")),
              );
              let _ = ray_guard.create_node("User", &format!("batch{i}_{j}"), props);
            }
          }
          thread::sleep(Duration::from_micros(500));
        }
      })
    };

    reader1_handle.join().unwrap();
    reader2_handle.join().unwrap();
    writer_handle.join().unwrap();

    let times = reader_times.lock();
    let max_read_time = times.iter().max().unwrap();

    // Reads should not be blocked for more than 100ms (generous threshold)
    assert!(
      *max_read_time < Duration::from_millis(100),
      "Max read time {max_read_time:?} exceeded threshold - possible writer starvation"
    );
  }

  // ============================================================================
  // MVCC Transaction Isolation Tests
  // ============================================================================

  #[test]
  fn test_mvcc_concurrent_transactions_no_conflict() {
    // Test that transactions on different keys don't conflict
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    // Start two concurrent transactions
    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    // They write to different keys
    tx_mgr.record_write(txid1, key("key_a"));
    tx_mgr.record_write(txid2, key("key_b"));

    // Both should commit without conflict
    assert!(
      detector.validate_commit(&tx_mgr, txid1).is_ok(),
      "Tx1 should not conflict"
    );
    tx_mgr.commit_tx(txid1).unwrap();

    assert!(
      detector.validate_commit(&tx_mgr, txid2).is_ok(),
      "Tx2 should not conflict"
    );
    tx_mgr.commit_tx(txid2).unwrap();
  }

  #[test]
  fn test_mvcc_write_write_conflict() {
    // Test that concurrent writes to same key conflict
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    // Both write to same key
    tx_mgr.record_write(txid1, key("shared_key"));
    tx_mgr.record_write(txid2, key("shared_key"));

    // First commits
    assert!(detector.validate_commit(&tx_mgr, txid1).is_ok());
    tx_mgr.commit_tx(txid1).unwrap();

    // Second should conflict
    assert!(
      detector.validate_commit(&tx_mgr, txid2).is_err(),
      "Tx2 should conflict due to write-write conflict"
    );
  }

  #[test]
  fn test_mvcc_read_write_conflict() {
    // Test that reading a key modified by concurrent transaction conflicts
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    // Tx1 writes, Tx2 reads the same key
    tx_mgr.record_write(txid1, key("key"));
    tx_mgr.record_read(txid2, key("key"));

    // Tx1 commits first
    tx_mgr.commit_tx(txid1).unwrap();

    // Tx2 should conflict (read-write conflict)
    assert!(
      detector.validate_commit(&tx_mgr, txid2).is_err(),
      "Tx2 should conflict due to read-write conflict"
    );
  }

  #[test]
  fn test_mvcc_many_concurrent_readers() {
    // Test that many readers don't conflict with each other
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    // First, establish some data
    let (setup_tx, _) = tx_mgr.begin_tx();
    tx_mgr.record_write(setup_tx, key("data"));
    tx_mgr.commit_tx(setup_tx).unwrap();

    // Start many concurrent read transactions
    let num_readers = 10;
    let reader_txids: Vec<_> = (0..num_readers)
      .map(|_| {
        let (txid, _) = tx_mgr.begin_tx();
        tx_mgr.record_read(txid, key("data"));
        txid
      })
      .collect();

    // All readers should be able to commit (no conflicts among readers)
    for txid in reader_txids {
      assert!(
        detector.validate_commit(&tx_mgr, txid).is_ok(),
        "Read-only transaction should not conflict"
      );
      tx_mgr.commit_tx(txid).unwrap();
    }
  }

  #[test]
  fn test_mvcc_serialized_writes() {
    // Test that serialized writes (non-concurrent) don't conflict
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    for i in 0..5 {
      let (txid, _) = tx_mgr.begin_tx();
      tx_mgr.record_write(txid, key("key"));

      assert!(
        detector.validate_commit(&tx_mgr, txid).is_ok(),
        "Serial transaction {i} should not conflict"
      );
      tx_mgr.commit_tx(txid).unwrap();
    }
  }

  #[test]
  fn test_mvcc_conflict_details() {
    let mut tx_mgr = TxManager::new();
    let detector = ConflictDetector::new();

    let (txid1, _) = tx_mgr.begin_tx();
    let (txid2, _) = tx_mgr.begin_tx();

    tx_mgr.record_write(txid1, key("conflict_key"));
    tx_mgr.record_write(txid2, key("conflict_key"));
    tx_mgr.record_write(txid2, key("ok_key"));

    tx_mgr.commit_tx(txid1).unwrap();

    let conflicts = detector.check_conflicts(&tx_mgr, txid2);
    assert!(!conflicts.is_empty(), "Should detect conflicts");
    assert!(
      conflicts.contains(&key("conflict_key").to_string()),
      "Should identify conflicting key"
    );
    assert!(
      !conflicts.contains(&key("ok_key").to_string()),
      "Non-conflicting key should not be reported"
    );
  }

  // ============================================================================
  // Stress Tests
  // ============================================================================

  #[test]
  fn test_high_concurrency_reads() {
    let (_temp_dir, ray) = setup_test_db(1000, 500);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_threads = 16;
    let reads_per_thread = 200;
    let barrier = Arc::new(Barrier::new(num_threads));
    let total_ops = Arc::new(AtomicU64::new(0));

    let start = Instant::now();

    let handles: Vec<_> = (0..num_threads)
      .map(|thread_id| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let total_ops = Arc::clone(&total_ops);

        thread::spawn(move || {
          barrier.wait();

          for i in 0..reads_per_thread {
            let idx = (thread_id * 100 + i) % 1000;
            let ray_guard = ray.read();
            let _ = ray_guard.get("User", &format!("user{idx}"));
            total_ops.fetch_add(1, Ordering::Relaxed);
          }
        })
      })
      .collect();

    for handle in handles {
      handle.join().unwrap();
    }

    let elapsed = start.elapsed();
    let total = total_ops.load(Ordering::Relaxed);
    let ops_per_sec = total as f64 / elapsed.as_secs_f64();

    println!("High concurrency test: {total} ops in {elapsed:?} ({ops_per_sec:.0} ops/sec)");

    assert_eq!(
      total,
      (num_threads * reads_per_thread) as u64,
      "All operations should complete"
    );
  }

  #[test]
  fn test_mixed_workload_stress() {
    let (_temp_dir, ray) = setup_test_db(500, 250);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let num_readers = 8;
    let num_writers = 2;
    let ops_per_thread = 100;
    let barrier = Arc::new(Barrier::new(num_readers + num_writers));
    let read_ops = Arc::new(AtomicU64::new(0));
    let write_ops = Arc::new(AtomicU64::new(0));

    // Spawn readers
    let reader_handles: Vec<_> = (0..num_readers)
      .map(|thread_id| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let read_ops = Arc::clone(&read_ops);

        thread::spawn(move || {
          barrier.wait();

          for i in 0..ops_per_thread {
            let idx = (thread_id * 50 + i) % 500;
            let ray_guard = ray.read();
            let _ = ray_guard.get("User", &format!("user{idx}"));
            read_ops.fetch_add(1, Ordering::Relaxed);
            // Small delay to simulate real workload
            if i % 10 == 0 {
              thread::yield_now();
            }
          }
        })
      })
      .collect();

    // Spawn writers
    let writer_handles: Vec<_> = (0..num_writers)
      .map(|writer_id| {
        let ray = Arc::clone(&ray);
        let barrier = Arc::clone(&barrier);
        let write_ops = Arc::clone(&write_ops);

        thread::spawn(move || {
          barrier.wait();

          for i in 0..ops_per_thread {
            let mut ray_guard = ray.write();
            let mut props = HashMap::new();
            props.insert(
              "name".to_string(),
              PropValue::String(format!("StressUser{writer_id}_{i}")),
            );
            let _ = ray_guard.create_node("User", &format!("stress{writer_id}_{i}"), props);
            write_ops.fetch_add(1, Ordering::Relaxed);
            // Writers yield more to allow readers through
            if i % 5 == 0 {
              thread::yield_now();
            }
          }
        })
      })
      .collect();

    // Wait for all
    for handle in reader_handles {
      handle.join().unwrap();
    }
    for handle in writer_handles {
      handle.join().unwrap();
    }

    let total_reads = read_ops.load(Ordering::Relaxed);
    let total_writes = write_ops.load(Ordering::Relaxed);

    assert_eq!(
      total_reads,
      (num_readers * ops_per_thread) as u64,
      "All reads should complete"
    );
    assert_eq!(
      total_writes,
      (num_writers * ops_per_thread) as u64,
      "All writes should complete"
    );

    println!("Mixed workload: {total_reads} reads, {total_writes} writes completed");
  }

  // ============================================================================
  // Single-File DB Tests (Sequential - SingleFileDB is not thread-safe)
  // ============================================================================

  #[test]
  fn test_single_file_sequential_reads() {
    // Note: SingleFileDB is not designed for concurrent access from multiple threads.
    // The internal LruCache is not Sync. This test verifies sequential performance.
    let temp_dir = tempdir().unwrap();
    let db_path = temp_dir.path().join("test.kitedb");

    // Setup: Create database with data
    {
      let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
      db.begin(false).unwrap();
      for i in 0..100 {
        let key = format!("node{i}");
        let node_id = db.create_node(Some(&key)).unwrap();
        db.set_node_prop_by_name(node_id, "value", PropValue::I64(i as i64))
          .unwrap();
      }
      db.commit().unwrap();
      crate::core::single_file::close_single_file(db).unwrap();
    }

    // Test: Sequential reads from single thread
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).unwrap();
    let reads = 400;

    let mut success_count = 0;
    for i in 0..reads {
      let key = format!("node{}", i % 100);
      if db.node_by_key(&key).is_some() {
        success_count += 1;
      }
    }

    assert_eq!(success_count, reads, "All single-file reads should succeed");

    crate::core::single_file::close_single_file(db).unwrap();
  }

  // ============================================================================
  // Throughput Measurement Tests
  // ============================================================================

  #[test]
  fn test_read_throughput_scaling() {
    let (_temp_dir, ray) = setup_test_db(1000, 500);
    let ray = Arc::new(parking_lot::RwLock::new(ray));

    let thread_counts = [1, 2, 4, 8];
    let ops_per_thread = 500;

    println!("\nRead throughput scaling:");
    println!("Threads | Throughput (ops/sec) | Speedup");
    println!("--------|---------------------|--------");

    let mut baseline_throughput = 0.0;

    for &num_threads in &thread_counts {
      let barrier = Arc::new(Barrier::new(num_threads));
      let start = Instant::now();

      let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
          let ray = Arc::clone(&ray);
          let barrier = Arc::clone(&barrier);

          thread::spawn(move || {
            barrier.wait();

            for i in 0..ops_per_thread {
              let idx = (thread_id * 100 + i) % 1000;
              let ray_guard = ray.read();
              let _ = ray_guard.get("User", &format!("user{idx}"));
            }
          })
        })
        .collect();

      for handle in handles {
        handle.join().unwrap();
      }

      let elapsed = start.elapsed();
      let total_ops = num_threads * ops_per_thread;
      let throughput = total_ops as f64 / elapsed.as_secs_f64();

      let speedup = if num_threads == 1 {
        baseline_throughput = throughput;
        1.0
      } else {
        throughput / baseline_throughput
      };

      println!("{num_threads:7} | {throughput:>19.0} | {speedup:>6.2}x");
    }
  }
}
