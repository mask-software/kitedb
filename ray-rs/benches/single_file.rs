//! Benchmarks for single-file core operations
//!
//! Run with: cargo bench --bench single_file

use criterion::{
  black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::tempdir;

extern crate kitedb;

use kitedb::core::single_file::{
  close_single_file, close_single_file_with_options, open_single_file, SingleFileCloseOptions,
  SingleFileOpenOptions, SyncMode,
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
  .expect("expected value")
}

struct OpenCloseFixture {
  name: &'static str,
  path: PathBuf,
  wal_size: usize,
}

const MIB: usize = 1024 * 1024;

/// What an open/close fixture file holds.
#[derive(Clone, Copy, Default)]
struct FixtureSpec {
  nodes: usize,
  edges: usize,
  /// Nodes created after the final checkpoint, so they stay in the WAL.
  dirty_wal_tail: usize,
  /// Nodes (the first ones) that get a vector of `vector_dims` dimensions.
  vectors: usize,
  vector_dims: usize,
  wal_size: usize,
}

fn seed_graph_fixture(path: &Path, spec: FixtureSpec) {
  let FixtureSpec {
    nodes: node_count,
    edges: edge_count,
    dirty_wal_tail,
    vectors: vector_count,
    vector_dims,
    wal_size,
  } = spec;
  let db = open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .wal_size(wal_size)
      .auto_checkpoint(false),
  )
  .expect("expected value");

  if node_count > 0 {
    const NODE_BATCH_SIZE: usize = 2_000;
    let mut node_ids = Vec::with_capacity(node_count);

    for start in (0..node_count).step_by(NODE_BATCH_SIZE) {
      let end = (start + NODE_BATCH_SIZE).min(node_count);
      db.begin(false).expect("expected value");
      for i in start..end {
        let node_id = db
          .create_node(Some(&format!("bench:n{i}")))
          .expect("expected value");
        node_ids.push(node_id);
      }
      db.commit().expect("expected value");
    }

    if edge_count > 0 {
      const EDGE_BATCH_SIZE: usize = 4_000;
      db.begin(false).expect("expected value");
      let etype = db.define_etype("bench:connects").expect("expected value");
      db.commit().expect("expected value");

      for start in (0..edge_count).step_by(EDGE_BATCH_SIZE) {
        let end = (start + EDGE_BATCH_SIZE).min(edge_count);
        let mut edges = Vec::with_capacity(end - start);
        for i in start..end {
          let src_idx = i % node_count;
          let hop = (i / node_count) + 1;
          let mut dst_idx = (src_idx + hop) % node_count;
          if dst_idx == src_idx {
            dst_idx = (dst_idx + 1) % node_count;
          }
          edges.push((node_ids[src_idx], etype, node_ids[dst_idx]));
        }
        db.begin(false).expect("expected value");
        db.add_edges_batch(&edges).expect("expected value");
        db.commit().expect("expected value");
      }
    }

    if vector_count > 0 && vector_dims > 0 {
      const VECTOR_BATCH_SIZE: usize = 1_000;
      let vector_count = vector_count.min(node_ids.len());

      // Keep fixture generation stable for small WAL sizes by compacting
      // node/edge setup before vector batches.
      db.checkpoint().expect("expected value");

      db.begin(false).expect("expected value");
      let vector_prop = db
        .define_propkey("bench:embedding")
        .expect("expected value");
      db.commit().expect("expected value");

      for start in (0..vector_count).step_by(VECTOR_BATCH_SIZE) {
        let end = (start + VECTOR_BATCH_SIZE).min(vector_count);
        db.begin(false).expect("expected value");
        for (i, &node_id) in node_ids.iter().enumerate().take(end).skip(start) {
          let mut vector = vec![0.0f32; vector_dims];
          for (dim, value) in vector.iter_mut().enumerate() {
            *value = (((i + dim + 1) % 97) as f32) / 97.0;
          }
          db.set_node_vector(node_id, vector_prop, &vector)
            .expect("expected value");
        }
        db.commit().expect("expected value");
      }
    }

    db.checkpoint().expect("expected value");

    if dirty_wal_tail > 0 {
      for start in (0..dirty_wal_tail).step_by(NODE_BATCH_SIZE) {
        let end = (start + NODE_BATCH_SIZE).min(dirty_wal_tail);
        db.begin(false).expect("expected value");
        for i in start..end {
          let _ = db
            .create_node(Some(&format!("bench:tail{i}")))
            .expect("expected value");
        }
        db.commit().expect("expected value");
      }
    }
  }

  close_single_file(db).expect("expected value");
}

fn build_open_close_fixture(
  temp_dir: &tempfile::TempDir,
  name: &'static str,
  spec: FixtureSpec,
) -> OpenCloseFixture {
  let path = temp_dir.path().join(format!("open-close-{name}.kitedb"));
  seed_graph_fixture(&path, spec);

  let size = fs::metadata(&path).expect("expected value").len();
  println!(
    "prepared fixture {name}: nodes={}, edges={}, vectors={}, vector_dims={}, wal_size={} bytes, file_size={} bytes",
    spec.nodes, spec.edges, spec.vectors, spec.vector_dims, spec.wal_size, size
  );

  OpenCloseFixture {
    name,
    path,
    wal_size: spec.wal_size,
  }
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
            let temp_dir = tempdir().expect("expected value");
            let db = open_bench_db(&temp_db_path(&temp_dir));
            (temp_dir, db)
          },
          |(_temp_dir, db)| {
            db.begin(false).expect("expected value");
            for i in 0..count {
              let key = format!("n{i}");
              let node_id = db.create_node(Some(&key)).expect("expected value");
              let _ = db.set_node_prop_by_name(node_id, "name", PropValue::String(key));
            }
            db.commit().expect("expected value");
            close_single_file(db).expect("expected value");
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
            let temp_dir = tempdir().expect("expected value");
            let db = open_bench_db(&temp_db_path(&temp_dir));
            db.begin(false).expect("expected value");
            for i in 0..count {
              let key = format!("n{i}");
              let _ = db.create_node(Some(&key)).expect("expected value");
            }
            db.commit().expect("expected value");
            (temp_dir, db)
          },
          |(_temp_dir, db)| {
            db.checkpoint().expect("expected value");
            black_box(());
            close_single_file(db).expect("expected value");
          },
          BatchSize::SmallInput,
        );
      },
    );
  }

  group.finish();
}

fn bench_single_file_open_close(c: &mut Criterion) {
  let mut group = c.benchmark_group("single_file_open_close");
  group.sample_size(30);

  let temp_dir = tempdir().expect("expected value");
  let fixtures = vec![
    build_open_close_fixture(
      &temp_dir,
      "empty",
      FixtureSpec {
        wal_size: 4 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_1k_2k",
      FixtureSpec {
        nodes: 1_000,
        edges: 2_000,
        wal_size: 4 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_10k_20k",
      FixtureSpec {
        nodes: 10_000,
        edges: 20_000,
        wal_size: 4 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_10k_20k_vec5k",
      FixtureSpec {
        nodes: 10_000,
        edges: 20_000,
        vectors: 5_000,
        vector_dims: 128,
        wal_size: 4 * MIB,
        ..FixtureSpec::default()
      },
    ),
  ];

  for fixture in &fixtures {
    for (mode_name, read_only) in [("rw", false), ("ro", true)] {
      group.bench_with_input(
        BenchmarkId::new(format!("open_only/{mode_name}"), fixture.name),
        fixture,
        |bencher, fixture| {
          bencher.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
              let start = Instant::now();
              let db = open_single_file(
                &fixture.path,
                SingleFileOpenOptions::new()
                  .sync_mode(SyncMode::Normal)
                  .wal_size(fixture.wal_size)
                  .create_if_missing(false)
                  .read_only(read_only),
              )
              .expect("expected value");
              total += start.elapsed();
              close_single_file(db).expect("expected value");
            }
            total
          });
        },
      );

      group.bench_with_input(
        BenchmarkId::new(format!("close_only/{mode_name}"), fixture.name),
        fixture,
        |bencher, fixture| {
          bencher.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
              let db = open_single_file(
                &fixture.path,
                SingleFileOpenOptions::new()
                  .sync_mode(SyncMode::Normal)
                  .wal_size(fixture.wal_size)
                  .create_if_missing(false)
                  .read_only(read_only),
              )
              .expect("expected value");
              let start = Instant::now();
              close_single_file(db).expect("expected value");
              total += start.elapsed();
            }
            total
          });
        },
      );

      group.bench_with_input(
        BenchmarkId::new(format!("open_close/{mode_name}"), fixture.name),
        fixture,
        |bencher, fixture| {
          bencher.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
              let start = Instant::now();
              let db = open_single_file(
                &fixture.path,
                SingleFileOpenOptions::new()
                  .sync_mode(SyncMode::Normal)
                  .wal_size(fixture.wal_size)
                  .create_if_missing(false)
                  .read_only(read_only),
              )
              .expect("expected value");
              close_single_file(db).expect("expected value");
              total += start.elapsed();
            }
            total
          });
        },
      );
    }
  }

  group.finish();
}

fn bench_single_file_open_close_limits(c: &mut Criterion) {
  let mut group = c.benchmark_group("single_file_open_close_limits");
  group.sample_size(10);
  group.measurement_time(Duration::from_secs(4));

  let temp_dir = tempdir().expect("expected value");
  let fixtures = vec![
    build_open_close_fixture(
      &temp_dir,
      "graph_10k_20k_dirty_wal",
      FixtureSpec {
        nodes: 10_000,
        edges: 20_000,
        dirty_wal_tail: 2_000,
        wal_size: 64 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_100k_200k",
      FixtureSpec {
        nodes: 100_000,
        edges: 200_000,
        wal_size: 64 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_100k_200k_vec20k",
      FixtureSpec {
        nodes: 100_000,
        edges: 200_000,
        vectors: 20_000,
        vector_dims: 128,
        wal_size: 64 * MIB,
        ..FixtureSpec::default()
      },
    ),
    build_open_close_fixture(
      &temp_dir,
      "graph_100k_200k_dirty_wal",
      FixtureSpec {
        nodes: 100_000,
        edges: 200_000,
        dirty_wal_tail: 20_000,
        wal_size: 64 * MIB,
        ..FixtureSpec::default()
      },
    ),
  ];

  for fixture in &fixtures {
    for (mode_name, read_only) in [("rw", false), ("ro", true)] {
      group.bench_with_input(
        BenchmarkId::new(format!("open_close/{mode_name}"), fixture.name),
        fixture,
        |bencher, fixture| {
          bencher.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
              let start = Instant::now();
              let db = open_single_file(
                &fixture.path,
                SingleFileOpenOptions::new()
                  .sync_mode(SyncMode::Normal)
                  .wal_size(fixture.wal_size)
                  .create_if_missing(false)
                  .read_only(read_only),
              )
              .expect("expected value");
              close_single_file(db).expect("expected value");
              total += start.elapsed();
            }
            total
          });
        },
      );

      if fixture.name.contains("dirty_wal") {
        group.bench_with_input(
          BenchmarkId::new(format!("open_close_ckpt01/{mode_name}"), fixture.name),
          fixture,
          |bencher, fixture| {
            bencher.iter_custom(|iters| {
              let bench_tmp = tempdir().expect("expected value");
              let bench_path = bench_tmp.path().join("bench-copy.kitedb");
              fs::copy(&fixture.path, &bench_path).expect("expected value");

              let mut total = Duration::ZERO;
              for _ in 0..iters {
                let start = Instant::now();
                let db = open_single_file(
                  &bench_path,
                  SingleFileOpenOptions::new()
                    .sync_mode(SyncMode::Normal)
                    .wal_size(fixture.wal_size)
                    .create_if_missing(false)
                    .read_only(read_only),
                )
                .expect("expected value");
                close_single_file_with_options(
                  db,
                  SingleFileCloseOptions::new().checkpoint_if_wal_usage_at_least(0.01),
                )
                .expect("expected value");
                total += start.elapsed();
              }
              total
            });
          },
        );
      }
    }
  }

  group.finish();
}

criterion_group!(
  benches,
  bench_single_file_insert,
  bench_single_file_checkpoint,
  bench_single_file_open_close,
  bench_single_file_open_close_limits
);
criterion_main!(benches);
