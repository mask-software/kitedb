//! Vector Compaction Strategy Benchmark (Rust)
//!
//! Evaluates vector fragment compaction behavior for a given workload shape.
//!
//! Usage:
//!   cargo run --release --example vector_compaction_bench --no-default-features -- [options]
//!
//! Options:
//!   --vectors N                   Number of vectors to insert (default: 50000)
//!   --dimensions D                Vector dimensions (default: 384)
//!   --fragment-target-size N      Vectors per fragment before seal (default: 5000)
//!   --delete-ratio R              Ratio of vectors to delete [0..1] (default: 0.35)
//!   --min-deletion-ratio R        Compaction min deletion ratio (default: 0.30)
//!   --max-fragments N             Max fragments per compaction run (default: 4)
//!   --min-vectors-to-compact N    Min live vectors required for compaction (default: 10000)
//!   --seed N                      RNG seed (default: 42)
//!
//! Unknown options are an error. The bench drives the vector store directly
//! and opens no database, so it has no MVCC mode.

use kitedb::types::NodeId;
use kitedb::vector::compaction::{
  clear_deleted_fragments, compaction_stats, find_fragments_to_compact, run_compaction_if_needed,
  CompactionStrategy,
};
use kitedb::vector::{
  create_vector_store, vector_store_delete, vector_store_insert, vector_store_seal_active,
  vector_store_stats, DistanceMetric, VectorStoreConfig,
};
use rand::{rngs::StdRng, seq::SliceRandom, Rng, SeedableRng};
use std::env;
use std::process::exit;
use std::time::Instant;

#[derive(Debug, Clone)]
struct BenchConfig {
  vectors: usize,
  dimensions: usize,
  fragment_target_size: usize,
  delete_ratio: f32,
  strategy: CompactionStrategy,
  seed: u64,
}

impl Default for BenchConfig {
  fn default() -> Self {
    Self {
      vectors: 50_000,
      dimensions: 384,
      fragment_target_size: 5_000,
      delete_ratio: 0.35,
      strategy: CompactionStrategy::default(),
      seed: 42,
    }
  }
}

fn usage_error(message: &str) -> ! {
  eprintln!("error: {message}");
  eprintln!("see the header of examples/vector_compaction_bench.rs for the options");
  exit(2);
}

fn value<T: std::str::FromStr>(args: &[String], i: &mut usize, flag: &str) -> T {
  *i += 1;
  let Some(raw) = args.get(*i) else {
    usage_error(&format!("{flag} needs a value"));
  };
  raw
    .parse()
    .unwrap_or_else(|_| usage_error(&format!("invalid value for {flag}: {raw}")))
}

fn parse_args() -> BenchConfig {
  let mut config = BenchConfig::default();
  let args: Vec<String> = env::args().collect();
  let mut i = 1usize;

  while i < args.len() {
    let flag = args[i].as_str();
    match flag {
      "--vectors" => config.vectors = value(&args, &mut i, flag),
      "--dimensions" => config.dimensions = value(&args, &mut i, flag),
      "--fragment-target-size" => config.fragment_target_size = value(&args, &mut i, flag),
      "--delete-ratio" => config.delete_ratio = value(&args, &mut i, flag),
      "--min-deletion-ratio" => config.strategy.min_deletion_ratio = value(&args, &mut i, flag),
      "--max-fragments" => {
        config.strategy.max_fragments_per_compaction = value(&args, &mut i, flag)
      }
      "--min-vectors-to-compact" => {
        config.strategy.min_vectors_to_compact = value(&args, &mut i, flag)
      }
      "--seed" => config.seed = value(&args, &mut i, flag),
      other => usage_error(&format!("unknown option {other}")),
    }
    i += 1;
  }

  config.delete_ratio = config.delete_ratio.clamp(0.0, 1.0);
  config.vectors = config.vectors.max(1);
  config.dimensions = config.dimensions.max(1);
  config.fragment_target_size = config.fragment_target_size.max(1);
  config.strategy.max_fragments_per_compaction =
    config.strategy.max_fragments_per_compaction.max(1);

  config
}

fn random_vector(rng: &mut StdRng, dims: usize) -> Vec<f32> {
  let mut vector = vec![0.0f32; dims];
  for value in &mut vector {
    *value = rng.gen_range(-1.0f32..1.0f32);
  }
  vector
}

fn format_number(n: usize) -> String {
  let mut s = n.to_string();
  let mut i = s.len() as isize - 3;
  while i > 0 {
    s.insert(i as usize, ',');
    i -= 3;
  }
  s
}

fn format_ratio(ratio: f32) -> String {
  format!("{:.2}%", ratio * 100.0)
}

fn main() {
  let config = parse_args();
  let mut rng = StdRng::seed_from_u64(config.seed);

  println!("{}", "=".repeat(100));
  println!("Vector Compaction Strategy Benchmark (Rust)");
  println!("{}", "=".repeat(100));
  println!("vectors: {}", format_number(config.vectors));
  println!("dimensions: {}", config.dimensions);
  println!(
    "fragment_target_size: {}",
    format_number(config.fragment_target_size)
  );
  println!("delete_ratio: {}", format_ratio(config.delete_ratio));
  println!(
    "strategy: min_deletion_ratio={}, max_fragments={}, min_vectors_to_compact={}",
    config.strategy.min_deletion_ratio,
    config.strategy.max_fragments_per_compaction,
    format_number(config.strategy.min_vectors_to_compact)
  );
  println!("{}", "=".repeat(100));

  let store_config = VectorStoreConfig::new(config.dimensions)
    .with_metric(DistanceMetric::Cosine)
    .with_fragment_target_size(config.fragment_target_size);
  let mut manifest = create_vector_store(store_config);

  let insert_start = Instant::now();
  for node_id in 0..config.vectors {
    let vector = random_vector(&mut rng, config.dimensions);
    vector_store_insert(&mut manifest, node_id as NodeId, &vector).expect("vector insert failed");
  }
  vector_store_seal_active(&mut manifest);
  let insert_elapsed = insert_start.elapsed();

  let mut ids: Vec<usize> = (0..config.vectors).collect();
  ids.shuffle(&mut rng);
  let delete_count = ((config.vectors as f32) * config.delete_ratio).round() as usize;
  let delete_start = Instant::now();
  let mut deleted = 0usize;
  for node_id in ids.iter().take(delete_count) {
    if vector_store_delete(&mut manifest, *node_id as NodeId) {
      deleted += 1;
    }
  }
  let delete_elapsed = delete_start.elapsed();

  let before_store = vector_store_stats(&manifest);
  let before_compaction = compaction_stats(&manifest);
  let candidate_ids = find_fragments_to_compact(&manifest, &config.strategy);

  let clear_start = Instant::now();
  let cleared_fragments = clear_deleted_fragments(&mut manifest);
  let clear_elapsed = clear_start.elapsed();

  let compact_start = Instant::now();
  let compacted = run_compaction_if_needed(&mut manifest, &config.strategy);
  let compact_elapsed = compact_start.elapsed();

  let after_store = vector_store_stats(&manifest);
  let after_compaction = compaction_stats(&manifest);

  println!(
    "insert_elapsed_ms: {:.2}",
    insert_elapsed.as_secs_f64() * 1000.0
  );
  println!(
    "insert_throughput_vectors_per_sec: {}",
    format_number((config.vectors as f64 / insert_elapsed.as_secs_f64()).round() as usize)
  );
  println!(
    "delete_elapsed_ms: {:.2}",
    delete_elapsed.as_secs_f64() * 1000.0
  );
  println!(
    "deleted_vectors: {} (requested {})",
    format_number(deleted),
    format_number(delete_count)
  );
  println!(
    "clear_deleted_elapsed_ms: {:.2}",
    clear_elapsed.as_secs_f64() * 1000.0
  );
  println!("cleared_fragments: {}", cleared_fragments);
  println!(
    "compaction_elapsed_ms: {:.2}",
    compact_elapsed.as_secs_f64() * 1000.0
  );
  println!("compaction_performed: {}", compacted);
  println!(
    "candidate_fragments_before: {} ({:?})",
    candidate_ids.len(),
    candidate_ids
  );

  println!("\nStore stats (before -> after):");
  println!(
    "  live_vectors: {} -> {}",
    format_number(before_store.live_vectors),
    format_number(after_store.live_vectors)
  );
  println!(
    "  total_deleted: {} -> {}",
    format_number(before_store.total_deleted),
    format_number(after_store.total_deleted)
  );
  println!(
    "  fragment_count: {} -> {}",
    before_store.fragment_count, after_store.fragment_count
  );
  println!(
    "  bytes_used: {} -> {}",
    format_number(before_store.bytes_used),
    format_number(after_store.bytes_used)
  );

  println!("\nCompaction stats (before -> after):");
  println!(
    "  fragments_needing_compaction: {} -> {}",
    before_compaction.fragments_needing_compaction, after_compaction.fragments_needing_compaction
  );
  println!(
    "  total_deleted_vectors: {} -> {}",
    format_number(before_compaction.total_deleted_vectors),
    format_number(after_compaction.total_deleted_vectors)
  );
  println!(
    "  average_deletion_ratio: {} -> {}",
    format_ratio(before_compaction.average_deletion_ratio),
    format_ratio(after_compaction.average_deletion_ratio)
  );
}
