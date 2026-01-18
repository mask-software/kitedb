//! Benchmarks for graph operations
//!
//! Run with: cargo bench --bench graph

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::collections::HashMap;
use tempfile::tempdir;

extern crate raydb_core;
use raydb_core::api::ray::{BatchOp, EdgeDef, NodeDef, PropDef, Ray, RayOptions};
use raydb_core::types::PropValue;

fn create_test_schema() -> RayOptions {
  let user = NodeDef::new("User", "user:")
    .prop(PropDef::string("name"))
    .prop(PropDef::int("age"));

  let follows = EdgeDef::new("FOLLOWS");

  RayOptions::new().node(user).edge(follows)
}

// =============================================================================
// Node CRUD Benchmarks
// =============================================================================

fn bench_create_node(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_create");
  group.sample_size(10); // Reduce sample size for expensive operations

  for count in [100, 500, 1000].iter() {
    group.throughput(Throughput::Elements(*count as u64));

    group.bench_with_input(BenchmarkId::new("count", count), count, |bencher, &count| {
      bencher.iter_with_setup(
        || {
          let temp_dir = tempdir().unwrap();
          let ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();
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
    });
  }

  group.finish();
}

fn bench_get_node_by_key(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_get_by_key");

  // Setup: Create database with nodes
  let temp_dir = tempdir().unwrap();
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

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

fn bench_node_exists(c: &mut Criterion) {
  let mut group = c.benchmark_group("node_exists");

  let temp_dir = tempdir().unwrap();
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create nodes and store IDs
  let mut node_ids = Vec::new();
  for i in 0..1000 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
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
            let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

            // Create nodes first
            let node_count = ((edge_count as f64).sqrt() as usize).max(10);
            let mut node_ids = Vec::new();
            for i in 0..node_count {
              let node = ray
                .create_node("User", &format!("user{i}"), HashMap::new())
                .unwrap();
              node_ids.push(node.id);
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
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create nodes and edges
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
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
// Traversal Benchmarks
// =============================================================================

fn bench_neighbors_out(c: &mut Criterion) {
  let mut group = c.benchmark_group("traversal_neighbors_out");

  // Create a graph with varying out-degrees (smaller for faster setup)
  let temp_dir = tempdir().unwrap();
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
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

fn bench_multi_hop_traversal(c: &mut Criterion) {
  let mut group = c.benchmark_group("traversal_multi_hop");

  let temp_dir = tempdir().unwrap();
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create a chain of 100 nodes (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
  }

  // Linear chain
  for i in 0..99 {
    ray.link(node_ids[i], "FOLLOWS", node_ids[i + 1]).unwrap();
  }

  // Benchmark single hop repeatedly (simulating multi-hop with manual iteration)
  group.bench_function("single_hop", |bencher| {
    bencher.iter(|| {
      let result = ray
        .from(node_ids[0])
        .out(Some("FOLLOWS"))
        .unwrap()
        .to_vec();
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
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create nodes with properties (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..100 {
    let mut props = HashMap::new();
    props.insert("name".to_string(), PropValue::String(format!("User{i}")));
    props.insert("age".to_string(), PropValue::I64(i as i64));
    let node = ray.create_node("User", &format!("user{i}"), props).unwrap();
    node_ids.push(node.id);
  }

  group.bench_function("get_existing_prop", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % node_ids.len()];
      let _ = black_box(ray.get_prop(id, "name"));
      i += 1;
    });
  });

  group.bench_function("get_nonexistent_prop", |bencher| {
    let mut i = 0;
    bencher.iter(|| {
      let id = node_ids[i % node_ids.len()];
      let _ = black_box(ray.get_prop(id, "nonexistent"));
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
        let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();
        let node = ray
          .create_node("User", "testuser", HashMap::new())
          .unwrap();
        (temp_dir, ray, node.id)
      },
      |(_temp_dir, mut ray, node_id)| {
        for i in 0..100 {
          let _ = black_box(ray.set_prop(
            node_id,
            "name",
            PropValue::String(format!("Name{i}")),
          ));
        }
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
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create a grid-like graph (10x10 = 100 nodes)
  let grid_size = 10;
  let mut node_ids = Vec::new();
  for i in 0..(grid_size * grid_size) {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
  }

  // Connect grid horizontally and vertically
  for row in 0..grid_size {
    for col in 0..grid_size {
      let idx = row * grid_size + col;
      // Right neighbor
      if col < grid_size - 1 {
        ray.link(node_ids[idx], "FOLLOWS", node_ids[idx + 1]).unwrap();
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
      let result = ray.shortest_path(start, end)
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
  let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();

  // Create 1000 nodes and 5000 edges (smaller for faster setup)
  let mut node_ids = Vec::new();
  for i in 0..1000 {
    let node = ray
      .create_node("User", &format!("user{i}"), HashMap::new())
      .unwrap();
    node_ids.push(node.id);
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

    group.bench_with_input(BenchmarkId::new("count", count), count, |bencher, &count| {
      let temp_dir = tempdir().unwrap();
      let mut ray = Ray::open(temp_dir.path(), create_test_schema()).unwrap();
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
    });
  }

  group.finish();
}

criterion_group!(
  benches,
  bench_create_node,
  bench_batch_create_node,
  bench_get_node_by_key,
  bench_node_exists,
  bench_link,
  bench_has_edge,
  bench_neighbors_out,
  bench_multi_hop_traversal,
  bench_get_prop,
  bench_set_prop,
  bench_shortest_path,
  bench_count,
);
criterion_main!(benches);
