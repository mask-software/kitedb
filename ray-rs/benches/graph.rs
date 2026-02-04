//! Benchmarks for graph operations
//!
//! Run with: cargo bench --bench graph

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::collections::HashMap;
use tempfile::tempdir;

extern crate kitedb;
use kitedb::api::kite::{BatchOp, EdgeDef, Kite, KiteOptions, NodeDef, PropDef};
use kitedb::core::single_file::SyncMode;
use kitedb::types::{NodeId, PropValue};
use std::env;

fn temp_db_path(temp_dir: &tempfile::TempDir) -> std::path::PathBuf {
  temp_dir.path().join("bench")
}

fn create_test_schema() -> KiteOptions {
  let user = NodeDef::new("User", "user:")
    .prop(PropDef::string("name"))
    .prop(PropDef::int("age"));

  let follows = EdgeDef::new("FOLLOWS");

  KiteOptions::new()
    .node(user)
    .edge(follows)
    .sync_mode(SyncMode::Normal)
}

fn create_edge_prop_schema() -> KiteOptions {
  let user = NodeDef::new("User", "user:")
    .prop(PropDef::string("name"))
    .prop(PropDef::int("age"));

  let follows = EdgeDef::new("FOLLOWS").prop(PropDef::float("weight"));

  KiteOptions::new()
    .node(user)
    .edge(follows)
    .sync_mode(SyncMode::Normal)
}

fn create_code_graph_schema() -> KiteOptions {
  let file = NodeDef::new("File", "file:").prop(PropDef::string("path"));
  let chunk = NodeDef::new("Chunk", "chunk:").prop(PropDef::int("index"));
  let symbol = NodeDef::new("Symbol", "sym:").prop(PropDef::string("name"));

  let contains = EdgeDef::new("CONTAINS").prop(PropDef::int("order"));
  let references = EdgeDef::new("REFERENCES")
    .prop(PropDef::int("line"))
    .prop(PropDef::int("role"));
  let calls = EdgeDef::new("CALLS")
    .prop(PropDef::int("line"))
    .prop(PropDef::float("weight"));
  let imports = EdgeDef::new("IMPORTS").prop(PropDef::int("line"));

  let mut options = KiteOptions::new()
    .node(file)
    .node(chunk)
    .node(symbol)
    .edge(contains)
    .edge(references)
    .edge(calls)
    .edge(imports)
    .sync_mode(SyncMode::Normal);

  if let Ok(wal_mb) = env::var("KITE_BENCH_WAL_MB") {
    if let Ok(mb) = wal_mb.parse::<usize>() {
      options = options.wal_size_mb(mb);
    }
  }
  if let Ok(threshold) = env::var("KITE_BENCH_CHECKPOINT_THRESHOLD") {
    if let Ok(value) = threshold.parse::<f64>() {
      options = options.checkpoint_threshold(value);
    }
  }

  options
}

struct CodeGraphFixture {
  contains: Vec<(NodeId, NodeId, i64)>,
  references: Vec<(NodeId, NodeId, i64, i64)>,
  calls: Vec<(NodeId, NodeId, i64, f64)>,
  imports: Vec<(NodeId, NodeId, i64)>,
}

fn build_code_graph_fixture(
  ray: &mut Kite,
  file_count: usize,
  chunks_per_file: usize,
  symbols_per_file: usize,
  refs_per_chunk: usize,
  calls_per_chunk: usize,
  imports_per_file: usize,
) -> CodeGraphFixture {
  let mut file_ids = Vec::with_capacity(file_count);
  let mut chunk_ids: Vec<Vec<NodeId>> = Vec::with_capacity(file_count);
  let mut symbol_ids: Vec<Vec<NodeId>> = Vec::with_capacity(file_count);

  for i in 0..file_count {
    let file = ray
      .create_node("File", &format!("file{i}"), HashMap::new())
      .unwrap();
    file_ids.push(file.id());

    let mut chunks = Vec::with_capacity(chunks_per_file);
    for c in 0..chunks_per_file {
      let chunk = ray
        .create_node("Chunk", &format!("file{i}:chunk{c}"), HashMap::new())
        .unwrap();
      chunks.push(chunk.id());
    }
    chunk_ids.push(chunks);

    let mut symbols = Vec::with_capacity(symbols_per_file);
    for s in 0..symbols_per_file {
      let symbol = ray
        .create_node("Symbol", &format!("file{i}:sym{s}"), HashMap::new())
        .unwrap();
      symbols.push(symbol.id());
    }
    symbol_ids.push(symbols);
  }

  let mut contains = Vec::new();
  let mut references = Vec::new();
  let mut calls = Vec::new();
  let mut imports = Vec::new();

  for file_idx in 0..file_count {
    let file_id = file_ids[file_idx];
    let chunks = &chunk_ids[file_idx];
    let symbols = &symbol_ids[file_idx];

    for (order, &chunk_id) in chunks.iter().enumerate() {
      contains.push((file_id, chunk_id, order as i64));

      for r in 0..refs_per_chunk {
        let sym_id = symbols[(order * refs_per_chunk + r) % symbols_per_file];
        let line = (r + 1) as i64;
        let role = (r % 4) as i64;
        references.push((chunk_id, sym_id, line, role));
      }

      for r in 0..calls_per_chunk {
        let sym_id = symbols[(order * calls_per_chunk + r) % symbols_per_file];
        let line = (r + 1) as i64;
        let weight = 0.5 + (r as f64) * 0.1;
        calls.push((chunk_id, sym_id, line, weight));
      }
    }

    for i in 0..imports_per_file {
      let target = (file_idx + i + 1) % file_count;
      imports.push((file_id, file_ids[target], (i + 1) as i64));
    }
  }

  CodeGraphFixture {
    contains,
    references,
    calls,
    imports,
  }
}

fn apply_code_graph_edges(ray: &mut Kite, fixture: &CodeGraphFixture) {
  for (src, dst, order) in fixture.contains.iter() {
    let mut props = HashMap::with_capacity(1);
    props.insert("order".to_string(), PropValue::I64(*order));
    let _ = ray.link_with_props(*src, "CONTAINS", *dst, props);
  }

  for (src, dst, line, role) in fixture.references.iter() {
    let mut props = HashMap::with_capacity(2);
    props.insert("line".to_string(), PropValue::I64(*line));
    props.insert("role".to_string(), PropValue::I64(*role));
    let _ = ray.link_with_props(*src, "REFERENCES", *dst, props);
  }

  for (src, dst, line, weight) in fixture.calls.iter() {
    let mut props = HashMap::with_capacity(2);
    props.insert("line".to_string(), PropValue::I64(*line));
    props.insert("weight".to_string(), PropValue::F64(*weight));
    let _ = ray.link_with_props(*src, "CALLS", *dst, props);
  }

  for (src, dst, line) in fixture.imports.iter() {
    let mut props = HashMap::with_capacity(1);
    props.insert("line".to_string(), PropValue::I64(*line));
    let _ = ray.link_with_props(*src, "IMPORTS", *dst, props);
  }
}

fn apply_code_graph_edges_batched(ray: &mut Kite, fixture: &CodeGraphFixture, batch_size: usize) {
  if batch_size == 0 {
    apply_code_graph_edges(ray, fixture);
    return;
  }

  let mut ops = Vec::with_capacity(batch_size);
  let flush = |ray: &mut Kite, ops: &mut Vec<BatchOp>| {
    if !ops.is_empty() {
      let pending = std::mem::take(ops);
      ray.batch(pending).unwrap();
    }
  };

  for (src, dst, order) in fixture.contains.iter() {
    let mut props = HashMap::with_capacity(1);
    props.insert("order".to_string(), PropValue::I64(*order));
    ops.push(BatchOp::LinkWithProps {
      src: *src,
      edge_type: "CONTAINS".into(),
      dst: *dst,
      props,
    });
    if ops.len() >= batch_size {
      flush(ray, &mut ops);
    }
  }

  for (src, dst, line, role) in fixture.references.iter() {
    let mut props = HashMap::with_capacity(2);
    props.insert("line".to_string(), PropValue::I64(*line));
    props.insert("role".to_string(), PropValue::I64(*role));
    ops.push(BatchOp::LinkWithProps {
      src: *src,
      edge_type: "REFERENCES".into(),
      dst: *dst,
      props,
    });
    if ops.len() >= batch_size {
      flush(ray, &mut ops);
    }
  }

  for (src, dst, line, weight) in fixture.calls.iter() {
    let mut props = HashMap::with_capacity(2);
    props.insert("line".to_string(), PropValue::I64(*line));
    props.insert("weight".to_string(), PropValue::F64(*weight));
    ops.push(BatchOp::LinkWithProps {
      src: *src,
      edge_type: "CALLS".into(),
      dst: *dst,
      props,
    });
    if ops.len() >= batch_size {
      flush(ray, &mut ops);
    }
  }

  for (src, dst, line) in fixture.imports.iter() {
    let mut props = HashMap::with_capacity(1);
    props.insert("line".to_string(), PropValue::I64(*line));
    ops.push(BatchOp::LinkWithProps {
      src: *src,
      edge_type: "IMPORTS".into(),
      dst: *dst,
      props,
    });
    if ops.len() >= batch_size {
      flush(ray, &mut ops);
    }
  }

  flush(ray, &mut ops);
}

// =============================================================================
// Node CRUD Benchmarks
// =============================================================================

fn bench_create_node(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_create");
  group.sample_size(10); // Reduce sample size for expensive operations

  for count in [100, 500, 1000].iter() {
    group.throughput(Throughput::Elements(*count as u64));

    group.bench_with_input(
      BenchmarkId::new("count", count),
      count,
      |bencher, &count| {
        bencher.iter_with_setup(
          || {
            let temp_dir = tempdir().unwrap();
            let ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();
            (temp_dir, ray)
          },
          |(_temp_dir, mut ray)| {
            for i in 0..count {
              let mut props = HashMap::new();
              props.insert("name".to_string(), PropValue::String(format!("User{i}")));
              props.insert("age".to_string(), PropValue::I64(i as i64));
              let _ = black_box(ray.create_node("User", &format!("user{i}"), props));
            }
          },
        );
      },
    );
  }

  group.finish();
}

fn bench_get_node_by_key(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_get_by_key");

  // Setup: Create database with nodes
  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create 1000 nodes (smaller for faster setup)
  for i in 0..1000 {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String(format!("User{i}")));
    ray.create_node("User", &format!("user{i}"), props).unwrap();
  }

  group.bench_function("get_existing", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let key = format!("user{}", i % 1000);
      let _ = black_box(ray.get("User", &key));
      i += 1;
    });
  });

  group.bench_function("get_nonexistent", |bencher| {
    bencher.iter(|| {
      let _ = black_box(ray.get("User", "nonexistent"));
    });
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_get_node_by_key_micro(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_get_by_key_micro");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  let mut keys = Vec::with_capacity(1000);
  for i in 0..1000 {
    let key = format!("user{i}");
    ray.create_node("User", &key, HashMap::new()).unwrap();
    keys.push(key);
  }

  group.bench_function("existing_prebuilt", |bencher| {
    let mut i = 0usize;
    bencher.iter(|| {
      let key = &keys[i % keys.len()];
      let _ = black_box(ray.get("User", key));
      i = i.wrapping_add(1);
    });
  });

  group.bench_function("nonexistent_prebuilt", |bencher| {
    bencher.iter(|| {
      let _ = black_box(ray.get("User", "nonexistent"));
    });
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_node_exists(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_exists");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create nodes and store IDs
  let mut node_ids = Vec::new();
  for i in 0..1000 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  group.bench_function("exists_true", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % node_ids.len()];
      let _ = black_box(ray.exists(id));
      i += 1;
    });
  });

  group.bench_function("exists_false", |bencher| {
    bencher.iter(|| {
      let _ = black_box(ray.exists(999999999));
    });
  });

  group.finish();
  ray.close().unwrap();
}

// =============================================================================
// Edge Benchmarks
// =============================================================================

fn bench_link(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_link");
  group.sample_size(10); // Reduce sample size for expensive operations

  for edge_count in [100, 500, 1000].iter() {
    group.throughput(Throughput::Elements(*edge_count as u64));

    group.bench_with_input(
      BenchmarkId::new("edges", edge_count),
      edge_count,
      |bencher, &edge_count| {
        bencher.iter_with_setup(
          || {
            let temp_dir = tempdir().unwrap();
            let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

            // Create nodes first
            let node_count = ((edge_count as f64).sqrt() as usize).max(10);
            let mut node_ids = Vec::new();
            for i in 0..node_count {
              let node = ray
                .create_node("User", &format!("user{i}"), HashMap::new())
                .unwrap();
              node_ids.push(node.id());
            }

            (temp_dir, ray, node_ids)
          },
          |(_temp_dir, mut ray, node_ids)| {
            let node_count = node_ids.len();
            for i in 0..edge_count {
              let src = node_ids[i % node_count];
              let dst = node_ids[(i + 1) % node_count];
              if src != dst {
                let _ = black_box(ray.link(src, "FOLLOWS", dst));
              }
            }
          },
        );
      },
    );
  }

  group.finish();
}

fn bench_has_edge(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_has_edge");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create nodes and edges
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  // Create edges in a chain
  for i in 0..99 {
    ray.link(node_ids[i], "FOLLOWS", node_ids[i + 1]).unwrap();
  }

  group.bench_function("has_edge_true", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let src = node_ids[i % 99];
      let dst = node_ids[(i % 99) + 1];
      let _ = black_box(ray.has_edge(src, "FOLLOWS", dst));
      i += 1;
    });
  });

  group.bench_function("has_edge_false", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      // Check reverse direction (which doesn't exist)
      let src = node_ids[(i % 99) + 1];
      let dst = node_ids[i % 99];
      let _ = black_box(ray.has_edge(src, "FOLLOWS", dst));
      i += 1;
    });
  });

  group.finish();
  ray.close().unwrap();
}

// =============================================================================
// Edge Property Benchmarks
// =============================================================================

fn bench_set_edge_prop(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_set");
  group.sample_size(10);

  for edge_count in [100, 500, 1000].iter() {
    group.throughput(Throughput::Elements(*edge_count as u64));

    group.bench_with_input(
      BenchmarkId::new("edges", edge_count),
      edge_count,
      |bencher, &edge_count| {
        bencher.iter_with_setup(
          || {
            let temp_dir = tempdir().unwrap();
            let mut ray = Kite::open(temp_db_path(&temp_dir), create_edge_prop_schema()).unwrap();

            let node_count = ((edge_count as f64).sqrt() as usize).max(10);
            let mut node_ids = Vec::new();
            for i in 0..node_count {
              let node = ray
                .create_node("User", &format!("user{i}"), HashMap::new())
                .unwrap();
              node_ids.push(node.id());
            }

            let mut edges = Vec::with_capacity(edge_count);
            for i in 0..edge_count {
              let src = node_ids[i % node_count];
              let dst = node_ids[(i + 1) % node_count];
              if src != dst {
                let _ = ray.link(src, "FOLLOWS", dst);
                edges.push((src, dst));
              }
            }

            (temp_dir, ray, edges)
          },
          |(_temp_dir, mut ray, edges)| {
            for (i, (src, dst)) in edges.iter().enumerate() {
              let value = PropValue::F64(i as f64 * 0.01);
              let _ = black_box(ray.set_edge_prop(*src, "FOLLOWS", *dst, "weight", value));
            }
          },
        );
      },
    );
  }

  group.finish();
}

fn bench_get_edge_prop(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_get");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_edge_prop_schema()).unwrap();

  let edge_count = 1000usize;
  let node_count = (edge_count as f64).sqrt() as usize;
  let mut node_ids = Vec::new();
  for i in 0..node_count {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  let mut edges = Vec::with_capacity(edge_count);
  for i in 0..edge_count {
    let src = node_ids[i % node_count];
    let dst = node_ids[(i + 1) % node_count];
    if src != dst {
      let _ = ray.link(src, "FOLLOWS", dst);
      let value = PropValue::F64(i as f64 * 0.01);
      let _ = ray.set_edge_prop(src, "FOLLOWS", dst, "weight", value);
      edges.push((src, dst));
    }
  }

  group.bench_function("get_existing", |bencher| {
    let mut i = 0usize;
    bencher.iter(|| {
      let (src, dst) = edges[i % edges.len()];
      let _ = black_box(ray.edge_prop(src, "FOLLOWS", dst, "weight"));
      i += 1;
    });
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_edge_prop_codegraph_write(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_codegraph_write");
  group.sample_size(10);

  group.bench_function("write", |bencher| {
    bencher.iter_with_setup(
      || {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_code_graph_schema()).unwrap();
        let file_count = env::var("KITE_BENCH_FILES")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(100);
        let chunks_per_file = env::var("KITE_BENCH_CHUNKS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(20);
        let symbols_per_file = env::var("KITE_BENCH_SYMBOLS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(120);
        let refs_per_chunk = env::var("KITE_BENCH_REFS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(15);
        let calls_per_chunk = env::var("KITE_BENCH_CALLS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(5);
        let imports_per_file = env::var("KITE_BENCH_IMPORTS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(4);
        let fixture = build_code_graph_fixture(
          &mut ray,
          file_count,
          chunks_per_file,
          symbols_per_file,
          refs_per_chunk,
          calls_per_chunk,
          imports_per_file,
        );
        (temp_dir, ray, fixture)
      },
      |(_temp_dir, mut ray, fixture)| {
        apply_code_graph_edges(&mut ray, &fixture);
      },
    );
  });

  group.finish();
}

fn bench_edge_prop_codegraph_write_batched(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_codegraph_write_batched");
  group.sample_size(10);

  let batch_size = env::var("KITE_BENCH_BATCH_SIZE")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(3000);

  group.bench_function("write_batched", |bencher| {
    bencher.iter_with_setup(
      || {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_code_graph_schema()).unwrap();
        let file_count = env::var("KITE_BENCH_FILES")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(100);
        let chunks_per_file = env::var("KITE_BENCH_CHUNKS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(20);
        let symbols_per_file = env::var("KITE_BENCH_SYMBOLS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(120);
        let refs_per_chunk = env::var("KITE_BENCH_REFS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(15);
        let calls_per_chunk = env::var("KITE_BENCH_CALLS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(5);
        let imports_per_file = env::var("KITE_BENCH_IMPORTS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(4);
        let fixture = build_code_graph_fixture(
          &mut ray,
          file_count,
          chunks_per_file,
          symbols_per_file,
          refs_per_chunk,
          calls_per_chunk,
          imports_per_file,
        );
        (temp_dir, ray, fixture)
      },
      |(_temp_dir, mut ray, fixture)| {
        apply_code_graph_edges_batched(&mut ray, &fixture, batch_size);
      },
    );
  });

  group.finish();
}

fn bench_edge_prop_codegraph_read(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_codegraph_read");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_code_graph_schema()).unwrap();
  let file_count = env::var("KITE_BENCH_FILES")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(100);
  let chunks_per_file = env::var("KITE_BENCH_CHUNKS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(20);
  let symbols_per_file = env::var("KITE_BENCH_SYMBOLS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(120);
  let refs_per_chunk = env::var("KITE_BENCH_REFS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(15);
  let calls_per_chunk = env::var("KITE_BENCH_CALLS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(5);
  let imports_per_file = env::var("KITE_BENCH_IMPORTS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(4);
  let fixture = build_code_graph_fixture(
    &mut ray,
    file_count,
    chunks_per_file,
    symbols_per_file,
    refs_per_chunk,
    calls_per_chunk,
    imports_per_file,
  );
  apply_code_graph_edges(&mut ray, &fixture);

  let reference_edges = fixture.references;
  group.bench_function("get_existing", |bencher| {
    let mut i = 0usize;
    bencher.iter(|| {
      let (src, dst, _line, _role) = reference_edges[i % reference_edges.len()];
      let _ = black_box(ray.edge_prop(src, "REFERENCES", dst, "line"));
      i += 1;
    });
  });

  group.finish();
  ray.close().unwrap();
}
// =============================================================================
// Traversal Benchmarks
// =============================================================================

fn bench_neighbors_out(c: &mut Criterion) {
  let mut group = c.benchmark_group("traversal_neighbors_out");

  // Create a graph with varying out-degrees (smaller for faster setup)
  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  // Create edges: each node follows the next 10 nodes
  for i in 0..90 {
    for j in 1..=10 {
      ray.link(node_ids[i], "FOLLOWS", node_ids[i + j]).unwrap();
    }
  }

  group.bench_function("10_neighbors", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % 90];
      let _ = black_box(ray.neighbors_out(id, Some("FOLLOWS")));
      i += 1;
    });
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_edge_prop_codegraph_props_only(c: &mut Criterion) {
  let mut group = c.benchmark_group("edge_prop_codegraph_props_only");
  group.sample_size(10);

  let file_count = env::var("KITE_BENCH_FILES")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(100);
  let chunks_per_file = env::var("KITE_BENCH_CHUNKS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(20);
  let symbols_per_file = env::var("KITE_BENCH_SYMBOLS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(120);
  let refs_per_chunk = env::var("KITE_BENCH_REFS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(15);
  let calls_per_chunk = env::var("KITE_BENCH_CALLS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(5);
  let imports_per_file = env::var("KITE_BENCH_IMPORTS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(4);

  group.bench_function("set_props", |bencher| {
    bencher.iter_with_setup(
      || {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_code_graph_schema()).unwrap();

        let fixture = build_code_graph_fixture(
          &mut ray,
          file_count,
          chunks_per_file,
          symbols_per_file,
          refs_per_chunk,
          calls_per_chunk,
          imports_per_file,
        );

        for (src, dst, _) in fixture.contains.iter() {
          let _ = ray.link(*src, "CONTAINS", *dst);
        }
        for (src, dst, _, _) in fixture.references.iter() {
          let _ = ray.link(*src, "REFERENCES", *dst);
        }
        for (src, dst, _, _) in fixture.calls.iter() {
          let _ = ray.link(*src, "CALLS", *dst);
        }
        for (src, dst, _) in fixture.imports.iter() {
          let _ = ray.link(*src, "IMPORTS", *dst);
        }

        (temp_dir, ray, fixture)
      },
      |(_temp_dir, mut ray, fixture)| {
        for (src, dst, order) in fixture.contains.iter() {
          let _ = ray.set_edge_prop(*src, "CONTAINS", *dst, "order", PropValue::I64(*order));
        }

        for (src, dst, line, role) in fixture.references.iter() {
          let _ = ray.set_edge_prop(*src, "REFERENCES", *dst, "line", PropValue::I64(*line));
          let _ = ray.set_edge_prop(*src, "REFERENCES", *dst, "role", PropValue::I64(*role));
        }

        for (src, dst, line, weight) in fixture.calls.iter() {
          let _ = ray.set_edge_prop(*src, "CALLS", *dst, "line", PropValue::I64(*line));
          let _ = ray.set_edge_prop(*src, "CALLS", *dst, "weight", PropValue::F64(*weight));
        }

        for (src, dst, line) in fixture.imports.iter() {
          let _ = ray.set_edge_prop(*src, "IMPORTS", *dst, "line", PropValue::I64(*line));
        }
      },
    );
  });

  group.finish();
}

fn bench_multi_hop_traversal(c: &mut Criterion) {
  let mut group = c.benchmark_group("traversal_multi_hop");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create a chain of 100 nodes (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  // Linear chain
  for i in 0..99 {
    ray.link(node_ids[i], "FOLLOWS", node_ids[i + 1]).unwrap();
  }

  // Benchmark single hop repeatedly (simulating multi-hop with manual iteration)
  group.bench_function("single_hop", |bencher| {
    bencher.iter(|| {
      let result = ray.from(node_ids[0]).out(Some("FOLLOWS")).unwrap().to_vec();
      black_box(result)
    });
  });

  // Benchmark 2-hop traversal using chained out() calls
  group.bench_function("two_hop", |bencher| {
    bencher.iter(|| {
      let result = ray
        .from(node_ids[0])
        .out(Some("FOLLOWS"))
        .unwrap()
        .out(Some("FOLLOWS"))
        .unwrap()
        .to_vec();
      black_box(result)
    });
  });

  // Benchmark 3-hop traversal
  group.bench_function("three_hop", |bencher| {
    bencher.iter(|| {
      let result = ray
        .from(node_ids[0])
        .out(Some("FOLLOWS"))
        .unwrap()
        .out(Some("FOLLOWS"))
        .unwrap()
        .out(Some("FOLLOWS"))
        .unwrap()
        .to_vec();
      black_box(result)
    });
  });

  group.finish();
  ray.close().unwrap();
}

// =============================================================================
// Property Benchmarks
// =============================================================================

fn bench_get_prop(c: &mut Criterion) {
  let mut group = c.benchmark_group("property_get");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create nodes with properties (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String(format!("User{i}")));
    props.insert("age".to_string(), PropValue::I64(i as i64));
    let node = ray.create_node("User", &format!("user{i}"), props).unwrap();
    node_ids.push(node.id());
  }

  group.bench_function("get_existing_prop", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % node_ids.len()];
      let _ = black_box(ray.prop(id, "name"));
      i += 1;
    });
  });

  group.bench_function("get_nonexistent_prop", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % node_ids.len()];
      let _ = black_box(ray.prop(id, "nonexistent"));
      i += 1;
    });
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_set_prop(c: &mut Criterion) {
  let mut group = c.benchmark_group("property_set");

  group.bench_function("set_string", |bencher| {
    bencher.iter_with_setup(
      || {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();
        let node = ray.create_node("User", "testuser", HashMap::new()).unwrap();
        (temp_dir, ray, node.id())
      },
      |(_temp_dir, mut ray, node_id)| {
        for i in 0..100 {
          let _ = black_box(ray.set_prop(node_id, "name", PropValue::String(format!("Name{i}"))));
        }
      },
    );
  });

  group.finish();
}

fn bench_set_prop_tx(c: &mut Criterion) {
  let mut group = c.benchmark_group("property_set_tx");

  group.bench_function("set_string_tx_100", |bencher| {
    bencher.iter_with_setup(
      || {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();
        let node = ray.create_node("User", "testuser", HashMap::new()).unwrap();
        (temp_dir, ray, node.id())
      },
      |(_temp_dir, mut ray, node_id)| {
        let _ = black_box(ray.transaction(|ctx| {
          for i in 0..100 {
            ctx.set_prop(node_id, "name", PropValue::String(format!("Name{i}")))?;
          }
          Ok(())
        }));
      },
    );
  });

  group.finish();
}

// =============================================================================
// Pathfinding Benchmarks
// =============================================================================

fn bench_shortest_path(c: &mut Criterion) {
  let mut group = c.benchmark_group("pathfinding_shortest");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create a grid-like graph (10x10 = 100 nodes)
  let grid_size = 10;
  let mut node_ids = Vec::new();
  for i in 0..(grid_size * grid_size) {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  // Connect grid horizontally and vertically
  for row in 0..grid_size {
    for col in 0..grid_size {
      let idx = row * grid_size + col;
      // Right neighbor
      if col < grid_size - 1 {
        ray
          .link(node_ids[idx], "FOLLOWS", node_ids[idx + 1])
          .unwrap();
      }
      // Down neighbor
      if row < grid_size - 1 {
        ray
          .link(node_ids[idx], "FOLLOWS", node_ids[idx + grid_size])
          .unwrap();
      }
    }
  }

  // Benchmark shortest path from corner to corner
  let start = node_ids[0];
  let end = node_ids[grid_size * grid_size - 1];

  group.bench_function("bfs_10x10_grid", |bencher| {
    bencher.iter(|| {
      let result = ray
        .shortest_path(start, end)
        .via("FOLLOWS")
        .unwrap()
        .find_bfs();
      black_box(result)
    });
  });

  group.finish();
  ray.close().unwrap();
}

// =============================================================================
// Count Benchmarks
// =============================================================================

fn bench_count(c: &mut Criterion) {
  let mut group = c.benchmark_group("count");

  let temp_dir = tempdir().unwrap();
  let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();

  // Create 1000 nodes and 5000 edges (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..1000 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id());
  }

  for i in 0..995 {
    for j in 1..=5 {
      ray.link(node_ids[i], "FOLLOWS", node_ids[i + j]).unwrap();
    }
  }

  group.bench_function("count_nodes", |bencher| {
    bencher.iter(|| black_box(ray.count_nodes()));
  });

  group.bench_function("count_edges", |bencher| {
    bencher.iter(|| black_box(ray.count_edges()));
  });

  group.finish();
  ray.close().unwrap();
}

fn bench_batch_create_node(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_create_batched");
  group.sample_size(20);

  for count in [10, 100, 1000].iter() {
    group.throughput(Throughput::Elements(*count as u64));

    group.bench_with_input(
      BenchmarkId::new("count", count),
      count,
      |bencher, &count| {
        let temp_dir = tempdir().unwrap();
        let mut ray = Kite::open(temp_db_path(&temp_dir), create_test_schema()).unwrap();
        let mut batch_num = 0;

        bencher.iter(|| {
          let ops: Vec<BatchOp> = (0..count)
            .map(|i| BatchOp::CreateNode {
              node_type: "User".to_string(),
              key_suffix: format!("batch{batch_num}_{i}"),
              props: HashMap::new(),
            })
            .collect();
          batch_num += 1;
          let _ = black_box(ray.batch(ops));
        });

        ray.close().unwrap();
      },
    );
  }

  group.finish();
}

criterion_group!(
  benches,
  bench_create_node,
  bench_batch_create_node,
  bench_get_node_by_key,
  bench_get_node_by_key_micro,
  bench_node_exists,
  bench_link,
  bench_has_edge,
  bench_set_edge_prop,
  bench_get_edge_prop,
  bench_edge_prop_codegraph_write,
  bench_edge_prop_codegraph_write_batched,
  bench_edge_prop_codegraph_read,
  bench_edge_prop_codegraph_props_only,
  bench_neighbors_out,
  bench_multi_hop_traversal,
  bench_get_prop,
  bench_set_prop,
  bench_set_prop_tx,
  bench_shortest_path,
  bench_count,
);
criterion_main!(benches);
