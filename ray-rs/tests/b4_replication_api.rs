//! raydb-b4 `replication-core` lane: API additions. Binary snapshot and log
//! transports (X5) and removing a replica's progress through the database
//! (P2). This file does not compile until the APIs exist.

use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use kitedb::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use kitedb::replication::manifest::ManifestStore;
use kitedb::replication::primary::default_replication_sidecar_path;
use kitedb::replication::progress::load_replica_progress;
use kitedb::replication::transport::{LogTransportPage, SnapshotTransport};
use kitedb::replication::types::{CommitToken, ReplicationRole};
use kitedb::util::crc::crc32;

fn primary_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn open_primary(path: &Path) -> SingleFileDB {
  open_single_file(path, primary_options()).expect("open primary")
}

fn commit_node(db: &SingleFileDB, key: &str) -> Option<CommitToken> {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token().expect("commit")
}

fn decode(value: &serde_json::Value) -> Vec<u8> {
  BASE64_STANDARD
    .decode(value.as_str().expect("base64 string"))
    .expect("decode base64")
}

fn manifest_generation(primary_path: &Path) -> u64 {
  ManifestStore::new(default_replication_sidecar_path(primary_path).join("manifest.json"))
    .read()
    .expect("read manifest")
    .generation
}

#[test]
fn b4_x5_binary_snapshot_transport_matches_the_json_export() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("x5-snapshot-primary.kitedb");
  let primary = open_primary(&primary_path);
  for i in 0..3 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let binary: SnapshotTransport = primary
    .primary_export_snapshot_transport(true)
    .expect("binary snapshot");
  let json: serde_json::Value = serde_json::from_str(
    &primary
      .primary_export_snapshot_transport_json(true)
      .expect("json snapshot"),
  )
  .expect("parse json");

  let data = binary.data.as_deref().expect("data included");
  assert_eq!(data, decode(&json["data_base64"]).as_slice());
  assert_eq!(data.len() as u64, binary.byte_length);
  assert_eq!(crc32(data), binary.checksum_crc32);
  assert_eq!(json["format"].as_str(), Some(binary.format));
  assert_eq!(json["byte_length"].as_u64(), Some(binary.byte_length));
  assert_eq!(
    json["checksum_crc32c"].as_str(),
    Some(format!("{:08x}", binary.checksum_crc32).as_str())
  );
  assert_eq!(json["epoch"].as_u64(), Some(binary.epoch));
  assert_eq!(json["head_log_index"].as_u64(), Some(binary.head_log_index));
  assert_eq!(json["retained_floor"].as_u64(), Some(binary.retained_floor));
  assert_eq!(
    json["start_cursor"].as_str(),
    Some(binary.start_cursor.to_string().as_str())
  );
  assert_eq!(binary.generation, manifest_generation(&primary_path));
  assert_eq!(binary.head_log_index, 3);
  assert_eq!(binary.start_cursor.log_index, 3);

  let metadata = primary
    .primary_export_snapshot_transport(false)
    .expect("metadata only");
  assert!(metadata.data.is_none());
  assert_eq!(metadata.byte_length, binary.byte_length);
  assert_eq!(metadata.checksum_crc32, binary.checksum_crc32);

  close_single_file(primary).expect("close primary");
}

#[test]
fn b4_x5_binary_log_transport_matches_the_json_export() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("x5-log-primary.kitedb");
  let primary = open_primary(&primary_path);
  for i in 0..5 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let page: LogTransportPage = primary
    .primary_export_log_transport(None, 3, 1 << 20, true)
    .expect("binary log page");
  let json: serde_json::Value = serde_json::from_str(
    &primary
      .primary_export_log_transport_json(None, 3, 1 << 20, true)
      .expect("json log page"),
  )
  .expect("parse json");

  assert_eq!(page.frames.len(), 3);
  assert!(!page.eof, "two more frames remain");
  assert_eq!(json["eof"].as_bool(), Some(page.eof));
  assert_eq!(json["frame_count"].as_u64(), Some(page.frames.len() as u64));
  assert_eq!(json["total_bytes"].as_u64(), Some(page.total_bytes));
  assert_eq!(json["head_log_index"].as_u64(), Some(page.head_log_index));
  assert_eq!(page.generation, manifest_generation(&primary_path));
  let next_cursor = page.next_cursor.expect("next cursor");
  assert_eq!(
    json["next_cursor"].as_str(),
    Some(next_cursor.to_string().as_str())
  );
  let json_frames = json["frames"].as_array().expect("json frames");
  for (frame, json_frame) in page.frames.iter().zip(json_frames) {
    assert_eq!(json_frame["log_index"].as_u64(), Some(frame.log_index));
    assert_eq!(json_frame["segment_id"].as_u64(), Some(frame.segment_id));
    assert_eq!(
      json_frame["segment_offset"].as_u64(),
      Some(frame.segment_offset)
    );
    assert_eq!(json_frame["bytes"].as_u64(), Some(frame.bytes));
    assert_eq!(
      frame.payload.as_deref(),
      Some(decode(&json_frame["payload_base64"]).as_slice())
    );
  }

  let rest = primary
    .primary_export_log_transport(Some(next_cursor), 64, 1 << 20, false)
    .expect("next page");
  assert!(rest.eof);
  assert_eq!(
    rest
      .frames
      .iter()
      .map(|frame| frame.log_index)
      .collect::<Vec<_>>(),
    vec![4, 5]
  );
  assert!(rest.frames.iter().all(|frame| frame.payload.is_none()));

  close_single_file(primary).expect("close primary");
}

#[test]
fn b4_p2_primary_remove_replica_progress_releases_retention() {
  const DECOMMISSIONED: &str = "decommissioned-replica";

  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("p2-remove-primary.kitedb");
  let sidecar = default_replication_sidecar_path(&primary_path);
  let primary = open_single_file(
    &primary_path,
    primary_options()
      .replication_segment_max_bytes(1)
      .replication_retention_min_entries(2),
  )
  .expect("open primary");

  commit_node(&primary, "n0").expect("token");
  primary
    .primary_report_replica_progress(DECOMMISSIONED, 1, 1)
    .expect("report progress");
  for i in 1..10 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }
  assert_eq!(
    primary
      .primary_run_retention()
      .expect("retention")
      .retained_floor,
    2,
    "setup: the replica at log 1 pins the floor"
  );

  assert!(primary
    .primary_remove_replica_progress(DECOMMISSIONED)
    .expect("remove progress"));
  assert!(
    !primary
      .primary_remove_replica_progress(DECOMMISSIONED)
      .expect("remove again"),
    "a second removal finds nothing"
  );
  assert_eq!(
    primary
      .primary_run_retention()
      .expect("retention after removal")
      .retained_floor,
    8
  );
  assert!(primary
    .primary_replication_status()
    .expect("status")
    .replica_lags
    .iter()
    .all(|lag| lag.replica_id != DECOMMISSIONED));
  assert!(!load_replica_progress(&sidecar)
    .expect("load progress")
    .contains_key(DECOMMISSIONED));
  close_single_file(primary).expect("close primary");

  let plain = open_single_file(
    dir.path().join("p2-remove-plain.kitedb"),
    SingleFileOpenOptions::new(),
  )
  .expect("open plain db");
  assert!(
    plain.primary_remove_replica_progress("any").is_err(),
    "only a primary has replica progress"
  );
  close_single_file(plain).expect("close plain");
}
