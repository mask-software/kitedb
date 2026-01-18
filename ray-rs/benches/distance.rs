//! Benchmarks for distance functions
//!
//! Run with: cargo bench --bench distance

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

// Import from the crate being built
extern crate raydb_core;
use raydb_core::vector::{
  batch_cosine_distance, batch_squared_euclidean, dot_product, l2_norm, normalize,
  squared_euclidean,
};

fn generate_vectors(dimensions: usize, _count: usize) -> (Vec<f32>, Vec<f32>) {
  let a: Vec<f32> = (0..dimensions).map(|i| (i as f32 * 0.01) % 1.0).collect();
  let b: Vec<f32> = (0..dimensions).map(|i| ((dimensions - i) as f32 * 0.01) % 1.0).collect();
  (a, b)
}

fn generate_row_group(dimensions: usize, count: usize) -> Vec<f32> {
  (0..dimensions * count)
    .map(|i| (i as f32 * 0.001) % 1.0)
    .collect()
}

fn bench_dot_product(c: &mut Criterion) {
  let mut group = c.benchmark_group("dot_product");
  
  for dims in [128, 256, 384, 768, 1536].iter() {
    let (a, b) = generate_vectors(*dims, 1);
    
    group.bench_with_input(BenchmarkId::new("dims", dims), dims, |bencher, _| {
      bencher.iter(|| dot_product(black_box(&a), black_box(&b)));
    });
  }
  
  group.finish();
}

fn bench_squared_euclidean(c: &mut Criterion) {
  let mut group = c.benchmark_group("squared_euclidean");
  
  for dims in [128, 256, 384, 768, 1536].iter() {
    let (a, b) = generate_vectors(*dims, 1);
    
    group.bench_with_input(BenchmarkId::new("dims", dims), dims, |bencher, _| {
      bencher.iter(|| squared_euclidean(black_box(&a), black_box(&b)));
    });
  }
  
  group.finish();
}

fn bench_l2_norm(c: &mut Criterion) {
  let mut group = c.benchmark_group("l2_norm");
  
  for dims in [128, 256, 384, 768, 1536].iter() {
    let (a, _) = generate_vectors(*dims, 1);
    
    group.bench_with_input(BenchmarkId::new("dims", dims), dims, |bencher, _| {
      bencher.iter(|| l2_norm(black_box(&a)));
    });
  }
  
  group.finish();
}

fn bench_normalize(c: &mut Criterion) {
  let mut group = c.benchmark_group("normalize");
  
  for dims in [128, 256, 384, 768, 1536].iter() {
    let (a, _) = generate_vectors(*dims, 1);
    
    group.bench_with_input(BenchmarkId::new("dims", dims), dims, |bencher, _| {
      bencher.iter(|| normalize(black_box(&a)));
    });
  }
  
  group.finish();
}

fn bench_batch_cosine(c: &mut Criterion) {
  let mut group = c.benchmark_group("batch_cosine_distance");
  
  let dims = 384;
  let query = generate_vectors(dims, 1).0;
  
  for count in [100, 500, 1000].iter() {
    let row_group = generate_row_group(dims, *count);
    
    group.bench_with_input(BenchmarkId::new("count", count), count, |bencher, _| {
      bencher.iter(|| batch_cosine_distance(
        black_box(&query),
        black_box(&row_group),
        dims,
        0,
        *count,
      ));
    });
  }
  
  group.finish();
}

fn bench_batch_euclidean(c: &mut Criterion) {
  let mut group = c.benchmark_group("batch_squared_euclidean");
  
  let dims = 384;
  let query = generate_vectors(dims, 1).0;
  
  for count in [100, 500, 1000].iter() {
    let row_group = generate_row_group(dims, *count);
    
    group.bench_with_input(BenchmarkId::new("count", count), count, |bencher, _| {
      bencher.iter(|| batch_squared_euclidean(
        black_box(&query),
        black_box(&row_group),
        dims,
        0,
        *count,
      ));
    });
  }
  
  group.finish();
}

criterion_group!(
  benches,
  bench_dot_product,
  bench_squared_euclidean,
  bench_l2_norm,
  bench_normalize,
  bench_batch_cosine,
  bench_batch_euclidean,
);
criterion_main!(benches);
