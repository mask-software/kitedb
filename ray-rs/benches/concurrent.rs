//! Concurrent access benchmarks for KiteDB
//!
//! Run with: cargo bench --bench concurrent
//!
//! These benchmarks measure:
//! - Multi-threaded read throughput
//! - Read scaling across thread counts
//! - Reader-writer contention impact
//! - MVCC transaction overhead

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
#[cfg(feature = "bench-profile")]
use std::time::Instant;
use tempfile::tempdir;

// Perf profiling (optional):
//   cargo bench --bench concurrent --features bench-profile
//   KITEDB_BENCH_PROFILE=1 cargo bench --bench concurrent --features bench-profile
//   perf record --call-graph=dwarf target/release/deps/concurrent-*

extern crate kitedb;
use kitedb::api::kite::{EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use kitedb::error::KiteError;
use kitedb::mvcc::TxManager;
use kitedb::types::{PropValue, TxKey};

// ============================================================================
// Setup Helpers
// ============================================================================

fn create_test_schema_with_group_commit(group_commit: bool) -> KiteOptions {
  let user = NodeDef::new("User", "user:")
    .prop(PropDef::string("name"))
    .prop(PropDef::int("age"))
    .prop(PropDef::float("score"));

  let follows = EdgeDef::new("FOLLOWS");

  KiteOptions::new()
    .node(user)
    .edge(follows)
    .sync_mode(SyncMode::Normal)
    .group_commit_enabled(group_commit)
    .group_commit_window_ms(2)
}

fn temp_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
  temp_dir.path().join("bench")
}

fn setup_ray_db(
  node_count: usize,
  edge_count: usize,
  group_commit: bool,
) -> (tempfile::TempDir, Kite) {
  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(
    temp_db_path(&temp_dir),
    create_test_schema_with_group_commit(group_commit),
  )
  .unwrap();

  let mut node_ids = Vec::with_capacity(node_count);
  for i in 0..node_count {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String(format!("User{i}")));
    props.insert("age".to_string(), PropValue::I64(20 + (i % 50) as i64));
    props.insert("score".to_string(), PropValue::F64(i as f64 * 0.1));
    let node = ray.create_node("User", &format!("user{i}"), props).unwrap();
    node_ids.push(node.id());

    // Optimize periodically to avoid WAL overflow
    if (i + 1) % 1000 == 0 {
      ray.optimize().unwrap();
    }
  }

  let edges_to_create = std::cmp::min(edge_count, node_count.saturating_sub(1));
  for i in 0..edges_to_create {
    let src = node_ids[i];
    let dst = node_ids[(i + 1) % node_count];
    if src != dst {
      let _ = ray.link(src, "FOLLOWS", dst);
    }
    // Optimize periodically to avoid WAL overflow
    if (i + 1) % 1000 == 0 {
      ray.optimize().unwrap();
    }
  }

  ray.optimize().unwrap();
  (temp_dir, ray)
}

const READ_BATCH_SIZE: usize = 100;
const WRITE_BATCH_SIZE: usize = 100;

#[derive(Clone, Copy, Debug)]
enum LockMode {
  PerOp,
  Batched,
}

#[cfg(feature = "bench-profile")]
#[derive(Default)]
struct BenchProfile {
  read_lock_wait_ns: AtomicU64,
  write_lock_wait_ns: AtomicU64,
}

#[cfg(feature = "bench-profile")]
impl BenchProfile {
  fn record_read_wait(&self, elapsed: std::time::Duration) {
    self
      .read_lock_wait_ns
      .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
  }

  fn record_write_wait(&self, elapsed: std::time::Duration) {
    self
      .write_lock_wait_ns
      .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
  }

  fn take_snapshot(&self) -> (u64, u64) {
    (
      self.read_lock_wait_ns.swap(0, Ordering::Relaxed),
      self.write_lock_wait_ns.swap(0, Ordering::Relaxed),
    )
  }
}

#[cfg(feature = "bench-profile")]
fn bench_profile_enabled() -> bool {
  std::env::var("KITEDB_BENCH_PROFILE")
    .map(|value| {
      let value = value.to_lowercase();
      value == "1" || value == "true" || value == "yes"
    })
    .unwrap_or(false)
}

// ============================================================================
// Multi-threaded Read Throughput Benchmarks
// ============================================================================

fn bench_concurrent_reads(c: &mut Criterion) {
  let mut group = c.benchmark_group("concurrent_reads");
  group.sample_size(20);

  // Test with different database sizes
  for node_count in [1000, 5000].iter() {
    let (_temp_dir, ray) = setup_ray_db(*node_count, node_count / 2, false);
    let ray = Arc::new(parking_lot::RwLock::new(ray));

    // Test with different thread counts
    for num_threads in [1, 2, 4, 8].iter() {
      let ops_per_thread = 500;
      group.throughput(Throughput::Elements((*num_threads * ops_per_thread) as u64));

      group.bench_with_input(
        BenchmarkId::new(format!("nodes_{node_count}"), num_threads),
        num_threads,
        |bencher, &num_threads| {
          bencher.iter(|| {
            let barrier = Arc::new(Barrier::new(num_threads));
            let ray = Arc::clone(&ray);

            let handles: Vec<_> = (0..num_threads)
              .map(|thread_id| {
                let ray = Arc::clone(&ray);
                let barrier = Arc::clone(&barrier);
                let node_count = *node_count;

                thread::spawn(move || {
                  barrier.wait();
                  for i in 0..ops_per_thread {
                    let idx = (thread_id * 100 + i) % node_count;
                    let ray_guard = ray.read();
                    let _ = black_box(ray_guard.get("User", &format!("user{idx}")));
                  }
                })
              })
              .collect();

            for handle in handles {
              handle.join().unwrap();
            }
          });
        },
      );
    }
  }

  group.finish();
}

// ============================================================================
// Read Scaling Benchmarks
// ============================================================================

fn bench_read_scaling(c: &mut Criterion) {
  let mut group = c.benchmark_group("read_scaling");
  group.sample_size(15);

  let (_temp_dir, ray) = setup_ray_db(2000, 1000, false);
  let ray = Arc::new(parking_lot::RwLock::new(ray));

  let ops_per_thread = 1000;

  for num_threads in [1, 2, 4, 8, 16].iter() {
    group.throughput(Throughput::Elements((*num_threads * ops_per_thread) as u64));

    group.bench_with_input(
      BenchmarkId::new("threads", num_threads),
      num_threads,
      |bencher, &num_threads| {
        bencher.iter(|| {
          let barrier = Arc::new(Barrier::new(num_threads));
          let ray = Arc::clone(&ray);

          let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
              let ray = Arc::clone(&ray);
              let barrier = Arc::clone(&barrier);

              thread::spawn(move || {
                barrier.wait();
                for i in 0..ops_per_thread {
                  let idx = (thread_id * 123 + i * 7) % 2000; // Spread reads
                  let ray_guard = ray.read();
                  let _ = black_box(ray_guard.get("User", &format!("user{idx}")));
                }
              })
            })
            .collect();

          for handle in handles {
            handle.join().unwrap();
          }
        });
      },
    );
  }

  group.finish();
}

// ============================================================================
// Property Read Scaling
// ============================================================================

fn bench_property_read_scaling(c: &mut Criterion) {
  let mut group = c.benchmark_group("property_read_scaling");
  group.sample_size(15);

  let (_temp_dir, ray) = setup_ray_db(1000, 500, false);
  let ray = Arc::new(parking_lot::RwLock::new(ray));

  // Collect node IDs for property reads
  let node_ids: Vec<u64> = {
    let ray_guard = ray.read();
    (0..500)
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

  let ops_per_thread = 500;

  for num_threads in [1, 2, 4, 8].iter() {
    group.throughput(Throughput::Elements((*num_threads * ops_per_thread) as u64));

    group.bench_with_input(
      BenchmarkId::new("threads", num_threads),
      num_threads,
      |bencher, &num_threads| {
        let node_ids = Arc::clone(&node_ids);

        bencher.iter(|| {
          let barrier = Arc::new(Barrier::new(num_threads));
          let ray = Arc::clone(&ray);

          let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
              let ray = Arc::clone(&ray);
              let barrier = Arc::clone(&barrier);
              let node_ids = Arc::clone(&node_ids);

              thread::spawn(move || {
                barrier.wait();
                for i in 0..ops_per_thread {
                  let node_id = node_ids[(thread_id * 50 + i) % node_ids.len()];
                  let ray_guard = ray.read();
                  black_box(ray_guard.prop(node_id, "name"));
                }
              })
            })
            .collect();

          for handle in handles {
            handle.join().unwrap();
          }
        });
      },
    );
  }

  group.finish();
}

// ============================================================================
// Traversal Scaling Benchmarks
// ============================================================================

fn bench_traversal_scaling(c: &mut Criterion) {
  let mut group = c.benchmark_group("traversal_scaling");
  group.sample_size(10);

  let (_temp_dir, ray) = setup_ray_db(500, 1000, false);
  let ray = Arc::new(parking_lot::RwLock::new(ray));

  // Get starting node IDs
  let start_ids: Vec<u64> = {
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
  let start_ids = Arc::new(start_ids);

  let traversals_per_thread = 100;

  for num_threads in [1, 2, 4, 8].iter() {
    group.throughput(Throughput::Elements(
      (*num_threads * traversals_per_thread) as u64,
    ));

    group.bench_with_input(
      BenchmarkId::new("threads", num_threads),
      num_threads,
      |bencher, &num_threads| {
        let start_ids = Arc::clone(&start_ids);

        bencher.iter(|| {
          let barrier = Arc::new(Barrier::new(num_threads));
          let ray = Arc::clone(&ray);

          let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
              let ray = Arc::clone(&ray);
              let barrier = Arc::clone(&barrier);
              let start_ids = Arc::clone(&start_ids);

              thread::spawn(move || {
                barrier.wait();
                for i in 0..traversals_per_thread {
                  let start = start_ids[(thread_id + i) % start_ids.len()];
                  let ray_guard = ray.read();
                  let _ = black_box(ray_guard.neighbors_out(start, Some("FOLLOWS")));
                }
              })
            })
            .collect();

          for handle in handles {
            handle.join().unwrap();
          }
        });
      },
    );
  }

  group.finish();
}

// ============================================================================
// Reader-Writer Contention Benchmarks
// ============================================================================

fn bench_reader_writer_contention_variant(
  c: &mut Criterion,
  group_name: &str,
  lock_mode: LockMode,
  group_commit: bool,
) {
  let mut group = c.benchmark_group(group_name);
  group.sample_size(10);

  #[cfg(feature = "bench-profile")]
  let profile_enabled = bench_profile_enabled();

  // Different ratios of readers to writers
  for (num_readers, num_writers) in [(8, 0), (7, 1), (6, 2), (4, 4)].iter() {
    let (_temp_dir, ray) = setup_ray_db(500, 250, group_commit);
    let ray = Arc::new(parking_lot::RwLock::new(ray));
    let key_counter = Arc::new(AtomicU64::new(0));
    let success_counter = Arc::new(AtomicU64::new(0));

    let initial_nodes = {
      let ray_guard = ray.read();
      ray_guard.count_nodes()
    };
    let initial_edges = {
      let ray_guard = ray.read();
      ray_guard.count_edges_by_type("FOLLOWS").unwrap()
    };

    #[cfg(feature = "bench-profile")]
    let profile = Arc::new(BenchProfile::default());

    let ops_per_thread = 100;
    let total_ops = (num_readers + num_writers) * ops_per_thread;
    group.throughput(Throughput::Elements(total_ops as u64));

    group.bench_with_input(
      BenchmarkId::new(format!("r{num_readers}_w{num_writers}"), ""),
      &(*num_readers, *num_writers),
      |bencher, &(num_readers, num_writers)| {
        let key_counter = Arc::clone(&key_counter);
        let success_counter = Arc::clone(&success_counter);
        let ray = Arc::clone(&ray);
        #[cfg(feature = "bench-profile")]
        let profile = Arc::clone(&profile);

        bencher.iter(|| {
          let barrier = Arc::new(Barrier::new(num_readers + num_writers));
          let ray = Arc::clone(&ray);
          let key_counter = Arc::clone(&key_counter);
          let success_counter = Arc::clone(&success_counter);
          let created_keys = Arc::new(parking_lot::Mutex::new(Vec::new()));

          let counter_start = key_counter.load(Ordering::SeqCst);
          let mut handles = Vec::new();

          // Spawn readers
          for thread_id in 0..num_readers {
            let ray = Arc::clone(&ray);
            let barrier = Arc::clone(&barrier);
            #[cfg(feature = "bench-profile")]
            let profile = Arc::clone(&profile);

            handles.push(thread::spawn(move || {
              barrier.wait();
              match lock_mode {
                LockMode::PerOp => {
                  for i in 0..ops_per_thread {
                    let idx = (thread_id * 50 + i) % 500;
                    #[cfg(feature = "bench-profile")]
                    let lock_start = if profile_enabled {
                      Some(Instant::now())
                    } else {
                      None
                    };
                    let ray_guard = ray.read();
                    #[cfg(feature = "bench-profile")]
                    if let Some(start) = lock_start {
                      profile.record_read_wait(start.elapsed());
                    }
                    let _ = black_box(ray_guard.get("User", &format!("user{idx}")));
                  }
                }
                LockMode::Batched => {
                  let mut i = 0;
                  while i < ops_per_thread {
                    let end = (i + READ_BATCH_SIZE).min(ops_per_thread);
                    #[cfg(feature = "bench-profile")]
                    let lock_start = if profile_enabled {
                      Some(Instant::now())
                    } else {
                      None
                    };
                    let ray_guard = ray.read();
                    #[cfg(feature = "bench-profile")]
                    if let Some(start) = lock_start {
                      profile.record_read_wait(start.elapsed());
                    }
                    for j in i..end {
                      let idx = (thread_id * 50 + j) % 500;
                      let _ = black_box(ray_guard.get("User", &format!("user{idx}")));
                    }
                    i = end;
                  }
                }
              }
            }));
          }

          // Spawn writers
          for writer_id in 0..num_writers {
            let ray = Arc::clone(&ray);
            let barrier = Arc::clone(&barrier);
            let key_counter = Arc::clone(&key_counter);
            let success_counter = Arc::clone(&success_counter);
            let created_keys = Arc::clone(&created_keys);
            #[cfg(feature = "bench-profile")]
            let profile = Arc::clone(&profile);

            handles.push(thread::spawn(move || {
              barrier.wait();
              let mut local_keys = Vec::new();
              match lock_mode {
                LockMode::PerOp => {
                  for i in 0..ops_per_thread {
                    let counter = key_counter.fetch_add(1, Ordering::SeqCst);
                    #[cfg(feature = "bench-profile")]
                    let lock_start = if profile_enabled {
                      Some(Instant::now())
                    } else {
                      None
                    };
                    let mut ray_guard = ray.write();
                    #[cfg(feature = "bench-profile")]
                    if let Some(start) = lock_start {
                      profile.record_write_wait(start.elapsed());
                    }
                    let mut props = HashMap::new();
                    props.insert(
                      "name".to_string(),
                      PropValue::String(format!("BenchUser{writer_id}_{i}")),
                    );
                    let mut attempts = 0;
                    loop {
                      let result =
                        ray_guard.create_node("User", &format!("bench{counter}"), props.clone());
                      match result {
                        Ok(_) => {
                          success_counter.fetch_add(1, Ordering::SeqCst);
                          local_keys.push(counter);
                          break;
                        }
                        Err(KiteError::WalBufferFull) if attempts == 0 => {
                          ray_guard.optimize().unwrap();
                          attempts += 1;
                        }
                        Err(err) => {
                          panic!("writer create_node failed: {err}");
                        }
                      }
                    }
                  }
                }
                LockMode::Batched => {
                  let mut i = 0;
                  while i < ops_per_thread {
                    let end = (i + WRITE_BATCH_SIZE).min(ops_per_thread);
                    let batch_size = end - i;
                    let mut batch_keys = Vec::with_capacity(batch_size);
                    for _ in 0..batch_size {
                      let counter = key_counter.fetch_add(1, Ordering::SeqCst);
                      batch_keys.push(counter);
                    }
                    #[cfg(feature = "bench-profile")]
                    let lock_start = if profile_enabled {
                      Some(Instant::now())
                    } else {
                      None
                    };
                    let mut ray_guard = ray.write();
                    #[cfg(feature = "bench-profile")]
                    if let Some(start) = lock_start {
                      profile.record_write_wait(start.elapsed());
                    }
                    let mut attempts = 0;
                    loop {
                      let result = ray_guard.transaction(|ctx| {
                        for (j, counter) in batch_keys.iter().enumerate() {
                          let mut props = HashMap::new();
                          props.insert(
                            "name".to_string(),
                            PropValue::String(format!("BenchUser{writer_id}_{}", i + j)),
                          );
                          let _ = black_box(ctx.create_node(
                            "User",
                            &format!("bench{counter}"),
                            props,
                          )?);
                        }
                        Ok(())
                      });
                      match result {
                        Ok(_) => {
                          success_counter.fetch_add(batch_size as u64, Ordering::SeqCst);
                          local_keys.extend(batch_keys);
                          break;
                        }
                        Err(KiteError::WalBufferFull) if attempts == 0 => {
                          ray_guard.optimize().unwrap();
                          attempts += 1;
                        }
                        Err(err) => {
                          panic!("writer transaction failed: {err}");
                        }
                      }
                    }
                    i = end;
                  }
                }
              }
              if !local_keys.is_empty() {
                created_keys.lock().extend(local_keys);
              }
            }));
          }

          for handle in handles {
            handle.join().unwrap();
          }

          let counter_end = key_counter.load(Ordering::SeqCst);
          let success_total = success_counter.load(Ordering::SeqCst);
          let ray_guard = ray.read();
          let expected_nodes = initial_nodes + success_total;
          assert_eq!(ray_guard.count_nodes(), expected_nodes);
          let actual_edges = ray_guard.count_edges_by_type("FOLLOWS").unwrap();
          assert_eq!(actual_edges, initial_edges);
          for counter in created_keys.lock().iter().copied() {
            if counter < counter_start || counter >= counter_end {
              continue;
            }
            let node = ray_guard.get("User", &format!("bench{counter}")).unwrap();
            assert!(node.is_some());
          }

          #[cfg(feature = "bench-profile")]
          if profile_enabled {
            let (read_wait_ns, write_wait_ns) = profile.take_snapshot();
            let (commit_wait_ns, wal_flush_ns) = ray_guard.take_profile_snapshot();
            println!(
              "[{group_name}] r{num_readers}_w{num_writers} lock_wait_ns(read/write)={read_wait_ns}/{write_wait_ns} commit_lock_wait_ns={commit_wait_ns} wal_flush_ns={wal_flush_ns}"
            );
          }
        });
      },
    );
  }

  group.finish();
}

fn bench_reader_writer_contention(c: &mut Criterion) {
  bench_reader_writer_contention_variant(c, "reader_writer_contention", LockMode::PerOp, false);
}

fn bench_reader_writer_contention_batched(c: &mut Criterion) {
  bench_reader_writer_contention_variant(
    c,
    "reader_writer_contention_batched",
    LockMode::Batched,
    false,
  );
}

fn bench_reader_writer_contention_batched_group_commit(c: &mut Criterion) {
  bench_reader_writer_contention_variant(
    c,
    "reader_writer_contention_batched_group_commit",
    LockMode::Batched,
    true,
  );
}

// ============================================================================
// MVCC Transaction Overhead Benchmarks
// ============================================================================

fn bench_mvcc_transaction_overhead(c: &mut Criterion) {
  let mut group = c.benchmark_group("mvcc_transaction");
  group.sample_size(50);

  // Benchmark transaction begin/commit cycle
  group.bench_function("begin_commit_cycle", |bencher| {
    bencher.iter_with_setup(TxManager::new, |mut tx_mgr| {
      for i in 0..100 {
        let (txid, _) = tx_mgr.begin_tx();
        tx_mgr.record_read(txid, TxKey::Node(i as u64));
        black_box(tx_mgr.commit_tx(txid).unwrap());
      }
    });
  });

  // Benchmark transaction with writes
  group.bench_function("write_transaction", |bencher| {
    bencher.iter_with_setup(TxManager::new, |mut tx_mgr| {
      for i in 0..100 {
        let (txid, _) = tx_mgr.begin_tx();
        tx_mgr.record_write(txid, TxKey::Node(i as u64));
        black_box(tx_mgr.commit_tx(txid).unwrap());
      }
    });
  });

  // Benchmark read set tracking
  group.bench_function("read_set_tracking", |bencher| {
    bencher.iter_with_setup(TxManager::new, |mut tx_mgr| {
      let (txid, _) = tx_mgr.begin_tx();
      for i in 0..1000 {
        tx_mgr.record_read(txid, TxKey::Node(i as u64));
      }
      black_box(tx_mgr.commit_tx(txid).unwrap());
    });
  });

  // Benchmark write set tracking
  group.bench_function("write_set_tracking", |bencher| {
    bencher.iter_with_setup(TxManager::new, |mut tx_mgr| {
      let (txid, _) = tx_mgr.begin_tx();
      for i in 0..1000 {
        tx_mgr.record_write(txid, TxKey::Node(i as u64));
      }
      black_box(tx_mgr.commit_tx(txid).unwrap());
    });
  });

  group.finish();
}

// ============================================================================
// Single-File Sequential Read Benchmarks (SingleFileDB is not thread-safe)
// ============================================================================

fn bench_single_file_sequential_reads(c: &mut Criterion) {
  // Note: SingleFileDB is not designed for concurrent multi-threaded access.
  // The internal LruCache is not Sync. This benchmark measures sequential performance.
  let mut group = c.benchmark_group("single_file_sequential");
  group.sample_size(20);

  let temp_dir = tempdir().unwrap();
  let db_path = temp_dir.path().join("bench.kitedb");

  // Setup database
  {
    let db = open_single_file(
      &db_path,
      SingleFileOpenOptions::new().sync_mode(SyncMode::Normal),
    )
    .unwrap();
    db.begin(false).unwrap();
    for i in 0..1000 {
      let key = format!("node{i}");
      let node_id = db.create_node(Some(&key)).unwrap();
      db.set_node_prop_by_name(node_id, "value", PropValue::I64(i as i64))
        .unwrap();
    }
    db.commit().unwrap();
    kitedb::core::single_file::close_single_file(db).unwrap();
  }

  let db = open_single_file(
    &db_path,
    SingleFileOpenOptions::new().sync_mode(SyncMode::Normal),
  )
  .unwrap();

  group.throughput(Throughput::Elements(1000));

  group.bench_function("key_lookup_1000", |bencher| {
    bencher.iter(|| {
      for i in 0..1000 {
        black_box(db.node_by_key(&format!("node{i}")));
      }
    });
  });

  group.finish();
}

// ============================================================================
// Edge Existence Check Scaling
// ============================================================================

fn bench_edge_check_scaling(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_check_scaling");
  group.sample_size(15);

  let (_temp_dir, ray) = setup_ray_db(200, 400, false);
  let ray = Arc::new(parking_lot::RwLock::new(ray));

  // Get node IDs for edge checks
  let node_ids: Vec<u64> = {
    let ray_guard = ray.read();
    (0..200)
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

  let checks_per_thread = 200;

  for num_threads in [1, 2, 4, 8].iter() {
    group.throughput(Throughput::Elements(
      (*num_threads * checks_per_thread) as u64,
    ));

    group.bench_with_input(
      BenchmarkId::new("threads", num_threads),
      num_threads,
      |bencher, &num_threads| {
        let node_ids = Arc::clone(&node_ids);

        bencher.iter(|| {
          let barrier = Arc::new(Barrier::new(num_threads));
          let ray = Arc::clone(&ray);

          let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
              let ray = Arc::clone(&ray);
              let barrier = Arc::clone(&barrier);
              let node_ids = Arc::clone(&node_ids);

              thread::spawn(move || {
                barrier.wait();
                for i in 0..checks_per_thread {
                  let src_idx = (thread_id * 20 + i) % (node_ids.len() - 1);
                  let dst_idx = src_idx + 1;
                  let ray_guard = ray.read();
                  let _ =
                    black_box(ray_guard.has_edge(node_ids[src_idx], "FOLLOWS", node_ids[dst_idx]));
                }
              })
            })
            .collect();

          for handle in handles {
            handle.join().unwrap();
          }
        });
      },
    );
  }

  group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
  benches,
  bench_concurrent_reads,
  bench_read_scaling,
  bench_property_read_scaling,
  bench_traversal_scaling,
  bench_reader_writer_contention,
  bench_reader_writer_contention_batched,
  bench_reader_writer_contention_batched_group_commit,
  bench_mvcc_transaction_overhead,
  bench_single_file_sequential_reads,
  bench_edge_check_scaling,
);

criterion_main!(benches);
