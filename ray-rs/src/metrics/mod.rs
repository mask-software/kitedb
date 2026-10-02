//! Metrics and health checks.
//!
//! Core implementation used by bindings.

use std::time::SystemTime;

use serde_json::{json, Value};

use crate::core::single_file::SingleFileDB;
use crate::replication::primary::PrimaryReplicationStatus;
use crate::replication::replica::ReplicaReplicationStatus;
use crate::types::DeltaState;

// OTLP push needs sockets: wasm32 builds leave it out (its dependencies are
// native-only in Cargo.toml).
#[cfg(all(feature = "otlp", not(target_arch = "wasm32")))]
mod otlp;
#[cfg(all(feature = "otlp", not(target_arch = "wasm32")))]
pub use otlp::*;

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

/// Primary replication metrics
#[derive(Debug, Clone)]
pub struct PrimaryReplicationMetrics {
  pub epoch: i64,
  pub head_log_index: i64,
  pub retained_floor: i64,
  pub replica_count: i64,
  pub stale_epoch_replica_count: i64,
  pub max_replica_lag: i64,
  pub min_replica_applied_log_index: Option<i64>,
  pub sidecar_path: String,
  pub last_token: Option<String>,
  pub last_replication_error: Option<String>,
  pub sidecar_needs_repair: bool,
  pub append_attempts: i64,
  pub append_failures: i64,
  pub append_successes: i64,
}

/// Replica replication metrics
#[derive(Debug, Clone)]
pub struct ReplicaReplicationMetrics {
  pub applied_epoch: i64,
  pub applied_log_index: i64,
  pub needs_reseed: bool,
  pub last_error: Option<String>,
}

/// Replication metrics
#[derive(Debug, Clone)]
pub struct ReplicationMetrics {
  pub enabled: bool,
  pub role: String,
  pub primary: Option<PrimaryReplicationMetrics>,
  pub replica: Option<ReplicaReplicationMetrics>,
}

/// Memory metrics
#[derive(Debug, Clone)]
pub struct MemoryMetrics {
  pub delta_estimate_bytes: i64,
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
  pub mvcc: Option<MvccMetrics>,
  pub replication: ReplicationMetrics,
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
  // Every committed definition: the delta's `new_*` maps lose whatever a
  // checkpoint moved into the snapshot.
  let schema_labels = db.label_ids.read().len() as i64;
  let schema_etypes = db.etype_ids.read().len() as i64;
  let schema_prop_keys = db.propkey_ids.read().len() as i64;
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
    schema_labels,
    schema_etypes,
    schema_prop_keys,
  };

  let replication = build_replication_metrics(
    db.primary_replication_status(),
    db.replica_replication_status(),
  );
  let delta_bytes = estimate_delta_memory(&delta);
  let snapshot_bytes = (stats.snapshot_nodes as i64 * 50) + (stats.snapshot_edges as i64 * 20);

  let mvcc = db.mvcc.as_ref().map(|mvcc| {
    let tx_mgr = mvcc.tx_manager.lock();
    let gc = mvcc.gc.lock();
    let gc_stats = gc.stats();
    let committed_stats = tx_mgr.committed_writes_stats();
    MvccMetrics {
      enabled: true,
      active_transactions: tx_mgr.active_count() as i64,
      versions_pruned: gc_stats.versions_pruned as i64,
      gc_runs: gc_stats.gc_runs as i64,
      min_active_timestamp: tx_mgr.min_active_ts() as i64,
      committed_writes_size: committed_stats.size as i64,
      committed_writes_pruned: committed_stats.pruned as i64,
    }
  });

  DatabaseMetrics {
    path: db.path.to_string_lossy().to_string(),
    is_single_file: true,
    read_only: db.read_only,
    data,
    mvcc,
    replication,
    memory: MemoryMetrics {
      delta_estimate_bytes: delta_bytes,
      snapshot_bytes,
      total_estimate_bytes: delta_bytes + snapshot_bytes,
    },
    collected_at_ms: system_time_to_millis(SystemTime::now()),
  }
}

/// Collect replication-only metrics and render them in Prometheus text format.
pub fn collect_replication_metrics_prometheus_single_file(db: &SingleFileDB) -> String {
  let metrics = collect_metrics_single_file(db);
  render_replication_metrics_prometheus(&metrics)
}

/// Collect replication-only metrics and render them as OTLP JSON payload.
pub fn collect_replication_metrics_otel_json_single_file(db: &SingleFileDB) -> String {
  let metrics = collect_metrics_single_file(db);
  render_replication_metrics_otel_json(&metrics)
}

/// Render replication metrics from a metrics snapshot using Prometheus exposition format.
pub fn render_replication_metrics_prometheus(metrics: &DatabaseMetrics) -> String {
  let mut lines = Vec::new();
  let role = metrics.replication.role.as_str();
  let enabled = if metrics.replication.enabled { 1 } else { 0 };

  push_prometheus_help(
    &mut lines,
    "kitedb_replication_enabled",
    "gauge",
    "Whether replication is enabled for this database (1 enabled, 0 disabled).",
  );
  push_prometheus_sample(
    &mut lines,
    "kitedb_replication_enabled",
    enabled,
    &[("role", role)],
  );

  // Host-runtime export path is process-local and does not enforce HTTP auth.
  push_prometheus_help(
    &mut lines,
    "kitedb_replication_auth_enabled",
    "gauge",
    "Whether replication admin auth is enabled for this metrics exporter.",
  );
  push_prometheus_sample(&mut lines, "kitedb_replication_auth_enabled", 0, &[]);

  if let Some(primary) = metrics.replication.primary.as_ref() {
    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_epoch",
      "gauge",
      "Current primary replication epoch.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_epoch",
      primary.epoch,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_head_log_index",
      "gauge",
      "Current primary head log index.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_head_log_index",
      primary.head_log_index,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_retained_floor",
      "gauge",
      "Current primary retained floor log index.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_retained_floor",
      primary.retained_floor,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_replica_count",
      "gauge",
      "Replica progress reporters known by this primary.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_replica_count",
      primary.replica_count,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_stale_epoch_replica_count",
      "gauge",
      "Replica reporters currently on stale epochs.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_stale_epoch_replica_count",
      primary.stale_epoch_replica_count,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_max_replica_lag",
      "gauge",
      "Maximum reported lag (log frames) across replicas.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_max_replica_lag",
      primary.max_replica_lag,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_sidecar_needs_repair",
      "gauge",
      "Whether the primary sidecar is fenced and requires repair or resync.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_sidecar_needs_repair",
      if primary.sidecar_needs_repair { 1 } else { 0 },
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_last_replication_error_present",
      "gauge",
      "Whether the primary currently has a non-empty replication error.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_last_replication_error_present",
      if primary.last_replication_error.is_some() {
        1
      } else {
        0
      },
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_append_attempts_total",
      "counter",
      "Total replication append attempts on the primary commit path.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_append_attempts_total",
      primary.append_attempts,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_append_failures_total",
      "counter",
      "Total replication append failures on the primary commit path.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_append_failures_total",
      primary.append_failures,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_primary_append_successes_total",
      "counter",
      "Total replication append successes on the primary commit path.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_primary_append_successes_total",
      primary.append_successes,
      &[],
    );
  }

  if let Some(replica) = metrics.replication.replica.as_ref() {
    push_prometheus_help(
      &mut lines,
      "kitedb_replication_replica_applied_epoch",
      "gauge",
      "Replica applied epoch.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_replica_applied_epoch",
      replica.applied_epoch,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_replica_applied_log_index",
      "gauge",
      "Replica applied log index.",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_replica_applied_log_index",
      replica.applied_log_index,
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_replica_needs_reseed",
      "gauge",
      "Whether replica currently requires snapshot reseed (1 yes, 0 no).",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_replica_needs_reseed",
      if replica.needs_reseed { 1 } else { 0 },
      &[],
    );

    push_prometheus_help(
      &mut lines,
      "kitedb_replication_replica_last_error_present",
      "gauge",
      "Whether replica currently has a non-empty last_error value (1 yes, 0 no).",
    );
    push_prometheus_sample(
      &mut lines,
      "kitedb_replication_replica_last_error_present",
      if replica.last_error.is_some() { 1 } else { 0 },
      &[],
    );
  }

  let mut text = lines.join("\n");
  text.push('\n');
  text
}

/// Render replication metrics in OpenTelemetry OTLP JSON format.
pub fn render_replication_metrics_otel_json(metrics: &DatabaseMetrics) -> String {
  let role = metrics.replication.role.as_str();
  let enabled = if metrics.replication.enabled { 1 } else { 0 };
  let time_unix_nano = metric_time_unix_nano(metrics);
  let mut otel_metrics: Vec<Value> = Vec::new();

  otel_metrics.push(otel_gauge_metric(
    "kitedb.replication.enabled",
    "Whether replication is enabled for this database (1 enabled, 0 disabled).",
    "1",
    enabled,
    &[("role", role)],
    &time_unix_nano,
  ));

  // Host-runtime export path is process-local and does not enforce HTTP auth.
  otel_metrics.push(otel_gauge_metric(
    "kitedb.replication.auth.enabled",
    "Whether replication admin auth is enabled for this metrics exporter.",
    "1",
    0,
    &[],
    &time_unix_nano,
  ));

  if let Some(primary) = metrics.replication.primary.as_ref() {
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.epoch",
      "Current primary replication epoch.",
      "1",
      primary.epoch,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.head_log_index",
      "Current primary head log index.",
      "1",
      primary.head_log_index,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.retained_floor",
      "Current primary retained floor log index.",
      "1",
      primary.retained_floor,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.replica_count",
      "Replica progress reporters known by this primary.",
      "1",
      primary.replica_count,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.stale_epoch_replica_count",
      "Replica reporters currently on stale epochs.",
      "1",
      primary.stale_epoch_replica_count,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.max_replica_lag",
      "Maximum reported lag (log frames) across replicas.",
      "1",
      primary.max_replica_lag,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.sidecar_needs_repair",
      "Whether the primary sidecar is fenced and requires repair or resync.",
      "1",
      if primary.sidecar_needs_repair { 1 } else { 0 },
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.primary.last_replication_error_present",
      "Whether the primary currently has a non-empty replication error.",
      "1",
      if primary.last_replication_error.is_some() {
        1
      } else {
        0
      },
      &[],
      &time_unix_nano,
    ));

    otel_metrics.push(otel_sum_metric(
      "kitedb.replication.primary.append_attempts",
      "Total replication append attempts on the primary commit path.",
      "1",
      primary.append_attempts,
      true,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_sum_metric(
      "kitedb.replication.primary.append_failures",
      "Total replication append failures on the primary commit path.",
      "1",
      primary.append_failures,
      true,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_sum_metric(
      "kitedb.replication.primary.append_successes",
      "Total replication append successes on the primary commit path.",
      "1",
      primary.append_successes,
      true,
      &[],
      &time_unix_nano,
    ));
  }

  if let Some(replica) = metrics.replication.replica.as_ref() {
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.replica.applied_epoch",
      "Replica applied epoch.",
      "1",
      replica.applied_epoch,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.replica.applied_log_index",
      "Replica applied log index.",
      "1",
      replica.applied_log_index,
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.replica.needs_reseed",
      "Whether replica currently requires snapshot reseed (1 yes, 0 no).",
      "1",
      if replica.needs_reseed { 1 } else { 0 },
      &[],
      &time_unix_nano,
    ));
    otel_metrics.push(otel_gauge_metric(
      "kitedb.replication.replica.last_error_present",
      "Whether replica currently has a non-empty last_error value (1 yes, 0 no).",
      "1",
      if replica.last_error.is_some() { 1 } else { 0 },
      &[],
      &time_unix_nano,
    ));
  }

  let payload = json!({
    "resourceMetrics": [
      {
        "resource": {
          "attributes": [
            otel_attr_string("service.name", "kitedb"),
            otel_attr_string("kitedb.database.path", metrics.path.as_str()),
            otel_attr_string("kitedb.metrics.scope", "replication"),
          ]
        },
        "scopeMetrics": [
          {
            "scope": {
              "name": "kitedb.metrics.replication",
              "version": env!("CARGO_PKG_VERSION"),
            },
            "metrics": otel_metrics,
          }
        ]
      }
    ]
  });

  serde_json::to_string(&payload).unwrap_or_else(|_| "{\"resourceMetrics\":[]}".to_string())
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

fn build_replication_metrics(
  primary: Option<PrimaryReplicationStatus>,
  replica: Option<ReplicaReplicationStatus>,
) -> ReplicationMetrics {
  let role = if primary.is_some() {
    "primary"
  } else if replica.is_some() {
    "replica"
  } else {
    "disabled"
  };

  ReplicationMetrics {
    enabled: role != "disabled",
    role: role.to_string(),
    primary: primary.map(build_primary_replication_metrics),
    replica: replica.map(build_replica_replication_metrics),
  }
}

fn build_primary_replication_metrics(
  status: PrimaryReplicationStatus,
) -> PrimaryReplicationMetrics {
  let mut max_replica_lag = 0u64;
  let mut min_replica_applied_log_index: Option<u64> = None;
  let mut stale_epoch_replica_count = 0u64;

  for lag in &status.replica_lags {
    if lag.epoch != status.epoch {
      stale_epoch_replica_count = stale_epoch_replica_count.saturating_add(1);
    }

    if lag.epoch == status.epoch {
      let lag_value = status.head_log_index.saturating_sub(lag.applied_log_index);
      max_replica_lag = max_replica_lag.max(lag_value);
      min_replica_applied_log_index = Some(match min_replica_applied_log_index {
        Some(current) => current.min(lag.applied_log_index),
        None => lag.applied_log_index,
      });
    } else if lag.epoch < status.epoch {
      max_replica_lag = max_replica_lag.max(status.head_log_index);
    }
  }

  PrimaryReplicationMetrics {
    epoch: status.epoch as i64,
    head_log_index: status.head_log_index as i64,
    retained_floor: status.retained_floor as i64,
    replica_count: status.replica_lags.len() as i64,
    stale_epoch_replica_count: stale_epoch_replica_count as i64,
    max_replica_lag: max_replica_lag as i64,
    min_replica_applied_log_index: min_replica_applied_log_index.map(|value| value as i64),
    sidecar_path: status.sidecar_path.to_string_lossy().to_string(),
    last_token: status.last_token.map(|token| token.to_string()),
    last_replication_error: status.last_replication_error,
    sidecar_needs_repair: status.sidecar_needs_repair,
    append_attempts: status.append_attempts as i64,
    append_failures: status.append_failures as i64,
    append_successes: status.append_successes as i64,
  }
}

fn build_replica_replication_metrics(
  status: ReplicaReplicationStatus,
) -> ReplicaReplicationMetrics {
  ReplicaReplicationMetrics {
    applied_epoch: status.applied_epoch as i64,
    applied_log_index: status.applied_log_index as i64,
    needs_reseed: status.needs_reseed,
    last_error: status.last_error,
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

fn delta_health_size(delta: &DeltaState) -> usize {
  delta.created_nodes.len()
    + delta.deleted_nodes.len()
    + delta.modified_nodes.len()
    + delta.out_add.len()
    + delta.in_add.len()
}

fn system_time_to_millis(time: SystemTime) -> i64 {
  time
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as i64
}

fn escape_prometheus_label_value(value: &str) -> String {
  value
    .replace('\\', "\\\\")
    .replace('"', "\\\"")
    .replace('\n', "\\n")
}

fn format_prometheus_labels(labels: &[(&str, &str)]) -> String {
  if labels.is_empty() {
    return String::new();
  }

  let rendered = labels
    .iter()
    .map(|(key, value)| format!("{key}=\"{}\"", escape_prometheus_label_value(value)))
    .collect::<Vec<_>>()
    .join(",");
  format!("{{{rendered}}}")
}

fn push_prometheus_help(lines: &mut Vec<String>, metric: &str, metric_type: &str, help: &str) {
  lines.push(format!("# HELP {metric} {help}"));
  lines.push(format!("# TYPE {metric} {metric_type}"));
}

fn push_prometheus_sample(
  lines: &mut Vec<String>,
  metric: &str,
  value: i64,
  labels: &[(&str, &str)],
) {
  lines.push(format!(
    "{metric}{} {value}",
    format_prometheus_labels(labels)
  ));
}

fn metric_time_unix_nano(metrics: &DatabaseMetrics) -> String {
  metric_time_unix_nano_u64(metrics).to_string()
}

fn metric_time_unix_nano_u64(metrics: &DatabaseMetrics) -> u64 {
  let millis = metrics.collected_at_ms.max(0) as u64;
  millis.saturating_mul(1_000_000)
}

fn otel_attr_string(key: &str, value: &str) -> Value {
  json!({
    "key": key,
    "value": { "stringValue": value }
  })
}

fn otel_attributes(labels: &[(&str, &str)]) -> Vec<Value> {
  labels
    .iter()
    .map(|(key, value)| otel_attr_string(key, value))
    .collect()
}

fn otel_gauge_metric(
  name: &str,
  description: &str,
  unit: &str,
  value: i64,
  labels: &[(&str, &str)],
  time_unix_nano: &str,
) -> Value {
  json!({
    "name": name,
    "description": description,
    "unit": unit,
    "gauge": {
      "dataPoints": [
        {
          "attributes": otel_attributes(labels),
          "asInt": value,
          "timeUnixNano": time_unix_nano,
        }
      ]
    }
  })
}

fn otel_sum_metric(
  name: &str,
  description: &str,
  unit: &str,
  value: i64,
  is_monotonic: bool,
  labels: &[(&str, &str)],
  time_unix_nano: &str,
) -> Value {
  json!({
    "name": name,
    "description": description,
    "unit": unit,
    "sum": {
      // CUMULATIVE
      "aggregationTemporality": 2,
      "isMonotonic": is_monotonic,
      "dataPoints": [
        {
          "attributes": otel_attributes(labels),
          "asInt": value,
          "timeUnixNano": time_unix_nano,
        }
      ]
    }
  })
}
