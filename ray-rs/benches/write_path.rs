//! Write path microbenchmarks
//!
//! Run with: cargo bench --bench write_path

use criterion::{
  black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

extern crate kitedb;

use kitedb::api::kite::{Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::SyncMode;
use kitedb::error::KiteError;
use kitedb::types::PropValue;

fn temp_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
  temp_dir.path().join("bench")
}

#[derive(Clone, Copy, Debug)]
enum LockMode {
  PerOp,
  Batched,
}

fn create_write_schema(
  group_commit: bool,
  wal_mb: Option<usize>,
  checkpoint_threshold: Option<f64>,
) -> KiteOptions {
  let user = NodeDef::new("User", "user:").prop(PropDef::string("name"));

  let mut options = KiteOptions::new()
    .node(user)
    .sync_mode(SyncMode::Normal)
    .group_commit_enabled(group_commit)
    .group_commit_window_ms(2);

  let wal_mb = wal_mb.or_else(|| {
    env::var("KITE_BENCH_WAL_MB")
      .ok()
      .and_then(|value| value.parse::<usize>().ok())
  });
  let checkpoint_threshold = checkpoint_threshold.or_else(|| {
    env::var("KITE_BENCH_CHECKPOINT_THRESHOLD")
      .ok()
      .and_then(|value| value.parse::<f64>().ok())
  });

  if let Some(mb) = wal_mb {
    options = options.wal_size_mb(mb);
  }
  if let Some(value) = checkpoint_threshold {
    options = options.checkpoint_threshold(value);
  }

  options
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

#[cfg(feature = "bench-profile")]
fn maybe_log_profile(kite: &Kite, label: &str) {
  if bench_profile_enabled() {
    let (commit_wait_ns, wal_flush_ns) = kite.take_profile_snapshot();
    println!(
      "[write_path] {label} commit_lock_wait_ns={commit_wait_ns} wal_flush_ns={wal_flush_ns}"
    );
  }
}

fn create_user_with_retry(kite: &mut Kite, idx: usize) -> Result<(), KiteError> {
  let key = format!("user{idx}");
  let mut attempts = 0;
  loop {
    let mut props = HashMap::with_capacity(1);
    props.insert("name".to_string(), PropValue::String(key.clone()));
    match kite.create_node("User", &key, props) {
      Ok(_) => return Ok(()),
      Err(KiteError::WalBufferFull) if attempts == 0 => {
        kite.optimize()?;
        attempts += 1;
      }
      Err(err) => return Err(err),
    }
  }
}

fn create_users_batched_with_retry(
  kite: &mut Kite,
  start: usize,
  count: usize,
) -> Result<(), KiteError> {
  let mut attempts = 0;
  loop {
    let result = kite.transaction(|ctx| {
      for offset in 0..count {
        let idx = start + offset;
        let key = format!("user{idx}");
        let mut props = HashMap::with_capacity(1);
        props.insert("name".to_string(), PropValue::String(key.clone()));
        ctx.create_node("User", &key, props)?;
      }
      Ok(())
    });

    match result {
      Ok(_) => return Ok(()),
      Err(KiteError::WalBufferFull) if attempts == 0 => {
        kite.optimize()?;
        attempts += 1;
      }
      Err(err) => return Err(err),
    }
  }
}

fn bench_write_concurrent_variant(
  c: &mut Criterion,
  group_name: &str,
  lock_mode: LockMode,
  group_commit: bool,
) {
  let mut group = c.benchmark_group(group_name);
  group.sample_size(10);

  for &num_threads in [1usize, 2, 4, 8].iter() {
    let ops_per_thread = 500usize;
    let total_ops = num_threads * ops_per_thread;
    group.throughput(Throughput::Elements(total_ops as u64));

    group.bench_with_input(
      BenchmarkId::new(if group_commit { "gc_on" } else { "gc_off" }, num_threads),
      &num_threads,
      move |bencher, &num_threads| {
        bencher.iter_batched(
          || {
            let temp_dir = tempdir().unwrap();
            let kite = Kite::open(
              temp_db_path(&temp_dir),
              create_write_schema(group_commit, None, None),
            )
            .unwrap();
            let kite = Arc::new(parking_lot::RwLock::new(kite));
            (temp_dir, kite)
          },
          |(_temp_dir, kite)| {
            let barrier = Arc::new(Barrier::new(num_threads));
            let counter = Arc::new(AtomicU64::new(0));
            let mut handles = Vec::with_capacity(num_threads);

            for _ in 0..num_threads {
              let kite = Arc::clone(&kite);
              let barrier = Arc::clone(&barrier);
              let counter = Arc::clone(&counter);

              handles.push(thread::spawn(move || {
                barrier.wait();
                match lock_mode {
                  LockMode::PerOp => {
                    for _ in 0..ops_per_thread {
                      let idx = counter.fetch_add(1, Ordering::SeqCst) as usize;
                      let mut guard = kite.write();
                      create_user_with_retry(&mut guard, idx).unwrap();
                    }
                  }
                  LockMode::Batched => {
                    let mut i = 0usize;
                    while i < ops_per_thread {
                      let batch = (ops_per_thread - i).min(100);
                      let start = counter.fetch_add(batch as u64, Ordering::SeqCst) as usize;
                      let mut guard = kite.write();
                      create_users_batched_with_retry(&mut guard, start, batch).unwrap();
                      i += batch;
                    }
                  }
                }
              }));
            }

            for handle in handles {
              handle.join().unwrap();
            }

            #[cfg(feature = "bench-profile")]
            {
              let kite_ref = kite.read();
              maybe_log_profile(
                &kite_ref,
                &format!("concurrent threads={num_threads} mode={lock_mode:?} gc={group_commit}"),
              );
            }

            if let Ok(lock) = Arc::try_unwrap(kite) {
              let kite = lock.into_inner();
              kite.close().unwrap();
            }
          },
          BatchSize::SmallInput,
        );
      },
    );
  }

  group.finish();
}

fn bench_write_single(c: &mut Criterion) {
  let mut group = c.benchmark_group("write_single");
  group.sample_size(10);

  for &count in [1_000usize, 5_000usize].iter() {
    for &group_commit in [false, true].iter() {
      let label = if group_commit { "gc_on" } else { "gc_off" };
      group.throughput(Throughput::Elements(count as u64));
      group.bench_with_input(
        BenchmarkId::new(label, count),
        &count,
        move |bencher, &count| {
          bencher.iter_batched(
            || {
              let temp_dir = tempdir().unwrap();
              let kite = Kite::open(
                temp_db_path(&temp_dir),
                create_write_schema(group_commit, None, None),
              )
              .unwrap();
              (temp_dir, kite)
            },
            |(_temp_dir, mut kite)| {
              for i in 0..count {
                create_user_with_retry(&mut kite, i).unwrap();
              }
              #[cfg(feature = "bench-profile")]
              maybe_log_profile(&kite, &format!("single count={count} gc={group_commit}"));
              kite.close().unwrap();
              black_box(());
            },
            BatchSize::SmallInput,
          );
        },
      );
    }
  }

  group.finish();
}

fn bench_write_batch_tx(c: &mut Criterion) {
  let mut group = c.benchmark_group("write_batch_tx");
  group.sample_size(10);

  for &total_ops in [1_000usize, 5_000usize].iter() {
    for &batch_size in [10usize, 100usize, 500usize, 1000usize].iter() {
      for &group_commit in [false, true].iter() {
        let label = if group_commit { "gc_on" } else { "gc_off" };
        let bench_id = format!("bs{batch_size}_{label}");
        group.throughput(Throughput::Elements(total_ops as u64));
        group.bench_with_input(
          BenchmarkId::new(bench_id, total_ops),
          &total_ops,
          move |bencher, &total_ops| {
            bencher.iter_batched(
              || {
                let temp_dir = tempdir().unwrap();
                let kite = Kite::open(
                  temp_db_path(&temp_dir),
                  create_write_schema(group_commit, None, None),
                )
                .unwrap();
                (temp_dir, kite)
              },
              |(_temp_dir, mut kite)| {
                let mut start = 0usize;
                while start < total_ops {
                  let count = (total_ops - start).min(batch_size);
                  create_users_batched_with_retry(&mut kite, start, count).unwrap();
                  start += count;
                }
                #[cfg(feature = "bench-profile")]
                maybe_log_profile(
                  &kite,
                  &format!("batch total={total_ops} batch_size={batch_size} gc={group_commit}"),
                );
                kite.close().unwrap();
                black_box(());
              },
              BatchSize::SmallInput,
            );
          },
        );
      }
    }
  }

  group.finish();
}

fn bench_write_batch_tx_wal_sweep(c: &mut Criterion) {
  let mut group = c.benchmark_group("write_batch_tx_wal_sweep");
  group.sample_size(10);

  let total_ops = 5_000usize;
  let batch_size = 100usize;
  let wal_sizes = [4usize, 16, 64, 256];
  let thresholds = [0.5_f64, 0.8_f64, 0.95_f64];

  for &wal_mb in wal_sizes.iter() {
    for &threshold in thresholds.iter() {
      let bench_id = format!("wal{wal_mb}_thr{threshold:.2}");
      group.throughput(Throughput::Elements(total_ops as u64));
      group.bench_with_input(
        BenchmarkId::new(bench_id, total_ops),
        &total_ops,
        move |bencher, &total_ops| {
          bencher.iter_batched(
            || {
              let temp_dir = tempdir().unwrap();
              let kite = Kite::open(
                temp_db_path(&temp_dir),
                create_write_schema(false, Some(wal_mb), Some(threshold)),
              )
              .unwrap();
              (temp_dir, kite)
            },
            |(_temp_dir, mut kite)| {
              let mut start = 0usize;
              while start < total_ops {
                let count = (total_ops - start).min(batch_size);
                create_users_batched_with_retry(&mut kite, start, count).unwrap();
                start += count;
              }
              #[cfg(feature = "bench-profile")]
              maybe_log_profile(
                &kite,
                &format!(
                  "wal_sweep total={total_ops} batch_size={batch_size} wal_mb={wal_mb} thr={threshold}"
                ),
              );
              kite.close().unwrap();
              black_box(());
            },
            BatchSize::SmallInput,
          );
        },
      );
    }
  }

  group.finish();
}

fn bench_write_concurrent_per_op(c: &mut Criterion) {
  bench_write_concurrent_variant(c, "write_concurrent_per_op", LockMode::PerOp, false);
  bench_write_concurrent_variant(c, "write_concurrent_per_op_gc", LockMode::PerOp, true);
}

fn bench_write_concurrent_batched(c: &mut Criterion) {
  bench_write_concurrent_variant(c, "write_concurrent_batched", LockMode::Batched, false);
  bench_write_concurrent_variant(c, "write_concurrent_batched_gc", LockMode::Batched, true);
}

criterion_group!(
  benches,
  bench_write_single,
  bench_write_batch_tx,
  bench_write_batch_tx_wal_sweep,
  bench_write_concurrent_per_op,
  bench_write_concurrent_batched
);
criterion_main!(benches);
