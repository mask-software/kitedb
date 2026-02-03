//! Benchmarks for single-file core operations
//!
//! Run with: cargo bench --bench single_file

use criterion::{
  black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use tempfile::tempdir;

extern crate kitedb;

use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use kitedb::types::PropValue;

fn temp_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
  temp_dir.path().join("bench.kitedb")
}

fn open_bench_db(path: &std::path::Path) -> kitedb::core::single_file::SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new().sync_mode(SyncMode::Normal),
  )
  .unwrap()
}

fn bench_single_file_insert(c: &mut Criterion) {
  let mut group = c.benchmark_group("single_file_insert");
  group.sample_size(10);

  for count in [100usize, 1000usize].iter() {
    group.throughput(Throughput::Elements(*count as u64));
    group.bench_with_input(
      BenchmarkId::new("count", count),
      count,
      |bencher, &count| {
        bencher.iter_with_setup(
          || {
            let temp_dir = tempdir().unwrap();
            let db = open_bench_db(&temp_db_path(&temp_dir));
            (temp_dir, db)
          },
          |(_temp_dir, db)| {
            db.begin(false).unwrap();
            for i in 0..count {
              let key = format!("n{i}");
              let node_id = db.create_node(Some(&key)).unwrap();
              let _ = db.set_node_prop_by_name(node_id, "name", PropValue::String(key));
            }
            db.commit().unwrap();
            close_single_file(db).unwrap();
          },
        );
      },
    );
  }

  group.finish();
}

fn bench_single_file_checkpoint(c: &mut Criterion) {
  let mut group = c.benchmark_group("single_file_checkpoint");
  group.sample_size(10);

  for count in [1_000usize, 5_000usize].iter() {
    group.throughput(Throughput::Elements(*count as u64));
    group.bench_with_input(
      BenchmarkId::new("nodes", count),
      count,
      |bencher, &count| {
        bencher.iter_batched(
          || {
            let temp_dir = tempdir().unwrap();
            let db = open_bench_db(&temp_db_path(&temp_dir));
            db.begin(false).unwrap();
            for i in 0..count {
              let key = format!("n{i}");
              let _ = db.create_node(Some(&key)).unwrap();
            }
            db.commit().unwrap();
            (temp_dir, db)
          },
          |(_temp_dir, db)| {
            db.checkpoint().unwrap();
            black_box(());
            close_single_file(db).unwrap();
          },
          BatchSize::SmallInput,
        );
      },
    );
  }

  group.finish();
}

criterion_group!(
  benches,
  bench_single_file_insert,
  bench_single_file_checkpoint
);
criterion_main!(benches);
