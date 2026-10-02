//! OTLP export of replication metrics: protobuf rendering and push to a
//! collector over HTTP (JSON or protobuf) or gRPC, with retries and a circuit
//! breaker. Compiled with the `otlp` feature.

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
use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::transport::{
  Certificate as TonicCertificate, Channel as TonicChannel, ClientTlsConfig,
  Endpoint as TonicEndpoint, Identity as TonicIdentity,
};
use tonic::Code as TonicCode;

use super::{
  collect_metrics_single_file, collect_replication_metrics_otel_json_single_file,
  metric_time_unix_nano_u64, DatabaseMetrics,
};
use crate::core::single_file::SingleFileDB;
use crate::error::{KiteError, Result};

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
  pub circuit_breaker_state_url: Option<String>,
  pub circuit_breaker_state_patch: bool,
  pub circuit_breaker_state_patch_batch: bool,
  pub circuit_breaker_state_patch_batch_max_keys: u32,
  pub circuit_breaker_state_patch_merge: bool,
  pub circuit_breaker_state_patch_merge_max_keys: u32,
  pub circuit_breaker_state_patch_retry_max_attempts: u32,
  pub circuit_breaker_state_cas: bool,
  pub circuit_breaker_state_lease_id: Option<String>,
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
      circuit_breaker_state_url: None,
      circuit_breaker_state_patch: false,
      circuit_breaker_state_patch_batch: false,
      circuit_breaker_state_patch_batch_max_keys: 8,
      circuit_breaker_state_patch_merge: false,
      circuit_breaker_state_patch_merge_max_keys: 32,
      circuit_breaker_state_patch_retry_max_attempts: 1,
      circuit_breaker_state_cas: false,
      circuit_breaker_state_lease_id: None,
      circuit_breaker_scope_key: None,
      compression_gzip: false,
      tls: OtlpHttpTlsOptions::default(),
    }
  }
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
///
/// Pushes run on one runtime the crate creates on first use, so this can be
/// called from any thread, including a task of the caller's own tokio
/// runtime (it blocks that thread until the push ends). Connections are
/// reused across pushes to the same endpoint and TLS configuration.
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
  let tls = OtlpTlsPaths::from_options(endpoint, options)?;

  let mut endpoint_builder = TonicEndpoint::from_shared(endpoint.to_string())
    .map_err(|error| {
      KiteError::InvalidQuery(format!("Invalid OTLP gRPC endpoint: {error}").into())
    })?
    .connect_timeout(timeout)
    .timeout(timeout);

  if endpoint_uses_https(endpoint) || tls.custom() {
    let mut tls_config = ClientTlsConfig::new();
    if let Some(path) = tls.ca_cert {
      let pem = load_pem_bytes(path, "ca_cert_pem_path")?;
      tls_config = tls_config.ca_certificate(TonicCertificate::from_pem(pem));
    }
    if let Some((cert_path, key_path)) = tls.client_identity {
      let cert_pem = load_pem_bytes(cert_path, "client_cert_pem_path")?;
      let key_pem = load_pem_bytes(key_path, "client_key_pem_path")?;
      tls_config = tls_config.identity(TonicIdentity::from_pem(cert_pem, key_pem));
    }
    endpoint_builder = endpoint_builder.tls_config(tls_config).map_err(|error| {
      KiteError::InvalidQuery(format!("Invalid OTLP gRPC TLS configuration: {error}").into())
    })?;
  }

  let authorization = match options
    .bearer_token
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
  {
    Some(token) => Some(
      MetadataValue::try_from(format!("Bearer {token}")).map_err(|error| {
        KiteError::InvalidQuery(
          format!("Invalid OTLP bearer token for gRPC metadata: {error}").into(),
        )
      })?,
    ),
    None => None,
  };

  let runtime = otlp_grpc_runtime()?;
  let admission = check_circuit_breaker_open(endpoint, options)?;
  let push = GrpcPush {
    channel_key: format!(
      "{endpoint}|{}|{:?}|{:?}",
      options.timeout_ms, tls.ca_cert, tls.client_identity
    ),
    endpoint: endpoint.to_string(),
    endpoint_builder,
    request: request_payload,
    authorization,
    options: options.clone(),
  };
  // A push runs as a task of the shared runtime: blocking on it is allowed
  // on any thread, while `block_on` panics inside another runtime.
  let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
  runtime.spawn(async move {
    let _ = result_tx.send(push.run().await);
  });
  let result = result_rx.recv().unwrap_or_else(|_| {
    Err(KiteError::Internal(
      "OTLP gRPC push task stopped without a result".to_string(),
    ))
  });
  admission.record(result)
}

/// The runtime every gRPC push runs on, created on first use.
fn otlp_grpc_runtime() -> Result<&'static tokio::runtime::Runtime> {
  static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
  static INIT: Mutex<()> = Mutex::new(());
  if let Some(runtime) = RUNTIME.get() {
    return Ok(runtime);
  }
  let _init = INIT.lock();
  if let Some(runtime) = RUNTIME.get() {
    return Ok(runtime);
  }
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(1)
    .thread_name("kitedb-otlp-grpc")
    .enable_all()
    .build()
    .map_err(|error| {
      KiteError::Internal(format!("Failed to initialize OTLP gRPC runtime: {error}"))
    })?;
  Ok(RUNTIME.get_or_init(|| runtime))
}

/// Connected gRPC channels by endpoint, timeout, and TLS file paths. A channel
/// reconnects by itself; one whose export fails is dropped, so the next push
/// connects afresh (and rereads the TLS files).
fn otlp_grpc_channels() -> &'static Mutex<HashMap<String, TonicChannel>> {
  static CHANNELS: OnceLock<Mutex<HashMap<String, TonicChannel>>> = OnceLock::new();
  CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One gRPC push, owned so it can run as a task of the shared runtime.
struct GrpcPush {
  channel_key: String,
  endpoint: String,
  endpoint_builder: TonicEndpoint,
  request: OtelExportMetricsServiceRequest,
  authorization: Option<AsciiMetadataValue>,
  options: OtlpHttpPushOptions,
}

impl GrpcPush {
  /// Every error is the endpoint's: setup errors were returned before.
  async fn run(self) -> Result<OtlpHttpExportResult> {
    let options = &self.options;
    for attempt in 1..=options.retry_max_attempts {
      let cached = otlp_grpc_channels().lock().get(&self.channel_key).cloned();
      let channel = match cached {
        Some(channel) => channel,
        None => match self.endpoint_builder.clone().connect().await {
          Ok(channel) => {
            otlp_grpc_channels()
              .lock()
              .insert(self.channel_key.clone(), channel.clone());
            channel
          }
          Err(error) => {
            if attempt < options.retry_max_attempts {
              tokio::time::sleep(retry_backoff_with_jitter_duration(
                &self.endpoint,
                options,
                attempt,
              ))
              .await;
              continue;
            }
            return Err(KiteError::Io(std::io::Error::other(format!(
              "OTLP collector gRPC transport error: {error}"
            ))));
          }
        },
      };

      let mut client = OtelMetricsServiceClient::new(channel);
      if options.compression_gzip {
        client = client
          .send_compressed(TonicCompressionEncoding::Gzip)
          .accept_compressed(TonicCompressionEncoding::Gzip);
      }

      let mut request = tonic::Request::new(self.request.clone());
      if let Some(authorization) = self.authorization.clone() {
        request
          .metadata_mut()
          .insert("authorization", authorization);
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
          return Ok(OtlpHttpExportResult {
            status_code: 200,
            response_body,
          });
        }
        Err(status) => {
          otlp_grpc_channels().lock().remove(&self.channel_key);
          if attempt < options.retry_max_attempts && should_retry_grpc_status(status.code()) {
            tokio::time::sleep(retry_backoff_with_jitter_duration(
              &self.endpoint,
              options,
              attempt,
            ))
            .await;
            continue;
          }
          return Err(KiteError::Internal(format!(
            "OTLP collector rejected replication metrics over gRPC: {status}"
          )));
        }
      }
    }

    Err(KiteError::Internal(
      "OTLP gRPC exporter exhausted retry attempts".to_string(),
    ))
  }
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

  let request_payload = encode_http_request_payload(payload, options.compression_gzip)?;
  let timeout = Duration::from_millis(options.timeout_ms);
  let agent = build_otel_http_agent(endpoint, options, timeout)?;
  let admission = check_circuit_breaker_open(endpoint, options)?;
  let result = (|| {
    for attempt in 1..=options.retry_max_attempts {
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
          return Err(KiteError::Io(std::io::Error::other(format!(
            "OTLP collector transport error: {error}"
          ))));
        }
      }
    }

    Err(KiteError::Internal(
      "OTLP exporter exhausted retry attempts".to_string(),
    ))
  })();
  admission.record(result)
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
  if let Some(url) = options.circuit_breaker_state_url.as_deref() {
    let trimmed = url.trim();
    if trimmed.is_empty() {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_url must not be empty when provided".into(),
      ));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_url must use http:// or https://".into(),
      ));
    }
    if options.tls.https_only && !endpoint_uses_https(trimmed) {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_url must use https when https_only is enabled".into(),
      ));
    }
  }
  if options.circuit_breaker_state_path.is_some() && options.circuit_breaker_state_url.is_some() {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_path and circuit_breaker_state_url are mutually exclusive".into(),
    ));
  }
  if options.circuit_breaker_state_patch && options.circuit_breaker_state_url.is_none() {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch requires circuit_breaker_state_url".into(),
    ));
  }
  if options.circuit_breaker_state_patch_batch && !options.circuit_breaker_state_patch {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch_batch requires circuit_breaker_state_patch".into(),
    ));
  }
  if options.circuit_breaker_state_patch_merge && !options.circuit_breaker_state_patch {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch_merge requires circuit_breaker_state_patch".into(),
    ));
  }
  if options.circuit_breaker_state_patch_batch_max_keys == 0 {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch_batch_max_keys must be > 0".into(),
    ));
  }
  if options.circuit_breaker_state_patch_merge_max_keys == 0 {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch_merge_max_keys must be > 0".into(),
    ));
  }
  if options.circuit_breaker_state_patch_retry_max_attempts == 0 {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_patch_retry_max_attempts must be > 0".into(),
    ));
  }
  if options.circuit_breaker_state_cas && options.circuit_breaker_state_url.is_none() {
    return Err(KiteError::InvalidQuery(
      "circuit_breaker_state_cas requires circuit_breaker_state_url".into(),
    ));
  }
  if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
    if lease_id.trim().is_empty() {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_lease_id must not be empty when provided".into(),
      ));
    }
    if options.circuit_breaker_state_url.is_none() {
      return Err(KiteError::InvalidQuery(
        "circuit_breaker_state_lease_id requires circuit_breaker_state_url".into(),
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
static OTLP_CIRCUIT_BREAKER_STATE_URL_ETAGS: OnceLock<Mutex<HashMap<String, String>>> =
  OnceLock::new();

fn otlp_circuit_breakers() -> &'static Mutex<HashMap<String, OtlpCircuitBreakerState>> {
  OTLP_CIRCUIT_BREAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn otlp_circuit_breaker_state_url_etags() -> &'static Mutex<HashMap<String, String>> {
  OTLP_CIRCUIT_BREAKER_STATE_URL_ETAGS.get_or_init(|| Mutex::new(HashMap::new()))
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

fn circuit_breaker_state_url(options: &OtlpHttpPushOptions) -> Option<&str> {
  options
    .circuit_breaker_state_url
    .as_deref()
    .map(str::trim)
    .filter(|value| !value.is_empty())
}

fn circuit_breaker_state_url_etag_key(url: &str, scope: &str, key: Option<&str>) -> String {
  match key {
    Some(value) => format!("{url}::{scope}::{value}"),
    None => format!("{url}::{scope}"),
  }
}

fn load_persisted_breakers_from_path(path: &str) -> HashMap<String, OtlpCircuitBreakerState> {
  let raw = match fs::read(path) {
    Ok(bytes) => bytes,
    Err(_) => return HashMap::new(),
  };
  serde_json::from_slice::<HashMap<String, OtlpCircuitBreakerState>>(&raw).unwrap_or_default()
}

fn load_persisted_breakers_from_url(
  url: &str,
  options: &OtlpHttpPushOptions,
) -> HashMap<String, OtlpCircuitBreakerState> {
  let timeout = Duration::from_millis(options.timeout_ms.max(1));
  let agent = match build_otel_http_agent(url, options, timeout) {
    Ok(agent) => agent,
    Err(_) => return HashMap::new(),
  };
  let mut request = agent.get(url).timeout(timeout);
  if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
    request = request.set("x-kitedb-breaker-lease", lease_id);
  }
  let response = match request.call() {
    Ok(response) => response,
    Err(_) => return HashMap::new(),
  };
  if options.circuit_breaker_state_cas {
    if let Some(etag) = response.header("etag") {
      otlp_circuit_breaker_state_url_etags().lock().insert(
        circuit_breaker_state_url_etag_key(url, "doc", None),
        etag.to_string(),
      );
    }
  }
  let body = response.into_string().unwrap_or_default();
  serde_json::from_str::<HashMap<String, OtlpCircuitBreakerState>>(&body).unwrap_or_default()
}

fn load_persisted_breaker_from_url_patch(
  url: &str,
  key: &str,
  options: &OtlpHttpPushOptions,
) -> Option<OtlpCircuitBreakerState> {
  let timeout = Duration::from_millis(options.timeout_ms.max(1));
  let agent = match build_otel_http_agent(url, options, timeout) {
    Ok(agent) => agent,
    Err(_) => return None,
  };
  let mut request = agent
    .get(url)
    .set("x-kitedb-breaker-mode", "patch-v1")
    .set("x-kitedb-breaker-key", key)
    .timeout(timeout);
  if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
    request = request.set("x-kitedb-breaker-lease", lease_id);
  }
  let response = match request.call() {
    Ok(response) => response,
    Err(ureq::Error::Status(404, _)) => return None,
    Err(_) => return None,
  };
  if options.circuit_breaker_state_cas {
    if let Some(etag) = response.header("etag") {
      let mut etags = otlp_circuit_breaker_state_url_etags().lock();
      etags.insert(
        circuit_breaker_state_url_etag_key(url, "patch", Some(key)),
        etag.to_string(),
      );
      if options.circuit_breaker_state_patch_batch {
        etags.insert(
          circuit_breaker_state_url_etag_key(url, "batch", None),
          etag.to_string(),
        );
      }
      if options.circuit_breaker_state_patch_merge {
        etags.insert(
          circuit_breaker_state_url_etag_key(url, "merge", None),
          etag.to_string(),
        );
      }
    }
  }
  let body = response.into_string().unwrap_or_default();
  if body.trim().is_empty() {
    return None;
  }
  if let Ok(state) = serde_json::from_str::<OtlpCircuitBreakerState>(&body) {
    return Some(state);
  }
  let wrapper = serde_json::from_str::<Value>(&body).ok()?;
  let state = wrapper.get("state")?;
  serde_json::from_value::<OtlpCircuitBreakerState>(state.clone()).ok()
}

fn load_persisted_breaker_state(
  key: &str,
  options: &OtlpHttpPushOptions,
) -> Option<OtlpCircuitBreakerState> {
  if let Some(path) = circuit_breaker_state_path(options) {
    return load_persisted_breakers_from_path(path).get(key).cloned();
  }
  if let Some(url) = circuit_breaker_state_url(options) {
    if options.circuit_breaker_state_patch {
      return load_persisted_breaker_from_url_patch(url, key, options);
    }
    return load_persisted_breakers_from_url(url, options)
      .get(key)
      .cloned();
  }
  None
}

fn persist_breakers_to_path(path: &str, states: &HashMap<String, OtlpCircuitBreakerState>) {
  let Ok(serialized) = serde_json::to_vec(states) else {
    return;
  };
  let _ = fs::write(path, serialized);
}

fn persist_breakers_to_url(
  url: &str,
  options: &OtlpHttpPushOptions,
  states: &HashMap<String, OtlpCircuitBreakerState>,
) {
  let Ok(serialized) = serde_json::to_vec(states) else {
    return;
  };
  let timeout = Duration::from_millis(options.timeout_ms.max(1));
  let Ok(agent) = build_otel_http_agent(url, options, timeout) else {
    return;
  };
  let mut request = agent
    .put(url)
    .set("content-type", "application/json")
    .timeout(timeout);
  if options.circuit_breaker_state_cas {
    if let Some(etag) = otlp_circuit_breaker_state_url_etags()
      .lock()
      .get(&circuit_breaker_state_url_etag_key(url, "doc", None))
      .cloned()
    {
      request = request.set("if-match", &etag);
    } else {
      request = request.set("if-match", "*");
    }
  }
  if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
    request = request.set("x-kitedb-breaker-lease", lease_id);
  }
  match request.send_bytes(&serialized) {
    Ok(response) => {
      if options.circuit_breaker_state_cas {
        if let Some(etag) = response.header("etag") {
          otlp_circuit_breaker_state_url_etags().lock().insert(
            circuit_breaker_state_url_etag_key(url, "doc", None),
            etag.to_string(),
          );
        }
      }
    }
    Err(ureq::Error::Status(status, response)) => {
      if options.circuit_breaker_state_cas && (status == 409 || status == 412) {
        if let Some(etag) = response.header("etag") {
          otlp_circuit_breaker_state_url_etags().lock().insert(
            circuit_breaker_state_url_etag_key(url, "doc", None),
            etag.to_string(),
          );
        }
      }
    }
    Err(_) => {}
  }
}

fn persist_breaker_to_url_patch(
  url: &str,
  key: &str,
  state: Option<&OtlpCircuitBreakerState>,
  options: &OtlpHttpPushOptions,
) {
  let payload = json!({
    "key": key,
    "state": state,
  });
  let Ok(serialized) = serde_json::to_vec(&payload) else {
    return;
  };
  let attempts = options
    .circuit_breaker_state_patch_retry_max_attempts
    .max(1);
  for attempt in 1..=attempts {
    let timeout = Duration::from_millis(options.timeout_ms.max(1));
    let Ok(agent) = build_otel_http_agent(url, options, timeout) else {
      return;
    };
    let mut request = agent
      .request("PATCH", url)
      .set("content-type", "application/json")
      .set("x-kitedb-breaker-mode", "patch-v1")
      .set("x-kitedb-breaker-key", key)
      .timeout(timeout);
    if options.circuit_breaker_state_cas {
      if let Some(etag) = otlp_circuit_breaker_state_url_etags()
        .lock()
        .get(&circuit_breaker_state_url_etag_key(url, "patch", Some(key)))
        .cloned()
      {
        request = request.set("if-match", &etag);
      } else {
        request = request.set("if-match", "*");
      }
    }
    if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
      request = request.set("x-kitedb-breaker-lease", lease_id);
    }
    match request.send_bytes(&serialized) {
      Ok(response) => {
        if options.circuit_breaker_state_cas {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "patch", Some(key)),
              etag.to_string(),
            );
          }
        }
        return;
      }
      Err(ureq::Error::Status(status, response)) => {
        if options.circuit_breaker_state_cas && (status == 409 || status == 412) {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "patch", Some(key)),
              etag.to_string(),
            );
          }
          if attempt < attempts {
            continue;
          }
        }
        return;
      }
      Err(_) => return,
    }
  }
}

fn persist_breakers_to_url_patch_batch(
  url: &str,
  primary_key: &str,
  states: &HashMap<String, OtlpCircuitBreakerState>,
  options: &OtlpHttpPushOptions,
) {
  let mut updates = Vec::new();
  let max_keys =
    usize::try_from(options.circuit_breaker_state_patch_batch_max_keys).unwrap_or(usize::MAX);
  if let Some(state) = states.get(primary_key) {
    updates.push(json!({ "key": primary_key, "state": state }));
  } else {
    updates.push(json!({ "key": primary_key, "state": Value::Null }));
  }
  if max_keys > 1 {
    for (key, state) in states {
      if key == primary_key {
        continue;
      }
      updates.push(json!({ "key": key, "state": state }));
      if updates.len() >= max_keys {
        break;
      }
    }
  }
  let payload = json!({ "updates": updates });
  let Ok(serialized) = serde_json::to_vec(&payload) else {
    return;
  };

  let attempts = options
    .circuit_breaker_state_patch_retry_max_attempts
    .max(1);
  for attempt in 1..=attempts {
    let timeout = Duration::from_millis(options.timeout_ms.max(1));
    let Ok(agent) = build_otel_http_agent(url, options, timeout) else {
      return;
    };
    let mut request = agent
      .request("PATCH", url)
      .set("content-type", "application/json")
      .set("x-kitedb-breaker-mode", "patch-batch-v1")
      .set("x-kitedb-breaker-key", primary_key)
      .timeout(timeout);
    if options.circuit_breaker_state_cas {
      if let Some(etag) = otlp_circuit_breaker_state_url_etags()
        .lock()
        .get(&circuit_breaker_state_url_etag_key(url, "batch", None))
        .cloned()
      {
        request = request.set("if-match", &etag);
      } else {
        request = request.set("if-match", "*");
      }
    }
    if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
      request = request.set("x-kitedb-breaker-lease", lease_id);
    }
    match request.send_bytes(&serialized) {
      Ok(response) => {
        if options.circuit_breaker_state_cas {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "batch", None),
              etag.to_string(),
            );
          }
        }
        return;
      }
      Err(ureq::Error::Status(status, response)) => {
        if options.circuit_breaker_state_cas && (status == 409 || status == 412) {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "batch", None),
              etag.to_string(),
            );
          }
          if attempt < attempts {
            continue;
          }
        }
        return;
      }
      Err(_) => return,
    }
  }
}

fn persist_breakers_to_url_patch_merge(
  url: &str,
  primary_key: &str,
  states: &HashMap<String, OtlpCircuitBreakerState>,
  options: &OtlpHttpPushOptions,
) {
  let mut updates = Vec::new();
  let max_keys =
    usize::try_from(options.circuit_breaker_state_patch_merge_max_keys).unwrap_or(usize::MAX);
  if let Some(state) = states.get(primary_key) {
    updates.push(json!({ "key": primary_key, "state": state }));
  } else {
    updates.push(json!({ "key": primary_key, "state": Value::Null }));
  }
  if max_keys > 1 {
    for (key, state) in states {
      if key == primary_key {
        continue;
      }
      updates.push(json!({ "key": key, "state": state }));
      if updates.len() >= max_keys {
        break;
      }
    }
  }
  let total_keys = states
    .len()
    .saturating_add(usize::from(!states.contains_key(primary_key)));
  let payload = json!({
    "scope_key": primary_key,
    "total_keys": total_keys,
    "truncated": total_keys > updates.len(),
    "updates": updates,
  });
  let Ok(serialized) = serde_json::to_vec(&payload) else {
    return;
  };

  let attempts = options
    .circuit_breaker_state_patch_retry_max_attempts
    .max(1);
  for attempt in 1..=attempts {
    let timeout = Duration::from_millis(options.timeout_ms.max(1));
    let Ok(agent) = build_otel_http_agent(url, options, timeout) else {
      return;
    };
    let mut request = agent
      .request("PATCH", url)
      .set("content-type", "application/json")
      .set("x-kitedb-breaker-mode", "patch-merge-v1")
      .set("x-kitedb-breaker-key", primary_key)
      .timeout(timeout);
    if options.circuit_breaker_state_cas {
      if let Some(etag) = otlp_circuit_breaker_state_url_etags()
        .lock()
        .get(&circuit_breaker_state_url_etag_key(url, "merge", None))
        .cloned()
      {
        request = request.set("if-match", &etag);
      } else {
        request = request.set("if-match", "*");
      }
    }
    if let Some(lease_id) = options.circuit_breaker_state_lease_id.as_deref() {
      request = request.set("x-kitedb-breaker-lease", lease_id);
    }
    match request.send_bytes(&serialized) {
      Ok(response) => {
        if options.circuit_breaker_state_cas {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "merge", None),
              etag.to_string(),
            );
          }
        }
        return;
      }
      Err(ureq::Error::Status(status, response)) => {
        if options.circuit_breaker_state_cas && (status == 409 || status == 412) {
          if let Some(etag) = response.header("etag") {
            otlp_circuit_breaker_state_url_etags().lock().insert(
              circuit_breaker_state_url_etag_key(url, "merge", None),
              etag.to_string(),
            );
          }
          if attempt < attempts {
            continue;
          }
        }
        return;
      }
      Err(_) => return,
    }
  }
}

fn persist_breakers(
  options: &OtlpHttpPushOptions,
  key: &str,
  states: &HashMap<String, OtlpCircuitBreakerState>,
) {
  if let Some(path) = circuit_breaker_state_path(options) {
    persist_breakers_to_path(path, states);
  } else if let Some(url) = circuit_breaker_state_url(options) {
    if options.circuit_breaker_state_patch {
      if options.circuit_breaker_state_patch_merge {
        persist_breakers_to_url_patch_merge(url, key, states, options);
      } else if options.circuit_breaker_state_patch_batch {
        persist_breakers_to_url_patch_batch(url, key, states, options);
      } else {
        persist_breaker_to_url_patch(url, key, states.get(key), options);
      }
    } else {
      persist_breakers_to_url(url, options, states);
    }
  }
}

fn merge_persisted_breaker_state(
  key: &str,
  persisted_state: Option<OtlpCircuitBreakerState>,
  states: &mut HashMap<String, OtlpCircuitBreakerState>,
) {
  let Some(persisted_state) = persisted_state else {
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
  let persisted_state = load_persisted_breaker_state(&key, options);
  let mut states = otlp_circuit_breakers().lock();
  merge_persisted_breaker_state(&key, persisted_state, &mut states);
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

/// A push the circuit breaker let through. `record` reports its outcome. A
/// push dropped unrecorded never reached the endpoint (a setup error after
/// admission), so the half-open probe it may hold goes back to the breaker;
/// kept in flight, it would refuse every later push.
#[must_use = "record the push outcome with `record`"]
struct BreakerAdmission<'a> {
  endpoint: &'a str,
  options: &'a OtlpHttpPushOptions,
  half_open_probe: bool,
}

impl BreakerAdmission<'_> {
  fn record<T>(mut self, result: Result<T>) -> Result<T> {
    self.half_open_probe = false;
    match &result {
      Ok(_) => record_circuit_breaker_success(self.endpoint, self.options),
      Err(_) => record_circuit_breaker_failure(self.endpoint, self.options),
    }
    result
  }
}

impl Drop for BreakerAdmission<'_> {
  fn drop(&mut self) {
    if self.half_open_probe {
      release_half_open_probe(self.endpoint, self.options);
    }
  }
}

/// Admit a push unless the breaker is open or its half-open probe is taken.
/// Call it right before the first request: every error after it must reach
/// the endpoint or the admission's drop.
fn check_circuit_breaker_open<'a>(
  endpoint: &'a str,
  options: &'a OtlpHttpPushOptions,
) -> Result<BreakerAdmission<'a>> {
  let mut admission = BreakerAdmission {
    endpoint,
    options,
    half_open_probe: false,
  };
  if options.circuit_breaker_failure_threshold == 0 {
    return Ok(admission);
  }
  let key = circuit_breaker_key(endpoint, options);
  let now = circuit_breaker_now_ms();
  let persisted_state = load_persisted_breaker_state(&key, options);
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, persisted_state, &mut states);
    let Some(state) = states.get_mut(&key) else {
      return Ok(admission);
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
      admission.half_open_probe = true;
      changed = true;
    }

    if changed {
      Some(states.clone())
    } else {
      None
    }
  };
  if let Some(snapshot) = snapshot {
    persist_breakers(options, &key, &snapshot);
  }
  Ok(admission)
}

/// Return an unused half-open probe: the next push may probe instead.
fn release_half_open_probe(endpoint: &str, options: &OtlpHttpPushOptions) {
  let key = circuit_breaker_key(endpoint, options);
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    let Some(state) = states.get_mut(&key) else {
      return;
    };
    if !state.half_open_in_flight {
      return;
    }
    state.half_open_in_flight = false;
    state.half_open_remaining_probes = state.half_open_remaining_probes.saturating_add(1);
    states.clone()
  };
  persist_breakers(options, &key, &snapshot);
}

fn record_circuit_breaker_success(endpoint: &str, options: &OtlpHttpPushOptions) {
  if options.circuit_breaker_failure_threshold == 0 && !options.adaptive_retry {
    return;
  }
  let key = circuit_breaker_key(endpoint, options);
  let persisted_state = load_persisted_breaker_state(&key, options);
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, persisted_state, &mut states);
    let state = states.entry(key.clone()).or_default();
    let alpha = options.adaptive_retry_ewma_alpha.clamp(0.0, 1.0);
    state.ewma_error_score = ((1.0 - alpha) * state.ewma_error_score).clamp(0.0, 1.0);
    state.consecutive_failures = 0;
    state.open_until_ms = 0;
    state.half_open_in_flight = false;
    let quiescent = state.consecutive_failures == 0
      && state.open_until_ms == 0
      && state.half_open_remaining_probes == 0
      && !state.half_open_in_flight;
    // Adaptive retry additionally keeps the entry while an EWMA error score
    // remains, so its decay continues to influence future retries.
    if quiescent && (!options.adaptive_retry || state.ewma_error_score <= f64::EPSILON) {
      states.remove(&key);
    }
    states.clone()
  };
  persist_breakers(options, &key, &snapshot);
}

fn record_circuit_breaker_failure(endpoint: &str, options: &OtlpHttpPushOptions) {
  if (options.circuit_breaker_failure_threshold == 0 || options.circuit_breaker_open_ms == 0)
    && !options.adaptive_retry
  {
    return;
  }
  let key = circuit_breaker_key(endpoint, options);
  let now = circuit_breaker_now_ms();
  let persisted_state = load_persisted_breaker_state(&key, options);
  let snapshot = {
    let mut states = otlp_circuit_breakers().lock();
    merge_persisted_breaker_state(&key, persisted_state, &mut states);
    let state = states.entry(key.clone()).or_default();
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
  persist_breakers(options, &key, &snapshot);
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

/// TLS file paths from the push options, checked against the endpoint.
#[derive(Debug, Clone, Copy)]
struct OtlpTlsPaths<'a> {
  ca_cert: Option<&'a str>,
  /// Client certificate and key, for mTLS.
  client_identity: Option<(&'a str, &'a str)>,
}

impl<'a> OtlpTlsPaths<'a> {
  fn from_options(endpoint: &str, options: &'a OtlpHttpPushOptions) -> Result<Self> {
    let path = |value: &'a Option<String>| {
      value
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    };
    let client_identity = match (
      path(&options.tls.client_cert_pem_path),
      path(&options.tls.client_key_pem_path),
    ) {
      (Some(cert), Some(key)) => Some((cert, key)),
      (None, None) => None,
      _ => {
        return Err(KiteError::InvalidQuery(
          "OTLP mTLS requires both client_cert_pem_path and client_key_pem_path".into(),
        ))
      }
    };
    let paths = Self {
      ca_cert: path(&options.tls.ca_cert_pem_path),
      client_identity,
    };
    if paths.custom() && !endpoint_uses_https(endpoint) {
      return Err(KiteError::InvalidQuery(
        "OTLP custom TLS/mTLS configuration requires an https endpoint".into(),
      ));
    }
    Ok(paths)
  }

  fn custom(&self) -> bool {
    self.ca_cert.is_some() || self.client_identity.is_some()
  }
}

fn build_otel_http_agent(
  endpoint: &str,
  options: &OtlpHttpPushOptions,
  timeout: Duration,
) -> Result<ureq::Agent> {
  let tls = OtlpTlsPaths::from_options(endpoint, options)?;

  let mut builder = ureq::builder()
    .https_only(options.tls.https_only)
    .timeout_connect(timeout)
    .timeout_read(timeout)
    .timeout_write(timeout);

  if tls.custom() {
    let mut root_store = ureq::rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if let Some(path) = tls.ca_cert {
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
    let client_config = if let Some((cert_path, key_path)) = tls.client_identity {
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
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.sidecar_needs_repair",
      "Whether the primary sidecar is fenced and requires repair or resync.",
      "1",
      if primary.sidecar_needs_repair { 1 } else { 0 },
      &[],
      time_unix_nano,
    ));
    otel_metrics.push(otel_proto_gauge_metric(
      "kitedb.replication.primary.last_replication_error_present",
      "Whether the primary currently has a non-empty replication error.",
      "1",
      if primary.last_replication_error.is_some() {
        1
      } else {
        0
      },
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
