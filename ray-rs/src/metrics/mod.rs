//! Metrics and health checks.
//!
//! Core implementation used by bindings.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::sync::Arc;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, SystemTime};

use flate2::write::GzEncoder;
use flate2::Compression;
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_client::MetricsServiceClient as OtelMetricsServiceClient;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest as OtelExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{
  any_value as otel_any_value, AnyValue as OtelAnyValue,
  InstrumentationScope as OtelInstrumentationScope, KeyValue as OtelKeyValue,
};
use opentelemetry_proto::tonic::metrics::v1::{
  metric as otel_metric, number_data_point as otel_number_data_point,
  AggregationTemporality as OtelAggregationTemporality, Gauge as OtelGauge, Metric as OtelMetric,
  NumberDataPoint as OtelNumberDataPoint, ResourceMetrics as OtelResourceMetrics,
  ScopeMetrics as OtelScopeMetrics, Sum as OtelSum,
};
use opentelemetry_proto::tonic::resource::v1::Resource as OtelResource;
use parking_lot::Mutex;
use prost::Message;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tonic::codec::CompressionEncoding as TonicCompressionEncoding;
use tonic::metadata::MetadataValue;
use tonic::transport::{
  Certificate as TonicCertificate, ClientTlsConfig, Endpoint as TonicEndpoint,
  Identity as TonicIdentity,
};
use tonic::Code as TonicCode;

use crate::cache::manager::CacheManagerStats;
use crate::core::single_file::SingleFileDB;
use crate::error::{KiteError, Result};
use crate::replication::primary::PrimaryReplicationStatus;
use crate::replication::replica::ReplicaReplicationStatus;
use crate::types::DeltaState;

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

/// OTLP HTTP push result for replication metrics export.
#[derive(Debug, Clone)]
pub struct OtlpHttpExportResult {
  pub status_code: i64,
  pub response_body: String,
}

/// TLS/mTLS options for OTLP HTTP push.
#[derive(Debug, Clone, Default)]
pub struct OtlpHttpTlsOptions {
  pub https_only: bool,
  pub ca_cert_pem_path: Option<String>,
  pub client_cert_pem_path: Option<String>,
  pub client_key_pem_path: Option<String>,
}

/// OTLP HTTP push options for collector export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtlpAdaptiveRetryMode {
  #[default]
  Linear,
  Ewma,
}

#[derive(Debug, Clone)]
pub struct OtlpHttpPushOptions {
  pub timeout_ms: u64,
  pub bearer_token: Option<String>,
  pub retry_max_attempts: u32,
  pub retry_backoff_ms: u64,
  pub retry_backoff_max_ms: u64,
  pub retry_jitter_ratio: f64,
  pub adaptive_retry_mode: OtlpAdaptiveRetryMode,
  pub adaptive_retry_ewma_alpha: f64,
  pub adaptive_retry: bool,
  pub circuit_breaker_failure_threshold: u32,
  pub circuit_breaker_open_ms: u64,
  pub circuit_breaker_half_open_probes: u32,
  pub circuit_breaker_state_path: Option<String>,
  pub circuit_breaker_scope_key: Option<String>,
  pub compression_gzip: bool,
  pub tls: OtlpHttpTlsOptions,
}

impl Default for OtlpHttpPushOptions {
  fn default() -> Self {
    Self {
      timeout_ms: 5_000,
      bearer_token: None,
      retry_max_attempts: 1,
      retry_backoff_ms: 100,
      retry_backoff_max_ms: 2_000,
      retry_jitter_ratio: 0.0,
      adaptive_retry_mode: OtlpAdaptiveRetryMode::Linear,
      adaptive_retry_ewma_alpha: 0.3,
      adaptive_retry: false,
      circuit_breaker_failure_threshold: 0,
      circuit_breaker_open_ms: 0,
      circuit_breaker_half_open_probes: 1,
      circuit_breaker_state_path: None,
      circuit_breaker_scope_key: None,
      compression_gzip: false,
      tls: OtlpHttpTlsOptions::default(),
    }
  }
}

pub fn collect_metrics_single_file(db: &SingleFileDB) -> DatabaseMetrics {
  let stats = db.stats();
  let delta = db.delta.read();
  let cache_stats = db.cache.read().as_ref().map(|cache| cache.manager_stats());

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
  let replication = build_replication_metrics(
    db.primary_replication_status(),
    db.replica_replication_status(),
  );
  let delta_bytes = estimate_delta_memory(&delta);
  let cache_bytes = estimate_cache_memory(cache_stats.as_ref());
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
    cache,
    mvcc,
    replication,
    memory: MemoryMetrics {
      delta_estimate_bytes: delta_bytes,
      cache_estimate_bytes: cache_bytes,
      snapshot_bytes,
      total_estimate_bytes: delta_bytes + cache_bytes + snapshot_bytes,
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

/// Collect replication-only metrics and render them as OTLP protobuf payload.
pub fn collect_replication_metrics_otel_protobuf_single_file(db: &SingleFileDB) -> Vec<u8> {
  let metrics = collect_metrics_single_file(db);
  render_replication_metrics_otel_protobuf(&metrics)
}

/// Push replication OTLP-JSON payload to an OTLP collector endpoint.
///
/// Expects collector HTTP endpoint (for example `/v1/metrics`).
/// Returns an error when collector responds with non-2xx status.
pub fn push_replication_metrics_otel_json_single_file(
  db: &SingleFileDB,
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_json_single_file_with_options(db, endpoint, &options)
}

/// Push replication OTLP-JSON payload using explicit push options.
pub fn push_replication_metrics_otel_json_single_file_with_options(
  db: &SingleFileDB,
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  let payload = collect_replication_metrics_otel_json_single_file(db);
  push_replication_metrics_otel_json_payload_with_options(&payload, endpoint, options)
}

/// Push pre-rendered replication OTLP-JSON payload to an OTLP collector endpoint.
pub fn push_replication_metrics_otel_json_payload(
  payload: &str,
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_json_payload_with_options(payload, endpoint, &options)
}

/// Push pre-rendered replication OTLP-JSON payload using explicit push options.
pub fn push_replication_metrics_otel_json_payload_with_options(
  payload: &str,
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  push_replication_metrics_otel_http_payload_with_options(
    payload.as_bytes(),
    endpoint,
    options,
    "application/json",
  )
}

/// Push replication OTLP-protobuf payload to an OTLP collector endpoint.
pub fn push_replication_metrics_otel_protobuf_single_file(
  db: &SingleFileDB,
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_protobuf_single_file_with_options(db, endpoint, &options)
}

/// Push replication OTLP-protobuf payload using explicit push options.
pub fn push_replication_metrics_otel_protobuf_single_file_with_options(
  db: &SingleFileDB,
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  let payload = collect_replication_metrics_otel_protobuf_single_file(db);
  push_replication_metrics_otel_protobuf_payload_with_options(&payload, endpoint, options)
}

/// Push pre-rendered replication OTLP-protobuf payload to an OTLP collector endpoint.
pub fn push_replication_metrics_otel_protobuf_payload(
  payload: &[u8],
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_protobuf_payload_with_options(payload, endpoint, &options)
}

/// Push pre-rendered replication OTLP-protobuf payload using explicit push options.
pub fn push_replication_metrics_otel_protobuf_payload_with_options(
  payload: &[u8],
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  push_replication_metrics_otel_http_payload_with_options(
    payload,
    endpoint,
    options,
    "application/x-protobuf",
  )
}

/// Push replication OTLP-protobuf payload to an OTLP collector gRPC endpoint.
pub fn push_replication_metrics_otel_grpc_single_file(
  db: &SingleFileDB,
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_grpc_single_file_with_options(db, endpoint, &options)
}

/// Push replication OTLP-protobuf payload over gRPC using explicit push options.
pub fn push_replication_metrics_otel_grpc_single_file_with_options(
  db: &SingleFileDB,
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  let payload = collect_replication_metrics_otel_protobuf_single_file(db);
  push_replication_metrics_otel_grpc_payload_with_options(&payload, endpoint, options)
}

/// Push pre-rendered replication OTLP-protobuf payload to an OTLP collector gRPC endpoint.
pub fn push_replication_metrics_otel_grpc_payload(
  payload: &[u8],
  endpoint: &str,
  timeout_ms: u64,
  bearer_token: Option<&str>,
) -> Result<OtlpHttpExportResult> {
  let options = OtlpHttpPushOptions {
    timeout_ms,
    bearer_token: bearer_token.map(ToOwned::to_owned),
    ..OtlpHttpPushOptions::default()
  };
  push_replication_metrics_otel_grpc_payload_with_options(payload, endpoint, &options)
}

/// Push pre-rendered replication OTLP-protobuf payload over gRPC using explicit push options.
pub fn push_replication_metrics_otel_grpc_payload_with_options(
  payload: &[u8],
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  let endpoint = endpoint.trim();
  if endpoint.is_empty() {
    return Err(KiteError::InvalidQuery(
      "OTLP endpoint must not be empty".into(),
    ));
  }
  validate_otel_push_options(options)?;
  if options.tls.https_only && !endpoint_uses_https(endpoint) {
    return Err(KiteError::InvalidQuery(
      "OTLP endpoint must use https when https_only is enabled".into(),
    ));
  }
  check_circuit_breaker_open(endpoint, options)?;

  let request = OtelExportMetricsServiceRequest::decode(payload).map_err(|error| {
    KiteError::InvalidQuery(format!("Invalid OTLP protobuf payload: {error}").into())
  })?;
  push_replication_metrics_otel_grpc_request_with_options(request, endpoint, options)
}

fn push_replication_metrics_otel_grpc_request_with_options(
  request_payload: OtelExportMetricsServiceRequest,
  endpoint: &str,
  options: &OtlpHttpPushOptions,
) -> Result<OtlpHttpExportResult> {
  let timeout = Duration::from_millis(options.timeout_ms);
  let ca_cert_pem_path = options
    .tls
    .ca_cert_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());
  let client_cert_pem_path = options
    .tls
    .client_cert_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());
  let client_key_pem_path = options
    .tls
    .client_key_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());
  if client_cert_pem_path.is_some() ^ client_key_pem_path.is_some() {
    return Err(KiteError::InvalidQuery(
      "OTLP mTLS requires both client_cert_pem_path and client_key_pem_path".into(),
    ));
  }
  let custom_tls_configured =
    ca_cert_pem_path.is_some() || (client_cert_pem_path.is_some() && client_key_pem_path.is_some());
  if custom_tls_configured && !endpoint_uses_https(endpoint) {
    return Err(KiteError::InvalidQuery(
      "OTLP custom TLS/mTLS configuration requires an https endpoint".into(),
    ));
  }

  let mut endpoint_builder = TonicEndpoint::from_shared(endpoint.to_string())
    .map_err(|error| {
      KiteError::InvalidQuery(format!("Invalid OTLP gRPC endpoint: {error}").into())
    })?
    .connect_timeout(timeout)
    .timeout(timeout);

  if endpoint_uses_https(endpoint) || custom_tls_configured {
    let mut tls = ClientTlsConfig::new();
    if let Some(path) = ca_cert_pem_path {
      let pem = load_pem_bytes(path, "ca_cert_pem_path")?;
      tls = tls.ca_certificate(TonicCertificate::from_pem(pem));
    }
    if let (Some(cert_path), Some(key_path)) = (client_cert_pem_path, client_key_pem_path) {
      let cert_pem = load_pem_bytes(cert_path, "client_cert_pem_path")?;
      let key_pem = load_pem_bytes(key_path, "client_key_pem_path")?;
      tls = tls.identity(TonicIdentity::from_pem(cert_pem, key_pem));
    }
    endpoint_builder = endpoint_builder.tls_config(tls).map_err(|error| {
      KiteError::InvalidQuery(format!("Invalid OTLP gRPC TLS configuration: {error}").into())
    })?;
  }

  let bearer_token = options
    .bearer_token
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(ToOwned::to_owned);

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .map_err(|error| {
      KiteError::Internal(format!("Failed to initialize OTLP gRPC runtime: {error}"))
    })?;

  runtime.block_on(async move {
    for attempt in 1..=options.retry_max_attempts {
      let channel = match endpoint_builder.clone().connect().await {
        Ok(channel) => channel,
        Err(error) => {
          let transport_error = KiteError::Io(std::io::Error::other(format!(
            "OTLP collector gRPC transport error: {error}"
          )));
          if attempt < options.retry_max_attempts {
            tokio::time::sleep(retry_backoff_with_jitter_duration(
              endpoint, options, attempt,
            ))
            .await;
            continue;
          }
          record_circuit_breaker_failure(endpoint, options);
          return Err(transport_error);
        }
      };

      let mut client = OtelMetricsServiceClient::new(channel);
      if options.compression_gzip {
        client = client
          .send_compressed(TonicCompressionEncoding::Gzip)
          .accept_compressed(TonicCompressionEncoding::Gzip);
      }

      let mut request = tonic::Request::new(request_payload.clone());
      if let Some(token) = bearer_token.as_deref() {
        let header_value = MetadataValue::try_from(format!("Bearer {token}")).map_err(|error| {
          KiteError::InvalidQuery(
            format!("Invalid OTLP bearer token for gRPC metadata: {error}").into(),
          )
        })?;
        request.metadata_mut().insert("authorization", header_value);
      }

      match client.export(request).await {
        Ok(response) => {
          let body = response.into_inner();
          let response_body = match body.partial_success {
            Some(partial) => format!(
              "partial_success rejected_data_points={} error_message={}",
              partial.rejected_data_points, partial.error_message
            ),
            None => String::new(),
          };
          record_circuit_breaker_success(endpoint, options);
          return Ok(OtlpHttpExportResult {
            status_code: 200,
            response_body,
          });
        }
        Err(status) => {
          if attempt < options.retry_max_attempts && should_retry_grpc_status(status.code()) {
            tokio::time::sleep(retry_backoff_with_jitter_duration(
              endpoint, options, attempt,
            ))
            .await;
            continue;
          }
          record_circuit_breaker_failure(endpoint, options);
          return Err(KiteError::Internal(format!(
            "OTLP collector rejected replication metrics over gRPC: {status}"
          )));
        }
      }
    }

    record_circuit_breaker_failure(endpoint, options);
    Err(KiteError::Internal(
      "OTLP gRPC exporter exhausted retry attempts".to_string(),
    ))
  })
}

fn push_replication_metrics_otel_http_payload_with_options(
  payload: &[u8],
  endpoint: &str,
  options: &OtlpHttpPushOptions,
  content_type: &str,
) -> Result<OtlpHttpExportResult> {
  let endpoint = endpoint.trim();
  if endpoint.is_empty() {
    return Err(KiteError::InvalidQuery(
      "OTLP endpoint must not be empty".into(),
    ));
  }
  validate_otel_push_options(options)?;
  if options.tls.https_only && !endpoint_uses_https(endpoint) {
    return Err(KiteError::InvalidQuery(
      "OTLP endpoint must use https when https_only is enabled".into(),
    ));
  }
  check_circuit_breaker_open(endpoint, options)?;

  let request_payload = encode_http_request_payload(payload, options.compression_gzip)?;
  for attempt in 1..=options.retry_max_attempts {
    let timeout = Duration::from_millis(options.timeout_ms);
    let agent = build_otel_http_agent(endpoint, options, timeout)?;
    let mut request = agent
      .post(endpoint)
      .set("content-type", content_type)
      .timeout(timeout);
    if options.compression_gzip {
      request = request.set("content-encoding", "gzip");
    }
    if let Some(token) = options.bearer_token.as_deref() {
      if !token.trim().is_empty() {
        request = request.set("authorization", &format!("Bearer {token}"));
      }
    }

    match request.send_bytes(&request_payload) {
      Ok(response) => {
        let status_code = response.status() as i64;
        let response_body = response.into_string().unwrap_or_default();
        record_circuit_breaker_success(endpoint, options);
        return Ok(OtlpHttpExportResult {
          status_code,
          response_body,
        });
      }
      Err(ureq::Error::Status(status_code, response)) => {
        let body = response.into_string().unwrap_or_default();
        if attempt < options.retry_max_attempts && should_retry_http_status(status_code) {
          thread::sleep(retry_backoff_with_jitter_duration(
            endpoint, options, attempt,
          ));
          continue;
        }
        record_circuit_breaker_failure(endpoint, options);
        return Err(KiteError::Internal(format!(
          "OTLP collector rejected replication metrics: status {status_code}, body: {body}"
        )));
      }
      Err(ureq::Error::Transport(error)) => {
        if attempt < options.retry_max_attempts {
          thread::sleep(retry_backoff_with_jitter_duration(
            endpoint, options, attempt,
          ));
          continue;
        }
        record_circuit_breaker_failure(endpoint, options);
        return Err(KiteError::Io(std::io::Error::other(format!(
          "OTLP collector transport error: {error}"
        ))));
      }
    }
  }

  Err(KiteError::Internal(
    "OTLP exporter exhausted retry attempts".to_string(),
  ))
}

fn validate_otel_push_options(options: &OtlpHttpPushOptions) -> Result<()> {
  if options.timeout_ms == 0 {
    return Err(KiteError::InvalidQuery("timeout_ms must be > 0".into()));
  }
  if options.retry_max_attempts == 0 {
    return Err(KiteError::InvalidQuery(
      "retry_max_attempts must be > 0".into(),
    ));
  }
  if !(0.0..=1.0).contains(&options.retry_jitter_ratio) {
    return Err(KiteError::InvalidQuery(
      "retry_jitter_ratio must be within [0.0, 1.0]".into(),
    ));
  }
  if !(0.0..=1.0).contains(&options.adaptive_retry_ewma_alpha) {
    return Err(KiteError::InvalidQuery(
      "adaptive_retry_ewma_alpha must be within [0.0, 1.0]".into(),
    ));
  }
  if options.circuit_breaker_failure_threshold > 0 && options.circuit_breaker_open_ms == 0 {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_open_ms must be > 0 when circuit_breaker_failure_threshold is enabled"
        .into(),
    ));
  }
  if options.circuit_breaker_failure_threshold > 0 && options.circuit_breaker_half_open_probes == 0
  {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_half_open_probes must be > 0 when circuit_breaker_failure_threshold is enabled"
        .into(),
    ));
  }
  if let Some(path) = options.circuit_breaker_state_path.as_deref() {
    if path.trim().is_empty() {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_path must not be empty when provided".into(),
      ));
    }
  }
  if let Some(scope_key) = options.circuit_breaker_scope_key.as_deref() {
    if scope_key.trim().is_empty() {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_scope_key must not be empty when provided".into(),
      ));
    }
  }
  Ok(())
}

fn should_retry_http_status(status_code: u16) -> bool {
  status_code == 429 || status_code >= 500
}

fn should_retry_grpc_status(code: TonicCode) -> bool {
  matches!(
    code,
    TonicCode::Unavailable | TonicCode::DeadlineExceeded | TonicCode::ResourceExhausted
  )
}

fn retry_backoff_duration(options: &OtlpHttpPushOptions, attempt: u32) -> Duration {
  if attempt <= 1 || options.retry_backoff_ms == 0 {
    return Duration::from_millis(options.retry_backoff_ms);
  }
  let shift = (attempt - 1).min(31);
  let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
  let raw = options.retry_backoff_ms.saturating_mul(multiplier);
  let backoff = if options.retry_backoff_max_ms == 0 {
    raw
  } else {
    raw.min(options.retry_backoff_max_ms)
  };
  Duration::from_millis(backoff)
}

fn retry_backoff_with_jitter_duration(
  endpoint: &str,
  options: &OtlpHttpPushOptions,
  attempt: u32,
) -> Duration {
  let multiplier = adaptive_retry_multiplier(endpoint, options);
  let base = retry_backoff_duration(options, attempt);
  let mut base_ms = base.as_millis() as u64;
  if multiplier > 1 {
    base_ms = base_ms.saturating_mul(multiplier);
    if options.retry_backoff_max_ms > 0 {
      base_ms = base_ms.min(options.retry_backoff_max_ms);
    }
  }
  if options.retry_jitter_ratio <= 0.0 {
    return Duration::from_millis(base_ms);
  }
  if base_ms == 0 {
    return Duration::from_millis(base_ms);
  }
  let jitter_max = ((base_ms as f64) * options.retry_jitter_ratio) as u64;
  if jitter_max == 0 {
    return Duration::from_millis(base_ms);
  }
  let jitter = rand::thread_rng().gen_range(0..=jitter_max);
  Duration::from_millis(base_ms.saturating_add(jitter))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct OtlpCircuitBreakerState {
  consecutive_failures: u32,
  open_until_ms: u64,
  half_open_remaining_probes: u32,
  half_open_in_flight: bool,
  ewma_error_score: f64,
}

static OTLP_CIRCUIT_BREAKERS: OnceLock<Mutex<HashMap<String, OtlpCircuitBreakerState>>> =
  OnceLock::new();

fn otlp_circuit_breakers() -> &'static Mutex<HashMap<String, OtlpCircuitBreakerState>> {
  OTLP_CIRCUIT_BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn circuit_breaker_now_ms() -> u64 {
  SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64
}

fn circuit_breaker_key(endpoint: &str, options: &OtlpHttpPushOptions) -> String {
  options
    .circuit_breaker_scope_key
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .unwrap_or(endpoint)
    .to_string()
}

fn circuit_breaker_state_path(options: &OtlpHttpPushOptions) -> Option<&str> {
  options
    .circuit_breaker_state_path
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
}

fn load_persisted_breakers(path: &str) -> HashMap<String, OtlpCircuitBreakerState> {
  let raw = match fs::read(path) {
    Ok(bytes) => bytes,
    Err(_) => return HashMap::new(),
  };
  serde_json::from_slice::<HashMap<String, OtlpCircuitBreakerState>>(&raw).unwrap_or_default()
}

fn persist_breakers(path: &str, states: &HashMap<String, OtlpCircuitBreakerState>) {
  let Ok(serialized) = serde_json::to_vec(states) else {
    return;
  };
  let _ = fs::write(path, serialized);
}

fn merge_persisted_breaker_state(
  key: &str,
  options: &OtlpHttpPushOptions,
  states: &mut HashMap<String, OtlpCircuitBreakerState>,
) {
  let Some(path) = circuit_breaker_state_path(options) else {
    return;
  };
  let persisted = load_persisted_breakers(path);
  let Some(persisted_state) = persisted.get(key).cloned() else {
    return;
  };
  let entry = states.entry(key.to_string()).or_default();
  entry.consecutive_failures = entry
    .consecutive_failures
    .max(persisted_state.consecutive_failures);
  entry.open_until_ms = entry.open_until_ms.max(persisted_state.open_until_ms);
  entry.half_open_remaining_probes = entry
    .half_open_remaining_probes
    .max(persisted_state.half_open_remaining_probes);
  entry.ewma_error_score = entry
    .ewma_error_score
    .max(persisted_state.ewma_error_score)
    .clamp(0.0, 1.0);
}

fn adaptive_retry_multiplier(endpoint: &str, options: &OtlpHttpPushOptions) -> u64 {
  if !options.adaptive_retry {
    return 1;
  }
  let key = circuit_breaker_key(endpoint, options);
  let mut states = otlp_circuit_breakers().lock();
  merge_persisted_breaker_state(&key, options, &mut states);
  let multiplier = states
    .get(&key)
    .map(|state| match options.adaptive_retry_mode {
      OtlpAdaptiveRetryMode::Linear => 1 + u64::from(state.consecutive_failures.min(8)),
      OtlpAdaptiveRetryMode::Ewma => {
        let score = state.ewma_error_score.clamp(0.0, 1.0);
        1 + ((score * 8.0).round() as u64)
      }
    })
    .unwrap_or(1);
  multiplier.max(1)
}

fn check_circuit_breaker_open(endpoint: &str, options: &OtlpHttpPushOptions) -> Result<()> {
  if options.circuit_breaker_failure_threshold == 0 {
    return Ok(());
  }
  let key = circuit_breaker_key(endpoint, options);
  let now = circuit_breaker_now_ms();
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, options, &mut states);
    let Some(state) = states.get_mut(&key) else {
      return Ok(());
    };
    if state.open_until_ms > now {
      return Err(KiteError::Internal(format!(
        "OTLP circuit breaker open for endpoint {endpoint} until {}",
        state.open_until_ms
      )));
    }

    let mut changed = false;
    if state.open_until_ms > 0 {
      state.open_until_ms = 0;
      if state.half_open_remaining_probes == 0 && !state.half_open_in_flight {
        state.half_open_remaining_probes = options.circuit_breaker_half_open_probes.max(1);
      }
      changed = true;
    }

    if state.half_open_in_flight {
      return Err(KiteError::Internal(format!(
        "OTLP circuit breaker half-open probe already in flight for endpoint {endpoint}"
      )));
    }

    if state.half_open_remaining_probes > 0 {
      state.half_open_remaining_probes = state.half_open_remaining_probes.saturating_sub(1);
      state.half_open_in_flight = true;
      changed = true;
    }

    if changed {
      Some(states.clone())
    } else {
      None
    }
  };
  if let (Some(path), Some(snapshot)) = (circuit_breaker_state_path(options), snapshot) {
    persist_breakers(path, &snapshot);
  }
  Ok(())
}

fn record_circuit_breaker_success(endpoint: &str, options: &OtlpHttpPushOptions) {
  if options.circuit_breaker_failure_threshold == 0 && !options.adaptive_retry {
    return;
  }
  let key = circuit_breaker_key(endpoint, options);
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, options, &mut states);
    let state = states.entry(key.clone()).or_default();
    let alpha = options.adaptive_retry_ewma_alpha.clamp(0.0, 1.0);
    state.ewma_error_score = ((1.0 - alpha) * state.ewma_error_score).clamp(0.0, 1.0);
    state.consecutive_failures = 0;
    state.open_until_ms = 0;
    state.half_open_in_flight = false;
    if !options.adaptive_retry
      && state.consecutive_failures == 0
      && state.open_until_ms == 0
      && state.half_open_remaining_probes == 0
      && !state.half_open_in_flight
    {
      states.remove(&key);
    } else if options.adaptive_retry
      && state.consecutive_failures == 0
      && state.open_until_ms == 0
      && state.half_open_remaining_probes == 0
      && !state.half_open_in_flight
      && state.ewma_error_score <= f64::EPSILON
    {
      states.remove(&key);
    }
    states.clone()
  };
  if let Some(path) = circuit_breaker_state_path(options) {
    persist_breakers(path, &snapshot);
  }
}

fn record_circuit_breaker_failure(endpoint: &str, options: &OtlpHttpPushOptions) {
  if (options.circuit_breaker_failure_threshold == 0 || options.circuit_breaker_open_ms == 0)
    && !options.adaptive_retry
  {
    return;
  }
  let key = circuit_breaker_key(endpoint, options);
  let now = circuit_breaker_now_ms();
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, options, &mut states);
    let state = states.entry(key).or_default();
    let alpha = options.adaptive_retry_ewma_alpha.clamp(0.0, 1.0);
    state.ewma_error_score = ((1.0 - alpha) * state.ewma_error_score + alpha).clamp(0.0, 1.0);
    if options.circuit_breaker_failure_threshold > 0 && options.circuit_breaker_open_ms > 0 {
      let probe_budget = options.circuit_breaker_half_open_probes.max(1);
      if state.half_open_in_flight || state.half_open_remaining_probes > 0 {
        state.open_until_ms = now.saturating_add(options.circuit_breaker_open_ms);
        state.consecutive_failures = 0;
        state.half_open_remaining_probes = probe_budget;
        state.half_open_in_flight = false;
      } else {
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        if state.consecutive_failures >= options.circuit_breaker_failure_threshold {
          state.open_until_ms = now.saturating_add(options.circuit_breaker_open_ms);
          state.consecutive_failures = 0;
          state.half_open_remaining_probes = probe_budget;
          state.half_open_in_flight = false;
        }
      }
    }
    states.clone()
  };
  if let Some(path) = circuit_breaker_state_path(options) {
    persist_breakers(path, &snapshot);
  }
}

fn encode_http_request_payload(payload: &[u8], compression_gzip: bool) -> Result<Vec<u8>> {
  if !compression_gzip {
    return Ok(payload.to_vec());
  }
  let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
  encoder.write_all(payload).map_err(|error| {
    KiteError::Internal(format!(
      "Failed compressing OTLP payload with gzip: {error}"
    ))
  })?;
  encoder.finish().map_err(|error| {
    KiteError::Internal(format!(
      "Failed finalizing compressed OTLP payload: {error}"
    ))
  })
}

fn endpoint_uses_https(endpoint: &str) -> bool {
  endpoint.to_ascii_lowercase().starts_with("https://")
}

fn build_otel_http_agent(
  endpoint: &str,
  options: &OtlpHttpPushOptions,
  timeout: Duration,
) -> Result<ureq::Agent> {
  let ca_cert_pem_path = options
    .tls
    .ca_cert_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());
  let client_cert_pem_path = options
    .tls
    .client_cert_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());
  let client_key_pem_path = options
    .tls
    .client_key_pem_path
    .as_deref()
    .map(str::trim)
    .filter(|path| !path.is_empty());

  if client_cert_pem_path.is_some() ^ client_key_pem_path.is_some() {
    return Err(KiteError::InvalidQuery(
      "OTLP mTLS requires both client_cert_pem_path and client_key_pem_path".into(),
    ));
  }

  let custom_tls_configured =
    ca_cert_pem_path.is_some() || (client_cert_pem_path.is_some() && client_key_pem_path.is_some());
  if custom_tls_configured && !endpoint_uses_https(endpoint) {
    return Err(KiteError::InvalidQuery(
      "OTLP custom TLS/mTLS configuration requires an https endpoint".into(),
    ));
  }

  let mut builder = ureq::builder()
    .https_only(options.tls.https_only)
    .timeout_connect(timeout)
    .timeout_read(timeout)
    .timeout_write(timeout);

  if custom_tls_configured {
    let mut root_store = ureq::rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if let Some(path) = ca_cert_pem_path {
      let certs = load_certificates_from_pem(path, "ca_cert_pem_path")?;
      let (valid_count, _) = root_store.add_parsable_certificates(certs);
      if valid_count == 0 {
        return Err(KiteError::InvalidQuery(
          format!("No valid CA certificates found in ca_cert_pem_path: {path}").into(),
        ));
      }
    }

    let client_config_builder =
      ureq::rustls::ClientConfig::builder().with_root_certificates(root_store);
    let client_config =
      if let (Some(cert_path), Some(key_path)) = (client_cert_pem_path, client_key_pem_path) {
        let certs = load_certificates_from_pem(cert_path, "client_cert_pem_path")?;
        let key = load_private_key_from_pem(key_path, "client_key_pem_path")?;
        client_config_builder
          .with_client_auth_cert(certs, key)
          .map_err(|error| {
            KiteError::InvalidQuery(
              format!("Invalid OTLP client certificate/key for mTLS: {error}").into(),
            )
          })?
      } else {
        client_config_builder.with_no_client_auth()
      };

    builder = builder.tls_config(Arc::new(client_config));
  }

  Ok(builder.build())
}

fn load_certificates_from_pem(
  path: &str,
  field_name: &str,
) -> Result<Vec<ureq::rustls::pki_types::CertificateDer<'static>>> {
  let file = File::open(path).map_err(|error| {
    KiteError::InvalidQuery(format!("Failed opening {field_name} '{path}': {error}").into())
  })?;
  let mut reader = BufReader::new(file);
  let certs = rustls_pemfile::certs(&mut reader)
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|error| {
      KiteError::InvalidQuery(
        format!("Failed parsing certificates from {field_name} '{path}': {error}").into(),
      )
    })?;
  if certs.is_empty() {
    return Err(KiteError::InvalidQuery(
      format!("No certificates found in {field_name} '{path}'").into(),
    ));
  }
  Ok(certs)
}

fn load_private_key_from_pem(
  path: &str,
  field_name: &str,
) -> Result<ureq::rustls::pki_types::PrivateKeyDer<'static>> {
  let file = File::open(path).map_err(|error| {
    KiteError::InvalidQuery(format!("Failed opening {field_name} '{path}': {error}").into())
  })?;
  let mut reader = BufReader::new(file);
  rustls_pemfile::private_key(&mut reader)
    .map_err(|error| {
      KiteError::InvalidQuery(
        format!("Failed parsing private key from {field_name} '{path}': {error}").into(),
      )
    })?
    .ok_or_else(|| {
      KiteError::InvalidQuery(format!("No private key found in {field_name} '{path}'").into())
    })
}

fn load_pem_bytes(path: &str, field_name: &str) -> Result<Vec<u8>> {
  let bytes = fs::read(path).map_err(|error| {
    KiteError::InvalidQuery(format!("Failed reading {field_name} '{path}': {error}").into())
  })?;
  if bytes.is_empty() {
    return Err(KiteError::InvalidQuery(
      format!("{field_name} '{path}' is empty").into(),
    ));
  }
  Ok(bytes)
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

/// Render replication metrics in OpenTelemetry OTLP protobuf wire format.
pub fn render_replication_metrics_otel_protobuf(metrics: &DatabaseMetrics) -> Vec<u8> {
  let role = metrics.replication.role.as_str();
  let enabled = if metrics.replication.enabled { 1 } else { 0 };
  let time_unix_nano = metric_time_unix_nano_u64(metrics);
  let mut otel_metrics: Vec<OtelMetric> = Vec::new();

  otel_metrics.push(otel_proto_gauge_metric(
    "kitedb.replication.enabled",
    "Whether replication is enabled for this database (1 enabled, 0 disabled).",
    "1",
    enabled,
    &[("role", role)],
    time_unix_nano,
  ));

  // Host-runtime export path is process-local and does not enforce HTTP auth.
  otel_metrics.push(otel_proto_gauge_metric(
    "kitedb.replication.auth.enabled",
    "Whether replication admin auth is enabled for this metrics exporter.",
    "1",
    0,
    &[],
    time_unix_nano,
  ));

  if let Some(primary) = metrics.replication.primary.as_ref() {
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.epoch",
      "Current primary replication epoch.",
      "1",
      primary.epoch,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.head_log_index",
      "Current primary head log index.",
      "1",
      primary.head_log_index,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.retained_floor",
      "Current primary retained floor log index.",
      "1",
      primary.retained_floor,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.replica_count",
      "Replica progress reporters known by this primary.",
      "1",
      primary.replica_count,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.stale_epoch_replica_count",
      "Replica reporters currently on stale epochs.",
      "1",
      primary.stale_epoch_replica_count,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.max_replica_lag",
      "Maximum reported lag (log frames) across replicas.",
      "1",
      primary.max_replica_lag,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_sum_metric(
      "kitedb.replication.primary.append_attempts",
      "Total replication append attempts on the primary commit path.",
      "1",
      primary.append_attempts,
      true,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_sum_metric(
      "kitedb.replication.primary.append_failures",
      "Total replication append failures on the primary commit path.",
      "1",
      primary.append_failures,
      true,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_sum_metric(
      "kitedb.replication.primary.append_successes",
      "Total replication append successes on the primary commit path.",
      "1",
      primary.append_successes,
      true,
      &[],
      time_unix_nano,
    ));
  }

  if let Some(replica) = metrics.replication.replica.as_ref() {
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.replica.applied_epoch",
      "Replica applied epoch.",
      "1",
      replica.applied_epoch,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.replica.applied_log_index",
      "Replica applied log index.",
      "1",
      replica.applied_log_index,
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.replica.needs_reseed",
      "Whether replica currently requires snapshot reseed (1 yes, 0 no).",
      "1",
      if replica.needs_reseed { 1 } else { 0 },
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.replica.last_error_present",
      "Whether replica currently has a non-empty last_error value (1 yes, 0 no).",
      "1",
      if replica.last_error.is_some() { 1 } else { 0 },
      &[],
      time_unix_nano,
    ));
  }

  let request = OtelExportMetricsServiceRequest {
    resource_metrics: vec![OtelResourceMetrics {
      resource: Some(OtelResource {
        attributes: vec![
          otel_proto_attr_string("service.name", "kitedb"),
          otel_proto_attr_string("kitedb.database.path", metrics.path.as_str()),
          otel_proto_attr_string("kitedb.metrics.scope", "replication"),
        ],
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
      }),
      scope_metrics: vec![OtelScopeMetrics {
        scope: Some(OtelInstrumentationScope {
          name: "kitedb.metrics.replication".to_string(),
          version: env!("CARGO_PKG_VERSION").to_string(),
          attributes: Vec::new(),
          dropped_attributes_count: 0,
        }),
        metrics: otel_metrics,
        schema_url: String::new(),
      }],
      schema_url: String::new(),
    }],
  };
  request.encode_to_vec()
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

  let cache_stats = db.cache.read().as_ref().map(|cache| cache.manager_stats());
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

fn otel_proto_attr_string(key: &str, value: &str) -> OtelKeyValue {
  OtelKeyValue {
    key: key.to_string(),
    value: Some(OtelAnyValue {
      value: Some(otel_any_value::Value::StringValue(value.to_string())),
    }),
  }
}

fn otel_proto_attributes(labels: &[(&str, &str)]) -> Vec<OtelKeyValue> {
  labels
    .iter()
    .map(|(key, value)| otel_proto_attr_string(key, value))
    .collect()
}

fn otel_proto_number_data_point(
  value: i64,
  labels: &[(&str, &str)],
  time_unix_nano: u64,
) -> OtelNumberDataPoint {
  OtelNumberDataPoint {
    attributes: otel_proto_attributes(labels),
    start_time_unix_nano: 0,
    time_unix_nano,
    exemplars: Vec::new(),
    flags: 0,
    value: Some(otel_number_data_point::Value::AsInt(value)),
  }
}

fn otel_proto_gauge_metric(
  name: &str,
  description: &str,
  unit: &str,
  value: i64,
  labels: &[(&str, &str)],
  time_unix_nano: u64,
) -> OtelMetric {
  OtelMetric {
    name: name.to_string(),
    description: description.to_string(),
    unit: unit.to_string(),
    metadata: Vec::new(),
    data: Some(otel_metric::Data::Gauge(OtelGauge {
      data_points: vec![otel_proto_number_data_point(value, labels, time_unix_nano)],
    })),
  }
}

fn otel_proto_sum_metric(
  name: &str,
  description: &str,
  unit: &str,
  value: i64,
  is_monotonic: bool,
  labels: &[(&str, &str)],
  time_unix_nano: u64,
) -> OtelMetric {
  OtelMetric {
    name: name.to_string(),
    description: description.to_string(),
    unit: unit.to_string(),
    metadata: Vec::new(),
    data: Some(otel_metric::Data::Sum(OtelSum {
      data_points: vec![otel_proto_number_data_point(value, labels, time_unix_nano)],
      aggregation_temporality: OtelAggregationTemporality::Cumulative as i32,
      is_monotonic,
    })),
  }
}
