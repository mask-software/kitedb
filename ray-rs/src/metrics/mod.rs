//! Metrics and health checks.
//!
//! Core implementation used by bindings.

use std::time::SystemTime;

use crate::cache::manager::CacheManagerStats;
use crate::core::single_file::SingleFileDB;
use crate::graph::db::GraphDB;
use crate::graph::iterators::{count_edges as graph_count_edges, count_nodes as graph_count_nodes};
use crate::types::{DbStats, DeltaState, MvccStats};

/// Cache layer metrics
#[derive(Debug, Clone)]
pub struct CacheLayerMetrics {
  pub hits: i64,
  pub misses: i64,
  pub hit_rate: f64,
  pub size: i64,
  pub max_size: i64,
  pub utilization_percent: f64,
}

/// Cache metrics
#[derive(Debug, Clone)]
pub struct CacheMetrics {
  pub enabled: bool,
  pub property_cache: CacheLayerMetrics,
  pub traversal_cache: CacheLayerMetrics,
  pub query_cache: CacheLayerMetrics,
}

/// Data metrics
#[derive(Debug, Clone)]
pub struct DataMetrics {
  pub node_count: i64,
  pub edge_count: i64,
  pub delta_nodes_created: i64,
  pub delta_nodes_deleted: i64,
  pub delta_edges_added: i64,
  pub delta_edges_deleted: i64,
  pub snapshot_generation: i64,
  pub max_node_id: i64,
  pub schema_labels: i64,
  pub schema_etypes: i64,
  pub schema_prop_keys: i64,
}

/// MVCC metrics
#[derive(Debug, Clone)]
pub struct MvccMetrics {
  pub enabled: bool,
  pub active_transactions: i64,
  pub versions_pruned: i64,
  pub gc_runs: i64,
  pub min_active_timestamp: i64,
  pub committed_writes_size: i64,
  pub committed_writes_pruned: i64,
}

/// Memory metrics
#[derive(Debug, Clone)]
pub struct MemoryMetrics {
  pub delta_estimate_bytes: i64,
  pub cache_estimate_bytes: i64,
  pub snapshot_bytes: i64,
  pub total_estimate_bytes: i64,
}

/// Database metrics
#[derive(Debug, Clone)]
pub struct DatabaseMetrics {
  pub path: String,
  pub is_single_file: bool,
  pub read_only: bool,
  pub data: DataMetrics,
  pub cache: CacheMetrics,
  pub mvcc: Option<MvccMetrics>,
  pub memory: MemoryMetrics,
  pub collected_at_ms: i64,
}

/// Health check entry
#[derive(Debug, Clone)]
pub struct HealthCheckEntry {
  pub name: String,
  pub passed: bool,
  pub message: String,
}

/// Health check result
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
  pub healthy: bool,
  pub checks: Vec<HealthCheckEntry>,
}

pub fn collect_metrics_single_file(db: &SingleFileDB) -> DatabaseMetrics {
  let stats = db.stats();
  let delta = db.delta.read();
  let cache_stats = db.cache.read().as_ref().map(|cache| cache.stats());

  let node_count = stats.snapshot_nodes as i64 + stats.delta_nodes_created as i64
    - stats.delta_nodes_deleted as i64;
  let edge_count =
    stats.snapshot_edges as i64 + stats.delta_edges_added as i64 - stats.delta_edges_deleted as i64;

  let data = DataMetrics {
    node_count,
    edge_count,
    delta_nodes_created: stats.delta_nodes_created as i64,
    delta_nodes_deleted: stats.delta_nodes_deleted as i64,
    delta_edges_added: stats.delta_edges_added as i64,
    delta_edges_deleted: stats.delta_edges_deleted as i64,
    snapshot_generation: stats.snapshot_gen as i64,
    max_node_id: stats.snapshot_max_node_id as i64,
    schema_labels: delta.new_labels.len() as i64,
    schema_etypes: delta.new_etypes.len() as i64,
    schema_prop_keys: delta.new_propkeys.len() as i64,
  };

  let cache = build_cache_metrics(cache_stats.as_ref());
  let delta_bytes = estimate_delta_memory(&delta);
  let cache_bytes = estimate_cache_memory(cache_stats.as_ref());
  let snapshot_bytes = (stats.snapshot_nodes as i64 * 50) + (stats.snapshot_edges as i64 * 20);

  DatabaseMetrics {
    path: db.path.to_string_lossy().to_string(),
    is_single_file: true,
    read_only: db.read_only,
    data,
    cache,
    mvcc: None,
    memory: MemoryMetrics {
      delta_estimate_bytes: delta_bytes,
      cache_estimate_bytes: cache_bytes,
      snapshot_bytes,
      total_estimate_bytes: delta_bytes + cache_bytes + snapshot_bytes,
    },
    collected_at_ms: system_time_to_millis(SystemTime::now()),
  }
}

pub fn collect_metrics_graph(db: &GraphDB) -> DatabaseMetrics {
  let stats = graph_stats(db);
  let delta = db.delta.read();

  let node_count = stats.snapshot_nodes as i64 + stats.delta_nodes_created as i64
    - stats.delta_nodes_deleted as i64;
  let edge_count =
    stats.snapshot_edges as i64 + stats.delta_edges_added as i64 - stats.delta_edges_deleted as i64;

  let data = DataMetrics {
    node_count,
    edge_count,
    delta_nodes_created: stats.delta_nodes_created as i64,
    delta_nodes_deleted: stats.delta_nodes_deleted as i64,
    delta_edges_added: stats.delta_edges_added as i64,
    delta_edges_deleted: stats.delta_edges_deleted as i64,
    snapshot_generation: stats.snapshot_gen as i64,
    max_node_id: stats.snapshot_max_node_id as i64,
    schema_labels: delta.new_labels.len() as i64,
    schema_etypes: delta.new_etypes.len() as i64,
    schema_prop_keys: delta.new_propkeys.len() as i64,
  };

  let cache = build_cache_metrics(None);
  let delta_bytes = estimate_delta_memory(&delta);
  let snapshot_bytes = (stats.snapshot_nodes as i64 * 50) + (stats.snapshot_edges as i64 * 20);

  let mvcc = db.mvcc.as_ref().map(|mvcc| {
    let tx_mgr = mvcc.tx_manager.lock();
    let gc = mvcc.gc.lock();
    let gc_stats = gc.get_stats();
    let committed_stats = tx_mgr.get_committed_writes_stats();
    MvccMetrics {
      enabled: true,
      active_transactions: tx_mgr.get_active_count() as i64,
      versions_pruned: gc_stats.versions_pruned as i64,
      gc_runs: gc_stats.gc_runs as i64,
      min_active_timestamp: tx_mgr.min_active_ts() as i64,
      committed_writes_size: committed_stats.size as i64,
      committed_writes_pruned: committed_stats.pruned as i64,
    }
  });

  DatabaseMetrics {
    path: db.path.to_string_lossy().to_string(),
    is_single_file: false,
    read_only: db.read_only,
    data,
    cache,
    mvcc,
    memory: MemoryMetrics {
      delta_estimate_bytes: delta_bytes,
      cache_estimate_bytes: 0,
      snapshot_bytes,
      total_estimate_bytes: delta_bytes + snapshot_bytes,
    },
    collected_at_ms: system_time_to_millis(SystemTime::now()),
  }
}

pub fn health_check_single_file(db: &SingleFileDB) -> HealthCheckResult {
  let mut checks = Vec::new();

  checks.push(HealthCheckEntry {
    name: "database_open".to_string(),
    passed: true,
    message: "Database handle is valid".to_string(),
  });

  let delta = db.delta.read();
  let delta_size = delta_health_size(&delta);
  let delta_ok = delta_size < 100000;
  checks.push(HealthCheckEntry {
    name: "delta_size".to_string(),
    passed: delta_ok,
    message: if delta_ok {
      format!("Delta size is reasonable ({delta_size} entries)")
    } else {
      format!("Delta is large ({delta_size} entries) - consider checkpointing")
    },
  });

  let cache_stats = db.cache.read().as_ref().map(|cache| cache.stats());
  if let Some(stats) = cache_stats {
    let total_hits = stats.property_cache_hits + stats.traversal_cache_hits;
    let total_misses = stats.property_cache_misses + stats.traversal_cache_misses;
    let total = total_hits + total_misses;
    let hit_rate = if total > 0 {
      total_hits as f64 / total as f64
    } else {
      1.0
    };
    let cache_ok = hit_rate > 0.5 || total < 100;
    checks.push(HealthCheckEntry {
      name: "cache_efficiency".to_string(),
      passed: cache_ok,
      message: if cache_ok {
        format!("Cache hit rate: {:.1}%", hit_rate * 100.0)
      } else {
        format!(
          "Low cache hit rate: {:.1}% - consider adjusting cache size",
          hit_rate * 100.0
        )
      },
    });
  }

  if db.read_only {
    checks.push(HealthCheckEntry {
      name: "write_access".to_string(),
      passed: true,
      message: "Database is read-only".to_string(),
    });
  }

  let healthy = checks.iter().all(|check| check.passed);
  HealthCheckResult { healthy, checks }
}

pub fn health_check_graph(db: &GraphDB) -> HealthCheckResult {
  let mut checks = Vec::new();

  checks.push(HealthCheckEntry {
    name: "database_open".to_string(),
    passed: true,
    message: "Database handle is valid".to_string(),
  });

  let delta = db.delta.read();
  let delta_size = delta_health_size(&delta);
  let delta_ok = delta_size < 100000;
  checks.push(HealthCheckEntry {
    name: "delta_size".to_string(),
    passed: delta_ok,
    message: if delta_ok {
      format!("Delta size is reasonable ({delta_size} entries)")
    } else {
      format!("Delta is large ({delta_size} entries) - consider checkpointing")
    },
  });

  if db.read_only {
    checks.push(HealthCheckEntry {
      name: "write_access".to_string(),
      passed: true,
      message: "Database is read-only".to_string(),
    });
  }

  let healthy = checks.iter().all(|check| check.passed);
  HealthCheckResult { healthy, checks }
}

fn calc_hit_rate(hits: u64, misses: u64) -> f64 {
  let total = hits + misses;
  if total > 0 {
    hits as f64 / total as f64
  } else {
    0.0
  }
}

fn build_cache_metrics(stats: Option<&CacheManagerStats>) -> CacheMetrics {
  if let Some(stats) = stats {
    CacheMetrics {
      enabled: true,
      property_cache: build_cache_layer_metrics(
        stats.property_cache_hits,
        stats.property_cache_misses,
        stats.property_cache_size,
        stats.property_cache_max_size,
      ),
      traversal_cache: build_cache_layer_metrics(
        stats.traversal_cache_hits,
        stats.traversal_cache_misses,
        stats.traversal_cache_size,
        stats.traversal_cache_max_size,
      ),
      query_cache: build_cache_layer_metrics(
        stats.query_cache_hits,
        stats.query_cache_misses,
        stats.query_cache_size,
        stats.query_cache_max_size,
      ),
    }
  } else {
    let empty = CacheLayerMetrics {
      hits: 0,
      misses: 0,
      hit_rate: 0.0,
      size: 0,
      max_size: 0,
      utilization_percent: 0.0,
    };
    CacheMetrics {
      enabled: false,
      property_cache: empty.clone(),
      traversal_cache: empty.clone(),
      query_cache: empty,
    }
  }
}

fn build_cache_layer_metrics(
  hits: u64,
  misses: u64,
  size: usize,
  max_size: usize,
) -> CacheLayerMetrics {
  let hit_rate = calc_hit_rate(hits, misses);
  let utilization_percent = if max_size > 0 {
    (size as f64 / max_size as f64) * 100.0
  } else {
    0.0
  };

  CacheLayerMetrics {
    hits: hits as i64,
    misses: misses as i64,
    hit_rate,
    size: size as i64,
    max_size: max_size as i64,
    utilization_percent,
  }
}

fn estimate_delta_memory(delta: &DeltaState) -> i64 {
  let mut bytes = 0i64;

  bytes += delta.created_nodes.len() as i64 * 100;
  bytes += delta.deleted_nodes.len() as i64 * 8;
  bytes += delta.modified_nodes.len() as i64 * 100;

  for patches in delta.out_add.values() {
    bytes += patches.len() as i64 * 24;
  }
  for patches in delta.out_del.values() {
    bytes += patches.len() as i64 * 24;
  }
  for patches in delta.in_add.values() {
    bytes += patches.len() as i64 * 24;
  }
  for patches in delta.in_del.values() {
    bytes += patches.len() as i64 * 24;
  }

  bytes += delta.edge_props.len() as i64 * 50;
  bytes += delta.key_index.len() as i64 * 40;

  bytes
}

fn estimate_cache_memory(stats: Option<&CacheManagerStats>) -> i64 {
  match stats {
    Some(stats) => {
      (stats.property_cache_size as i64 * 100)
        + (stats.traversal_cache_size as i64 * 200)
        + (stats.query_cache_size as i64 * 500)
    }
    None => 0,
  }
}

fn delta_health_size(delta: &DeltaState) -> usize {
  delta.created_nodes.len()
    + delta.deleted_nodes.len()
    + delta.modified_nodes.len()
    + delta.out_add.len()
    + delta.in_add.len()
}

fn graph_stats(db: &GraphDB) -> DbStats {
  let node_count = graph_count_nodes(db);
  let edge_count = graph_count_edges(db, None);

  let delta = db.delta.read();
  let delta_nodes_created = delta.created_nodes.len();
  let delta_nodes_deleted = delta.deleted_nodes.len();
  let delta_edges_added = delta.total_edges_added();
  let delta_edges_deleted = delta.total_edges_deleted();
  drop(delta);

  let (snapshot_gen, snapshot_nodes, snapshot_edges, snapshot_max_node_id) =
    if let Some(ref snapshot) = db.snapshot {
      (
        snapshot.header.generation,
        snapshot.header.num_nodes,
        snapshot.header.num_edges,
        snapshot.header.max_node_id,
      )
    } else {
      (0, 0, 0, 0)
    };

  let total_changes =
    delta_nodes_created + delta_nodes_deleted + delta_edges_added + delta_edges_deleted;
  let recommend_compact = total_changes > 10_000;

  let mvcc_stats = db.mvcc.as_ref().map(|mvcc| {
    let tx_mgr = mvcc.tx_manager.lock();
    let gc = mvcc.gc.lock();
    let gc_stats = gc.get_stats();
    let committed_stats = tx_mgr.get_committed_writes_stats();
    MvccStats {
      active_transactions: tx_mgr.get_active_count(),
      min_active_ts: tx_mgr.min_active_ts(),
      versions_pruned: gc_stats.versions_pruned,
      gc_runs: gc_stats.gc_runs,
      last_gc_time: gc_stats.last_gc_time,
      committed_writes_size: committed_stats.size,
      committed_writes_pruned: committed_stats.pruned,
    }
  });

  DbStats {
    snapshot_gen,
    snapshot_nodes: snapshot_nodes.max(node_count),
    snapshot_edges: snapshot_edges.max(edge_count),
    snapshot_max_node_id,
    delta_nodes_created,
    delta_nodes_deleted,
    delta_edges_added,
    delta_edges_deleted,
    wal_segment: 0,
    wal_bytes: db.wal_bytes(),
    recommend_compact,
    mvcc_stats,
  }
}

fn system_time_to_millis(time: SystemTime) -> i64 {
  time
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as i64
}
