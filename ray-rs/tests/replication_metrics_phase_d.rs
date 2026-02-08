use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::metrics::{
  collect_metrics_single_file, collect_replication_metrics_otel_json_single_file,
  collect_replication_metrics_otel_protobuf_single_file,
  collect_replication_metrics_prometheus_single_file, push_replication_metrics_otel_grpc_payload,
  push_replication_metrics_otel_grpc_payload_with_options,
  push_replication_metrics_otel_json_payload,
  push_replication_metrics_otel_json_payload_with_options,
  push_replication_metrics_otel_protobuf_payload, render_replication_metrics_prometheus,
  OtlpAdaptiveRetryMode, OtlpHttpPushOptions, OtlpHttpTlsOptions,
};
use kitedb::replication::types::ReplicationRole;
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::{
  MetricsService as OtelMetricsService, MetricsServiceServer as OtelMetricsServiceServer,
};
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest as OtelExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceResponse as OtelExportMetricsServiceResponse;
use prost::Message;

fn open_primary(
  path: &std::path::Path,
  sidecar: &std::path::Path,
  segment_max_bytes: u64,
  retention_min_entries: u64,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Primary)
      .replication_sidecar_path(sidecar)
      .replication_segment_max_bytes(segment_max_bytes)
      .replication_retention_min_entries(retention_min_entries),
  )
}

fn open_replica(
  replica_path: &std::path::Path,
  source_db_path: &std::path::Path,
  local_sidecar: &std::path::Path,
  source_sidecar: &std::path::Path,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    replica_path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_sidecar_path(local_sidecar)
      .replication_source_db_path(source_db_path)
      .replication_source_sidecar_path(source_sidecar),
  )
}

#[derive(Debug)]
struct CapturedHttpRequest {
  request_line: String,
  headers: HashMap<String, String>,
  body: Vec<u8>,
}

#[derive(Debug)]
struct CapturedGrpcRequest {
  authorization: Option<String>,
  resource_metrics_count: usize,
  attempt: usize,
}

#[derive(Debug)]
struct TestGrpcMetricsService {
  tx: Mutex<Option<mpsc::Sender<CapturedGrpcRequest>>>,
  fail_first_attempts: usize,
  attempts: AtomicUsize,
}

#[tonic::async_trait]
impl OtelMetricsService for TestGrpcMetricsService {
  async fn export(
    &self,
    request: tonic::Request<OtelExportMetricsServiceRequest>,
  ) -> std::result::Result<tonic::Response<OtelExportMetricsServiceResponse>, tonic::Status> {
    let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
    if attempt <= self.fail_first_attempts {
      return Err(tonic::Status::unavailable("transient"));
    }
    let authorization = request
      .metadata()
      .get("authorization")
      .and_then(|value| value.to_str().ok())
      .map(ToOwned::to_owned);
    if let Some(sender) = self.tx.lock().expect("lock capture sender").take() {
      sender
        .send(CapturedGrpcRequest {
          authorization,
          resource_metrics_count: request.get_ref().resource_metrics.len(),
          attempt,
        })
        .expect("send grpc capture");
    }
    Ok(tonic::Response::new(OtelExportMetricsServiceResponse {
      partial_success: None,
    }))
  }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  haystack
    .windows(needle.len())
    .position(|window| window == needle)
}

fn spawn_http_capture_server(
  status_code: u16,
  response_body: &str,
) -> (
  String,
  mpsc::Receiver<CapturedHttpRequest>,
  thread::JoinHandle<()>,
) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
  let address = listener.local_addr().expect("local addr");
  let endpoint = format!("http://{address}/v1/metrics");
  let response_body = response_body.to_string();
  let (tx, rx) = mpsc::channel::<CapturedHttpRequest>();

  let handle = thread::spawn(move || {
    let (mut stream, _) = listener.accept().expect("accept");
    stream
      .set_read_timeout(Some(Duration::from_secs(2)))
      .expect("set read timeout");

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut header_end: Option<usize> = None;
    let mut content_length = 0usize;

    loop {
      match stream.read(&mut chunk) {
        Ok(0) => break,
        Ok(read) => {
          buffer.extend_from_slice(&chunk[..read]);

          if header_end.is_none() {
            if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
              let end = position + 4;
              header_end = Some(end);
              let headers_text = String::from_utf8_lossy(&buffer[..end]);
              for line in headers_text.lines().skip(1) {
                let Some((name, value)) = line.split_once(':') else {
                  continue;
                };
                if name.eq_ignore_ascii_case("content-length") {
                  content_length = value.trim().parse::<usize>().unwrap_or(0);
                }
              }
            }
          }

          if let Some(end) = header_end {
            if buffer.len() >= end + content_length {
              break;
            }
          }
        }
        Err(error) => panic!("read request failed: {error}"),
      }
    }

    let end = header_end.expect("header terminator");
    let headers_text = String::from_utf8_lossy(&buffer[..end]);
    let mut lines = headers_text.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
      let Some((name, value)) = line.split_once(':') else {
        continue;
      };
      headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let body_end = (end + content_length).min(buffer.len());
    let body = buffer[end..body_end].to_vec();
    tx.send(CapturedHttpRequest {
      request_line,
      headers,
      body,
    })
    .expect("send captured request");

    let reason = if status_code == 200 { "OK" } else { "ERR" };
    let response = format!(
      "HTTP/1.1 {status_code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
      response_body.len(),
      response_body
    );
    stream
      .write_all(response.as_bytes())
      .expect("write response");
  });

  (endpoint, rx, handle)
}

fn spawn_http_sequence_capture_server(
  status_codes: Vec<u16>,
  response_body: &str,
) -> (
  String,
  mpsc::Receiver<Vec<CapturedHttpRequest>>,
  thread::JoinHandle<()>,
) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind sequence test server");
  let address = listener.local_addr().expect("sequence local addr");
  let endpoint = format!("http://{address}/v1/metrics");
  let response_body = response_body.to_string();
  let (tx, rx) = mpsc::channel::<Vec<CapturedHttpRequest>>();

  let handle = thread::spawn(move || {
    let mut captured = Vec::new();
    for status_code in status_codes {
      let (mut stream, _) = listener.accept().expect("accept sequence");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;

      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read sequence request failed: {error}"),
        }
      }

      let end = header_end.expect("header terminator");
      let headers_text = String::from_utf8_lossy(&buffer[..end]);
      let mut lines = headers_text.lines();
      let request_line = lines.next().unwrap_or_default().to_string();
      let mut headers = HashMap::new();
      for line in lines {
        let Some((name, value)) = line.split_once(':') else {
          continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
      }
      let body_end = (end + content_length).min(buffer.len());
      let body = buffer[end..body_end].to_vec();
      captured.push(CapturedHttpRequest {
        request_line,
        headers,
        body,
      });

      let reason = if status_code == 200 { "OK" } else { "ERR" };
      let response = format!(
        "HTTP/1.1 {status_code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
      );
      stream
        .write_all(response.as_bytes())
        .expect("write sequence response");
    }
    tx.send(captured).expect("send sequence captures");
  });

  (endpoint, rx, handle)
}

fn spawn_state_store_get_server(state_body: String) -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store");
  let address = listener.local_addr().expect("state store local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    let (mut stream, _) = listener.accept().expect("accept state store");
    stream
      .set_read_timeout(Some(Duration::from_secs(2)))
      .expect("set state store read timeout");
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
      match stream.read(&mut chunk) {
        Ok(0) => break,
        Ok(read) => {
          buffer.extend_from_slice(&chunk[..read]);
          if find_subsequence(&buffer, b"\r\n\r\n").is_some() {
            break;
          }
        }
        Err(error) => panic!("read state store request failed: {error}"),
      }
    }
    let request_text = String::from_utf8_lossy(&buffer);
    assert!(
      request_text.starts_with("GET /breaker-state HTTP/1.1"),
      "unexpected state store request: {request_text}"
    );
    let response = format!(
      "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
      state_body.len(),
      state_body
    );
    stream
      .write_all(response.as_bytes())
      .expect("write state store response");
  });
  (endpoint, handle)
}

fn spawn_state_store_roundtrip_server() -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store roundtrip");
  let address = listener
    .local_addr()
    .expect("state store roundtrip local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    let mut stored_state = "{}".to_string();
    for expected_method in ["GET", "GET", "PUT", "GET"] {
      let (mut stream, _) = listener.accept().expect("accept state store roundtrip");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set state store roundtrip read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;
      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read state store roundtrip request failed: {error}"),
        }
      }

      let end = header_end.expect("state store roundtrip header terminator");
      let request_text = String::from_utf8_lossy(&buffer[..end]);
      let request_line = request_text.lines().next().unwrap_or_default();
      assert!(
        request_line.starts_with(&format!("{expected_method} /breaker-state HTTP/1.1")),
        "unexpected state store roundtrip request line: {request_line}"
      );

      if expected_method == "PUT" {
        let body_end = (end + content_length).min(buffer.len());
        stored_state = String::from_utf8_lossy(&buffer[end..body_end]).to_string();
      }

      let response_body = if expected_method == "GET" {
        stored_state.clone()
      } else {
        String::new()
      };
      let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response_body.len(),
        response_body
      );
      stream
        .write_all(response.as_bytes())
        .expect("write state store roundtrip response");
    }
  });
  (endpoint, handle)
}

fn spawn_state_store_cas_lease_server(expected_lease: String) -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store cas lease");
  let address = listener
    .local_addr()
    .expect("state store cas lease local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    for expected_method in ["GET", "GET", "PUT"] {
      let (mut stream, _) = listener.accept().expect("accept state store cas lease");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set state store cas lease read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;
      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read state store cas lease request failed: {error}"),
        }
      }

      let end = header_end.expect("state store cas lease header terminator");
      let request_text = String::from_utf8_lossy(&buffer[..end]);
      let request_line = request_text.lines().next().unwrap_or_default();
      assert!(
        request_line.starts_with(&format!("{expected_method} /breaker-state HTTP/1.1")),
        "unexpected state store cas lease request line: {request_line}"
      );

      let mut headers = HashMap::new();
      for line in request_text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
          continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
      }

      let lease_header = headers
        .get("x-kitedb-breaker-lease")
        .map(String::as_str)
        .unwrap_or_default();
      assert_eq!(
        lease_header,
        expected_lease.as_str(),
        "lease header mismatch"
      );

      if expected_method == "PUT" {
        let if_match = headers
          .get("if-match")
          .map(String::as_str)
          .unwrap_or_default();
        assert_eq!(if_match, "v1", "if-match header mismatch");
      }

      let (status_line, etag, body) = if expected_method == "PUT" {
        ("HTTP/1.1 200 OK", "v2", "")
      } else {
        ("HTTP/1.1 200 OK", "v1", "{}")
      };
      let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      stream
        .write_all(response.as_bytes())
        .expect("write state store cas lease response");
    }
  });
  (endpoint, handle)
}

fn spawn_state_store_patch_server(expected_key: String) -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store patch");
  let address = listener.local_addr().expect("state store patch local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    for expected_method in ["GET", "GET", "PATCH"] {
      let (mut stream, _) = listener.accept().expect("accept state store patch");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set state store patch read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;
      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read state store patch request failed: {error}"),
        }
      }

      let end = header_end.expect("state store patch header terminator");
      let request_text = String::from_utf8_lossy(&buffer[..end]);
      let request_line = request_text.lines().next().unwrap_or_default();
      assert!(
        request_line.starts_with(&format!("{expected_method} /breaker-state HTTP/1.1")),
        "unexpected state store patch request line: {request_line}"
      );

      let mut headers = HashMap::new();
      for line in request_text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
          continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
      }

      assert_eq!(
        headers.get("x-kitedb-breaker-mode").map(String::as_str),
        Some("patch-v1"),
        "patch mode header mismatch"
      );
      assert_eq!(
        headers.get("x-kitedb-breaker-key").map(String::as_str),
        Some(expected_key.as_str()),
        "patch key header mismatch"
      );

      if expected_method == "PATCH" {
        let body_end = (end + content_length).min(buffer.len());
        let payload: serde_json::Value =
          serde_json::from_slice(&buffer[end..body_end]).expect("parse patch payload");
        assert_eq!(payload["key"].as_str(), Some(expected_key.as_str()));
        assert!(payload["state"].is_object(), "missing patch state object");
      }

      let (status_line, etag, body) = if expected_method == "PATCH" {
        ("HTTP/1.1 200 OK", "p2", "")
      } else {
        ("HTTP/1.1 200 OK", "p1", "{}")
      };
      let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      stream
        .write_all(response.as_bytes())
        .expect("write state store patch response");
    }
  });
  (endpoint, handle)
}

fn spawn_state_store_patch_retry_server(expected_key: String) -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store patch retry");
  let address = listener
    .local_addr()
    .expect("state store patch retry local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    let mut patch_attempts = 0usize;
    for expected_method in ["GET", "GET", "PATCH", "PATCH"] {
      let (mut stream, _) = listener.accept().expect("accept state store patch retry");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set state store patch retry read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;
      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read state store patch retry request failed: {error}"),
        }
      }

      let end = header_end.expect("state store patch retry header terminator");
      let request_text = String::from_utf8_lossy(&buffer[..end]);
      let request_line = request_text.lines().next().unwrap_or_default();
      assert!(
        request_line.starts_with(&format!("{expected_method} /breaker-state HTTP/1.1")),
        "unexpected state store patch retry request line: {request_line}"
      );

      let mut headers = HashMap::new();
      for line in request_text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
          continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
      }

      if expected_method == "PATCH" {
        assert_eq!(
          headers.get("x-kitedb-breaker-mode").map(String::as_str),
          Some("patch-v1"),
          "patch mode header mismatch"
        );
        assert_eq!(
          headers.get("x-kitedb-breaker-key").map(String::as_str),
          Some(expected_key.as_str()),
          "patch key header mismatch"
        );
        let if_match = headers
          .get("if-match")
          .map(String::as_str)
          .unwrap_or_default();
        patch_attempts = patch_attempts.saturating_add(1);
        if patch_attempts == 1 {
          assert_eq!(
            if_match, "pr1",
            "first patch if-match header should use GET ETag"
          );
          let response = "HTTP/1.1 412 Precondition Failed\r\nETag: pr2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
          stream
            .write_all(response.as_bytes())
            .expect("write patch retry precondition response");
          continue;
        }
        if patch_attempts == 2 {
          assert_eq!(if_match, "pr2", "retry if-match header mismatch");
        }
      }

      let (status_line, etag, body) = if expected_method == "GET" {
        ("HTTP/1.1 200 OK", "pr1", "{}")
      } else {
        ("HTTP/1.1 200 OK", "pr3", "")
      };
      let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      stream
        .write_all(response.as_bytes())
        .expect("write state store patch retry response");
    }
  });
  (endpoint, handle)
}

fn spawn_state_store_patch_batch_server(expected_key: String) -> (String, thread::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind state store patch batch");
  let address = listener
    .local_addr()
    .expect("state store patch batch local addr");
  let endpoint = format!("http://{address}/breaker-state");
  let handle = thread::spawn(move || {
    for expected_method in ["GET", "GET", "PATCH"] {
      let (mut stream, _) = listener.accept().expect("accept state store patch batch");
      stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set state store patch batch read timeout");

      let mut buffer = Vec::new();
      let mut chunk = [0u8; 1024];
      let mut header_end: Option<usize> = None;
      let mut content_length = 0usize;
      loop {
        match stream.read(&mut chunk) {
          Ok(0) => break,
          Ok(read) => {
            buffer.extend_from_slice(&chunk[..read]);
            if header_end.is_none() {
              if let Some(position) = find_subsequence(&buffer, b"\r\n\r\n") {
                let end = position + 4;
                header_end = Some(end);
                let headers_text = String::from_utf8_lossy(&buffer[..end]);
                for line in headers_text.lines().skip(1) {
                  let Some((name, value)) = line.split_once(':') else {
                    continue;
                  };
                  if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                  }
                }
              }
            }
            if let Some(end) = header_end {
              if buffer.len() >= end + content_length {
                break;
              }
            }
          }
          Err(error) => panic!("read state store patch batch request failed: {error}"),
        }
      }

      let end = header_end.expect("state store patch batch header terminator");
      let request_text = String::from_utf8_lossy(&buffer[..end]);
      let request_line = request_text.lines().next().unwrap_or_default();
      assert!(
        request_line.starts_with(&format!("{expected_method} /breaker-state HTTP/1.1")),
        "unexpected state store patch batch request line: {request_line}"
      );

      let mut headers = HashMap::new();
      for line in request_text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
          continue;
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
      }

      if expected_method == "PATCH" {
        assert_eq!(
          headers.get("x-kitedb-breaker-mode").map(String::as_str),
          Some("patch-batch-v1"),
          "patch batch mode header mismatch"
        );
        assert_eq!(
          headers.get("x-kitedb-breaker-key").map(String::as_str),
          Some(expected_key.as_str()),
          "patch batch key header mismatch"
        );
        let body_end = (end + content_length).min(buffer.len());
        let payload: serde_json::Value =
          serde_json::from_slice(&buffer[end..body_end]).expect("parse patch batch payload");
        let updates = payload["updates"].as_array().expect("updates array");
        assert!(!updates.is_empty(), "updates must not be empty");
        assert!(
          updates.len() <= 2,
          "updates should respect batch max keys, got {}",
          updates.len()
        );
        assert_eq!(
          updates[0]["key"].as_str(),
          Some(expected_key.as_str()),
          "primary key must be first update"
        );
      }

      let (status_line, etag, body) = if expected_method == "PATCH" {
        ("HTTP/1.1 200 OK", "pb2", "")
      } else {
        ("HTTP/1.1 200 OK", "pb1", "{}")
      };
      let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nETag: {etag}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
      );
      stream
        .write_all(response.as_bytes())
        .expect("write state store patch batch response");
    }
  });
  (endpoint, handle)
}

fn spawn_grpc_capture_server(
  fail_first_attempts: usize,
) -> (
  String,
  mpsc::Receiver<CapturedGrpcRequest>,
  tokio::sync::oneshot::Sender<()>,
  thread::JoinHandle<()>,
) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind grpc test server");
  let address = listener.local_addr().expect("grpc local addr");
  drop(listener);
  let endpoint = format!("http://{address}");
  let (tx, rx) = mpsc::channel::<CapturedGrpcRequest>();
  let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

  let handle = thread::spawn(move || {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("create grpc runtime");
    runtime.block_on(async move {
      let service = TestGrpcMetricsService {
        tx: Mutex::new(Some(tx)),
        fail_first_attempts,
        attempts: AtomicUsize::new(0),
      };
      tonic::transport::Server::builder()
        .add_service(OtelMetricsServiceServer::new(service))
        .serve_with_shutdown(address, async move {
          let _ = shutdown_rx.await;
        })
        .await
        .expect("serve grpc test endpoint");
    });
  });

  (endpoint, rx, shutdown_tx, handle)
}

#[test]
fn collect_metrics_exposes_primary_replication_fields() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("replication-metrics-primary.kitedb");
  let sidecar = dir.path().join("replication-metrics-primary.sidecar");

  let primary = open_primary(&db_path, &sidecar, 1, 2).expect("open primary");

  for i in 0..4 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("p-{i}")))
      .expect("create node");
    let _ = primary.commit_with_token().expect("commit").expect("token");
  }

  primary
    .primary_report_replica_progress("replica-a", 1, 2)
    .expect("report replica progress");

  let metrics = collect_metrics_single_file(&primary);
  let otel = collect_replication_metrics_otel_json_single_file(&primary);
  let otel_protobuf = collect_replication_metrics_otel_protobuf_single_file(&primary);
  let prometheus = collect_replication_metrics_prometheus_single_file(&primary);
  assert!(metrics.replication.enabled);
  assert_eq!(metrics.replication.role, "primary");
  assert!(metrics.replication.replica.is_none());

  let repl = metrics
    .replication
    .primary
    .as_ref()
    .expect("primary replication metrics");
  assert_eq!(repl.epoch, 1);
  assert_eq!(repl.replica_count, 1);
  assert_eq!(repl.stale_epoch_replica_count, 0);
  assert_eq!(repl.min_replica_applied_log_index, Some(2));
  assert_eq!(repl.max_replica_lag, repl.head_log_index.saturating_sub(2));
  assert!(repl.append_attempts >= repl.append_successes);
  assert_eq!(repl.append_failures, 0);
  assert!(repl.append_successes >= 4);
  assert!(repl.last_token.is_some());
  assert!(repl
    .sidecar_path
    .ends_with("replication-metrics-primary.sidecar"));
  assert!(prometheus.contains("# HELP kitedb_replication_enabled"));
  assert!(prometheus.contains("kitedb_replication_enabled{role=\"primary\"} 1"));
  assert!(prometheus.contains("kitedb_replication_primary_head_log_index"));
  assert!(prometheus.contains("kitedb_replication_primary_append_attempts_total"));
  assert!(otel.contains("\"kitedb.replication.enabled\""));
  assert!(otel.contains("\"kitedb.replication.primary.head_log_index\""));
  assert!(otel.contains("\"kitedb.replication.primary.append_attempts\""));
  let otel_json: serde_json::Value = serde_json::from_str(&otel).expect("parse otel json");
  assert!(otel_json["resourceMetrics"]
    .as_array()
    .map(|values| !values.is_empty())
    .unwrap_or(false));
  let otel_proto = OtelExportMetricsServiceRequest::decode(otel_protobuf.as_slice())
    .expect("decode otel protobuf request");
  assert_eq!(otel_proto.resource_metrics.len(), 1);
  let metric_names = otel_proto.resource_metrics[0]
    .scope_metrics
    .iter()
    .flat_map(|scope| scope.metrics.iter().map(|metric| metric.name.clone()))
    .collect::<Vec<_>>();
  assert!(metric_names
    .iter()
    .any(|name| name == "kitedb.replication.enabled"));
  assert!(metric_names
    .iter()
    .any(|name| name == "kitedb.replication.primary.head_log_index"));
  assert!(metric_names
    .iter()
    .any(|name| name == "kitedb.replication.primary.append_attempts"));

  close_single_file(primary).expect("close primary");
}

#[test]
fn collect_metrics_exposes_replica_reseed_error_state() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir
    .path()
    .join("replication-metrics-replica-primary.kitedb");
  let primary_sidecar = dir
    .path()
    .join("replication-metrics-replica-primary.sidecar");
  let replica_path = dir.path().join("replication-metrics-replica.kitedb");
  let replica_sidecar = dir.path().join("replication-metrics-replica.sidecar");

  let primary = open_primary(&primary_path, &primary_sidecar, 1, 2).expect("open primary");

  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(
    &replica_path,
    &primary_path,
    &replica_sidecar,
    &primary_sidecar,
  )
  .expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap replica");

  for i in 0..5 {
    primary.begin(false).expect("begin");
    primary
      .create_node(Some(&format!("r-{i}")))
      .expect("create");
    primary.commit_with_token().expect("commit").expect("token");
  }

  primary
    .primary_report_replica_progress("replica-r", 1, 1)
    .expect("report lagging replica");
  let _ = primary.primary_run_retention().expect("run retention");

  let err = replica
    .replica_catch_up_once(32)
    .expect_err("must need reseed");
  assert!(err.to_string().contains("reseed"));

  let metrics = collect_metrics_single_file(&replica);
  let otel = collect_replication_metrics_otel_json_single_file(&replica);
  let prometheus = render_replication_metrics_prometheus(&metrics);
  assert!(metrics.replication.enabled);
  assert_eq!(metrics.replication.role, "replica");
  assert!(metrics.replication.primary.is_none());

  let repl = metrics
    .replication
    .replica
    .as_ref()
    .expect("replica replication metrics");
  assert!(repl.needs_reseed);
  assert!(
    repl
      .last_error
      .as_deref()
      .unwrap_or_default()
      .contains("reseed"),
    "unexpected last_error: {:?}",
    repl.last_error
  );
  assert!(prometheus.contains("kitedb_replication_enabled{role=\"replica\"} 1"));
  assert!(prometheus.contains("kitedb_replication_replica_needs_reseed 1"));
  assert!(prometheus.contains("kitedb_replication_replica_last_error_present 1"));
  assert!(otel.contains("\"kitedb.replication.replica.needs_reseed\""));
  assert!(otel.contains("\"kitedb.replication.replica.last_error_present\""));

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn replication_prometheus_export_reports_disabled_role() {
  let dir = tempfile::tempdir().expect("tempdir");
  let db_path = dir.path().join("replication-metrics-disabled.kitedb");
  let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("open db");

  let metrics = collect_metrics_single_file(&db);
  let otel = collect_replication_metrics_otel_json_single_file(&db);
  let prometheus = render_replication_metrics_prometheus(&metrics);
  assert!(!metrics.replication.enabled);
  assert_eq!(metrics.replication.role, "disabled");
  assert!(prometheus.contains("kitedb_replication_enabled{role=\"disabled\"} 0"));
  assert!(prometheus.contains("kitedb_replication_auth_enabled 0"));
  assert!(otel.contains("\"kitedb.replication.enabled\""));
  assert!(otel.contains("\"role\""));
  assert!(otel.contains("\"disabled\""));

  close_single_file(db).expect("close db");
}

#[test]
fn otlp_push_payload_validates_endpoint_and_timeout() {
  let endpoint_err = push_replication_metrics_otel_json_payload("{}", " ", 1000, None)
    .expect_err("empty endpoint must fail");
  assert!(endpoint_err.to_string().contains("endpoint"));

  let timeout_err =
    push_replication_metrics_otel_json_payload("{}", "http://127.0.0.1:1/v1/metrics", 0, None)
      .expect_err("zero timeout must fail");
  assert!(timeout_err.to_string().contains("timeout_ms"));
}

#[test]
fn otlp_push_payload_posts_json_and_auth_header() {
  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) = spawn_http_capture_server(200, "ok");

  let result = push_replication_metrics_otel_json_payload(payload, &endpoint, 2_000, Some("token"))
    .expect("otlp push must succeed");
  assert_eq!(result.status_code, 200);
  assert_eq!(result.response_body, "ok");

  let captured = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured request");
  assert_eq!(captured.request_line, "POST /v1/metrics HTTP/1.1");
  assert_eq!(
    captured.headers.get("content-type").map(String::as_str),
    Some("application/json")
  );
  assert_eq!(
    captured.headers.get("authorization").map(String::as_str),
    Some("Bearer token")
  );
  assert_eq!(String::from_utf8_lossy(&captured.body), payload);

  handle.join().expect("server thread");
}

#[test]
fn otlp_push_protobuf_payload_posts_binary_and_auth_header() {
  let payload = vec![0x0a, 0x03, 0x66, 0x6f, 0x6f];
  let (endpoint, captured_rx, handle) = spawn_http_capture_server(200, "ok");

  let result =
    push_replication_metrics_otel_protobuf_payload(&payload, &endpoint, 2_000, Some("token"))
      .expect("otlp protobuf push must succeed");
  assert_eq!(result.status_code, 200);
  assert_eq!(result.response_body, "ok");

  let captured = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured request");
  assert_eq!(captured.request_line, "POST /v1/metrics HTTP/1.1");
  assert_eq!(
    captured.headers.get("content-type").map(String::as_str),
    Some("application/x-protobuf")
  );
  assert_eq!(
    captured.headers.get("authorization").map(String::as_str),
    Some("Bearer token")
  );
  assert_eq!(captured.body, payload);

  handle.join().expect("server thread");
}

#[test]
fn otlp_push_payload_retries_transient_http_failure() {
  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) = spawn_http_sequence_capture_server(vec![500, 200], "ok");
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 2,
    retry_backoff_ms: 1,
    retry_backoff_max_ms: 1,
    ..OtlpHttpPushOptions::default()
  };

  let result =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect("second attempt should succeed");
  assert_eq!(result.status_code, 200);
  assert_eq!(result.response_body, "ok");

  let captures = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured sequence requests");
  assert_eq!(captures.len(), 2);
  assert_eq!(
    String::from_utf8_lossy(&captures[0].body),
    payload,
    "first attempt payload mismatch"
  );
  assert_eq!(
    String::from_utf8_lossy(&captures[1].body),
    payload,
    "second attempt payload mismatch"
  );
  handle.join().expect("sequence server thread");
}

#[test]
fn otlp_push_payload_gzip_sets_header_and_compresses_body() {
  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) = spawn_http_capture_server(200, "ok");
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    compression_gzip: true,
    ..OtlpHttpPushOptions::default()
  };

  let result =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect("gzip push should succeed");
  assert_eq!(result.status_code, 200);

  let captured = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured gzip request");
  assert_eq!(
    captured.headers.get("content-encoding").map(String::as_str),
    Some("gzip")
  );
  assert!(
    captured.body.starts_with(&[0x1f, 0x8b]),
    "expected gzip magic bytes"
  );
  handle.join().expect("server thread");
}

#[test]
fn otlp_push_payload_returns_error_on_non_success_status() {
  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, _captured_rx, handle) = spawn_http_capture_server(401, "denied");

  let error = push_replication_metrics_otel_json_payload(payload, &endpoint, 2_000, None)
    .expect_err("non-2xx must fail");
  let message = error.to_string();
  assert!(
    message.contains("status 401"),
    "unexpected error: {message}"
  );
  assert!(message.contains("denied"), "unexpected error: {message}");

  handle.join().expect("server thread");
}

#[test]
fn otlp_push_payload_rejects_https_only_http_endpoint() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    bearer_token: None,
    tls: OtlpHttpTlsOptions {
      https_only: true,
      ..OtlpHttpTlsOptions::default()
    },
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("https_only should reject http endpoint");
  assert!(error.to_string().contains("https"));
}

#[test]
fn otlp_push_payload_rejects_partial_mtls_paths() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    bearer_token: None,
    tls: OtlpHttpTlsOptions {
      client_cert_pem_path: Some("/tmp/client.crt".to_string()),
      client_key_pem_path: None,
      ..OtlpHttpTlsOptions::default()
    },
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "https://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("partial mTLS path configuration should fail");
  assert!(error.to_string().contains("client_cert_pem_path"));
  assert!(error.to_string().contains("client_key_pem_path"));
}

#[test]
fn otlp_push_payload_rejects_zero_retry_attempts() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 0,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("zero retry attempts must be rejected");
  assert!(error.to_string().contains("retry_max_attempts"));
}

#[test]
fn otlp_push_payload_rejects_invalid_retry_jitter_ratio() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_jitter_ratio: 1.5,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("invalid jitter ratio must fail");
  assert!(error.to_string().contains("retry_jitter_ratio"));
}

#[test]
fn otlp_push_payload_rejects_invalid_adaptive_retry_ewma_alpha() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    adaptive_retry: true,
    adaptive_retry_mode: OtlpAdaptiveRetryMode::Ewma,
    adaptive_retry_ewma_alpha: 1.5,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("invalid adaptive ewma alpha must fail");
  assert!(error.to_string().contains("adaptive_retry_ewma_alpha"));
}

#[test]
fn otlp_push_payload_rejects_zero_half_open_probes_when_breaker_enabled() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 1_000,
    circuit_breaker_half_open_probes: 0,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("zero half-open probes must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_half_open_probes"));
}

#[test]
fn otlp_push_payload_rejects_conflicting_breaker_state_backends() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_path: Some("/tmp/otlp-breaker-state.json".to_string()),
    circuit_breaker_state_url: Some("http://127.0.0.1:4318/state".to_string()),
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("conflicting state backend options must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_path and circuit_breaker_state_url"));
}

#[test]
fn otlp_push_payload_rejects_state_cas_without_url() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_cas: true,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state cas without url must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_cas requires circuit_breaker_state_url"));
}

#[test]
fn otlp_push_payload_rejects_state_patch_without_url() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_patch: true,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state patch without url must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_patch requires circuit_breaker_state_url"));
}

#[test]
fn otlp_push_payload_rejects_state_patch_batch_without_patch() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_url: Some("http://127.0.0.1:4318/state".to_string()),
    circuit_breaker_state_patch_batch: true,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state patch batch without patch mode must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_patch_batch requires circuit_breaker_state_patch"));
}

#[test]
fn otlp_push_payload_rejects_state_patch_batch_max_keys_zero() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_url: Some("http://127.0.0.1:4318/state".to_string()),
    circuit_breaker_state_patch: true,
    circuit_breaker_state_patch_batch_max_keys: 0,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state patch batch max keys zero must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_patch_batch_max_keys"));
}

#[test]
fn otlp_push_payload_rejects_state_patch_retry_max_attempts_zero() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_url: Some("http://127.0.0.1:4318/state".to_string()),
    circuit_breaker_state_patch: true,
    circuit_breaker_state_patch_retry_max_attempts: 0,
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state patch retry max attempts zero must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_patch_retry_max_attempts"));
}

#[test]
fn otlp_push_payload_rejects_state_lease_without_url() {
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    circuit_breaker_state_lease_id: Some("lease-a".to_string()),
    ..OtlpHttpPushOptions::default()
  };
  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:4318/v1/metrics",
    &options,
  )
  .expect_err("state lease without url must fail");
  assert!(error
    .to_string()
    .contains("circuit_breaker_state_lease_id requires circuit_breaker_state_url"));
}

#[test]
fn otlp_push_payload_circuit_breaker_opens_after_failure() {
  let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
  let port = probe.local_addr().expect("probe addr").port();
  drop(probe);
  let endpoint = format!("http://127.0.0.1:{port}/v1/metrics");
  let options = OtlpHttpPushOptions {
    timeout_ms: 100,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 50,
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options("{}", &endpoint, &options)
    .expect_err("first call should fail transport");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  let second = push_replication_metrics_otel_json_payload_with_options("{}", &endpoint, &options)
    .expect_err("second call should be blocked by circuit breaker");
  assert!(
    second.to_string().contains("circuit breaker open"),
    "unexpected second error: {second}"
  );

  thread::sleep(Duration::from_millis(70));
  let third = push_replication_metrics_otel_json_payload_with_options("{}", &endpoint, &options)
    .expect_err("third call should attempt again after breaker window");
  assert!(
    !third.to_string().contains("circuit breaker open"),
    "breaker should have closed, got: {third}"
  );
}

#[test]
fn otlp_push_payload_half_open_probes_gate_recovery() {
  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) =
    spawn_http_sequence_capture_server(vec![500, 200, 200, 500], "ok");
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 1,
    retry_backoff_ms: 1,
    retry_backoff_max_ms: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 50,
    circuit_breaker_half_open_probes: 2,
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
    .expect_err("first call should open breaker");
  assert!(first.to_string().contains("status 500"));

  let second =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect_err("breaker should block while open");
  assert!(second.to_string().contains("circuit breaker open"));

  thread::sleep(Duration::from_millis(70));
  let third = push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
    .expect("first half-open probe should pass");
  assert_eq!(third.status_code, 200);

  let fourth =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect("second half-open probe should pass");
  assert_eq!(fourth.status_code, 200);

  let fifth = push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
    .expect_err("fifth call should hit configured server failure");
  assert!(
    !fifth.to_string().contains("circuit breaker open"),
    "expected call to be attempted after successful probes, got: {fifth}"
  );

  let captures = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured half-open requests");
  assert_eq!(
    captures.len(),
    4,
    "blocked open-window call should not hit endpoint"
  );
  handle.join().expect("half-open sequence server thread");
}

#[test]
fn otlp_push_payload_uses_persisted_shared_circuit_breaker_state() {
  let dir = tempfile::tempdir().expect("tempdir");
  let state_path = dir.path().join("otlp-breaker-state.json");
  let now_ms = SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64;
  let state_json = serde_json::json!({
    "shared-breaker": {
      "consecutive_failures": 0,
      "open_until_ms": now_ms + 5_000
    }
  });
  std::fs::write(
    &state_path,
    serde_json::to_vec(&state_json).expect("serialize state"),
  )
  .expect("write state");

  let options = OtlpHttpPushOptions {
    timeout_ms: 100,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 500,
    circuit_breaker_state_path: Some(state_path.to_string_lossy().to_string()),
    circuit_breaker_scope_key: Some("shared-breaker".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("persisted open breaker should block request");
  assert!(
    error.to_string().contains("circuit breaker open"),
    "unexpected error: {error}"
  );
}

#[test]
fn otlp_push_payload_uses_shared_circuit_breaker_state_url() {
  let now_ms = SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64;
  let state_json = serde_json::json!({
    "shared-breaker-url": {
      "consecutive_failures": 0,
      "open_until_ms": now_ms + 5_000
    }
  })
  .to_string();
  let (state_url, state_handle) = spawn_state_store_get_server(state_json);

  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 500,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_scope_key: Some("shared-breaker-url".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let error = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("remote shared open breaker should block request");
  assert!(
    error.to_string().contains("circuit breaker open"),
    "unexpected error: {error}"
  );
  state_handle.join().expect("state store thread");
}

#[test]
fn otlp_push_payload_shared_state_url_roundtrips_failure_open_state() {
  let (state_url, state_handle) = spawn_state_store_roundtrip_server();
  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_scope_key: Some("shared-roundtrip-breaker".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("first call should fail transport and persist open state");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  let second = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("second call should be blocked by state loaded from shared url");
  assert!(
    second.to_string().contains("circuit breaker open"),
    "unexpected second error: {second}"
  );

  state_handle.join().expect("state store roundtrip thread");
}

#[test]
fn otlp_push_payload_shared_state_url_applies_cas_and_lease_headers() {
  let (state_url, state_handle) = spawn_state_store_cas_lease_server("lease-cas-a".to_string());
  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_state_cas: true,
    circuit_breaker_state_lease_id: Some("lease-cas-a".to_string()),
    circuit_breaker_scope_key: Some("shared-cas-breaker".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("first call should fail transport and persist with CAS");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  state_handle.join().expect("state store cas lease thread");
}

#[test]
fn otlp_push_payload_shared_state_url_patch_protocol_uses_key_scoped_updates() {
  let scope_key = "shared-patch-breaker";
  let (state_url, state_handle) = spawn_state_store_patch_server(scope_key.to_string());
  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_state_patch: true,
    circuit_breaker_scope_key: Some(scope_key.to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("first call should fail transport and persist key-scoped patch");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  state_handle.join().expect("state store patch thread");
}

#[test]
fn otlp_push_payload_shared_state_url_patch_protocol_retries_on_precondition_failure() {
  let scope_key = "shared-patch-retry-breaker";
  let (state_url, state_handle) = spawn_state_store_patch_retry_server(scope_key.to_string());
  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_state_patch: true,
    circuit_breaker_state_cas: true,
    circuit_breaker_state_patch_retry_max_attempts: 2,
    circuit_breaker_scope_key: Some(scope_key.to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("first call should fail transport and persist key-scoped patch with retry");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  state_handle.join().expect("state store patch retry thread");
}

#[test]
fn otlp_push_payload_shared_state_url_patch_batch_protocol_uses_multi_key_updates() {
  let scope_key = "shared-patch-batch-breaker";
  let (state_url, state_handle) = spawn_state_store_patch_batch_server(scope_key.to_string());
  let options = OtlpHttpPushOptions {
    timeout_ms: 200,
    retry_max_attempts: 1,
    circuit_breaker_failure_threshold: 1,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_url: Some(state_url),
    circuit_breaker_state_patch: true,
    circuit_breaker_state_patch_batch: true,
    circuit_breaker_state_patch_batch_max_keys: 2,
    circuit_breaker_scope_key: Some(scope_key.to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let first = push_replication_metrics_otel_json_payload_with_options(
    "{}",
    "http://127.0.0.1:9/v1/metrics",
    &options,
  )
  .expect_err("first call should fail transport and persist patch batch update");
  assert!(
    first.to_string().contains("transport"),
    "unexpected first error: {first}"
  );

  state_handle.join().expect("state store patch batch thread");
}

#[test]
fn otlp_push_payload_adaptive_retry_uses_failure_history() {
  let dir = tempfile::tempdir().expect("tempdir");
  let state_path = dir.path().join("otlp-adaptive-state.json");
  let state_json = serde_json::json!({
    "adaptive-breaker": {
      "consecutive_failures": 4,
      "open_until_ms": 0
    }
  });
  std::fs::write(
    &state_path,
    serde_json::to_vec(&state_json).expect("serialize adaptive state"),
  )
  .expect("write adaptive state");

  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) = spawn_http_sequence_capture_server(vec![500, 200], "ok");
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 2,
    retry_backoff_ms: 80,
    retry_backoff_max_ms: 2_000,
    adaptive_retry: true,
    retry_jitter_ratio: 0.0,
    circuit_breaker_failure_threshold: 2,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_path: Some(state_path.to_string_lossy().to_string()),
    circuit_breaker_scope_key: Some("adaptive-breaker".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let start = Instant::now();
  let result =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect("adaptive retry second attempt should succeed");
  let elapsed = start.elapsed();
  assert_eq!(result.status_code, 200);
  assert!(
    elapsed >= Duration::from_millis(250),
    "adaptive retry backoff too small: {:?}",
    elapsed
  );

  let captures = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured adaptive requests");
  assert_eq!(captures.len(), 2);
  handle.join().expect("adaptive sequence thread");
}

#[test]
fn otlp_push_payload_adaptive_retry_ewma_mode_uses_error_score() {
  let dir = tempfile::tempdir().expect("tempdir");
  let state_path = dir.path().join("otlp-adaptive-ewma-state.json");
  let state_json = serde_json::json!({
    "adaptive-ewma-breaker": {
      "consecutive_failures": 0,
      "open_until_ms": 0,
      "ewma_error_score": 0.75
    }
  });
  std::fs::write(
    &state_path,
    serde_json::to_vec(&state_json).expect("serialize adaptive ewma state"),
  )
  .expect("write adaptive ewma state");

  let payload = "{\"resourceMetrics\":[]}";
  let (endpoint, captured_rx, handle) = spawn_http_sequence_capture_server(vec![500, 200], "ok");
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 2,
    retry_backoff_ms: 80,
    retry_backoff_max_ms: 2_000,
    retry_jitter_ratio: 0.0,
    adaptive_retry: true,
    adaptive_retry_mode: OtlpAdaptiveRetryMode::Ewma,
    adaptive_retry_ewma_alpha: 0.5,
    circuit_breaker_failure_threshold: 2,
    circuit_breaker_open_ms: 2_000,
    circuit_breaker_state_path: Some(state_path.to_string_lossy().to_string()),
    circuit_breaker_scope_key: Some("adaptive-ewma-breaker".to_string()),
    ..OtlpHttpPushOptions::default()
  };

  let start = Instant::now();
  let result =
    push_replication_metrics_otel_json_payload_with_options(payload, &endpoint, &options)
      .expect("adaptive ewma retry second attempt should succeed");
  let elapsed = start.elapsed();
  assert_eq!(result.status_code, 200);
  assert!(
    elapsed >= Duration::from_millis(450),
    "adaptive ewma retry backoff too small: {:?}",
    elapsed
  );

  let captures = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured adaptive ewma requests");
  assert_eq!(captures.len(), 2);
  handle.join().expect("adaptive ewma sequence thread");
}

#[test]
fn otlp_push_grpc_payload_posts_request_and_auth_header() {
  let payload = OtelExportMetricsServiceRequest {
    resource_metrics: Vec::new(),
  }
  .encode_to_vec();
  let (endpoint, captured_rx, shutdown_tx, handle) = spawn_grpc_capture_server(0);
  thread::sleep(Duration::from_millis(50));

  let result =
    push_replication_metrics_otel_grpc_payload(&payload, &endpoint, 2_000, Some("token"))
      .expect("otlp grpc push must succeed");
  assert_eq!(result.status_code, 200);

  let captured = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured grpc request");
  assert_eq!(captured.authorization.as_deref(), Some("Bearer token"));
  assert_eq!(captured.resource_metrics_count, 0);
  assert_eq!(captured.attempt, 1);

  let _ = shutdown_tx.send(());
  handle.join().expect("grpc server thread");
}

#[test]
fn otlp_push_grpc_payload_retries_unavailable_once() {
  let payload = OtelExportMetricsServiceRequest {
    resource_metrics: Vec::new(),
  }
  .encode_to_vec();
  let (endpoint, captured_rx, shutdown_tx, handle) = spawn_grpc_capture_server(1);
  thread::sleep(Duration::from_millis(50));
  let options = OtlpHttpPushOptions {
    timeout_ms: 2_000,
    retry_max_attempts: 2,
    retry_backoff_ms: 1,
    retry_backoff_max_ms: 1,
    ..OtlpHttpPushOptions::default()
  };

  let result =
    push_replication_metrics_otel_grpc_payload_with_options(&payload, &endpoint, &options)
      .expect("second grpc attempt should succeed");
  assert_eq!(result.status_code, 200);

  let captured = captured_rx
    .recv_timeout(Duration::from_secs(2))
    .expect("captured grpc retry request");
  assert_eq!(captured.attempt, 2);

  let _ = shutdown_tx.send(());
  handle.join().expect("grpc server thread");
}

#[test]
fn otlp_push_grpc_payload_rejects_invalid_protobuf() {
  let error = push_replication_metrics_otel_grpc_payload(
    &[0xff, 0x00, 0x12],
    "http://127.0.0.1:4317",
    2_000,
    None,
  )
  .expect_err("invalid protobuf payload must fail");
  assert!(error.to_string().contains("Invalid OTLP protobuf payload"));
}
