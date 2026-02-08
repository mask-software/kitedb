//! Replica-side operations and token wait helpers.

use crate::core::wal::record::{
  parse_add_edge_payload, parse_add_edge_props_payload, parse_add_edges_batch_payload,
  parse_add_edges_props_batch_payload, parse_add_node_label_payload, parse_create_node_payload,
  parse_create_nodes_batch_payload, parse_del_edge_prop_payload, parse_del_node_prop_payload,
  parse_del_node_vector_payload, parse_delete_edge_payload, parse_delete_node_payload,
  parse_remove_node_label_payload, parse_set_edge_prop_payload, parse_set_edge_props_payload,
  parse_set_node_prop_payload, parse_set_node_vector_payload, parse_wal_record, ParsedWalRecord,
};
use crate::error::{KiteError, Result};
use crate::replication::manifest::ManifestStore;
use crate::replication::primary::PrimaryRetentionOutcome;
use crate::replication::replica::ReplicaReplicationStatus;
use crate::replication::transport::decode_commit_frame_payload;
use crate::replication::types::{CommitToken, ReplicationCursor, ReplicationRole};
use crate::types::WalRecordType;
use crate::util::crc::crc32c;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde_json::json;
use std::collections::HashSet;
use std::str::FromStr;
use std::time::{Duration, Instant};

use super::{close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions};

const REPLICATION_MANIFEST_FILE: &str = "manifest.json";
const REPLICATION_FRAME_MAGIC: u32 = 0x474F_4C52;
const REPLICATION_FRAME_HEADER_BYTES: usize = 32;

impl SingleFileDB {
  /// Promote this primary instance to the next replication epoch.
  pub fn primary_promote_to_next_epoch(&self) -> Result<u64> {
    self
      .primary_replication
      .as_ref()
      .ok_or_else(|| {
        KiteError::InvalidReplication("database is not opened in primary role".to_string())
      })?
      .promote_to_next_epoch()
  }

  /// Report a replica's applied cursor to drive retention decisions.
  pub fn primary_report_replica_progress(
    &self,
    replica_id: &str,
    epoch: u64,
    applied_log_index: u64,
  ) -> Result<()> {
    self
      .primary_replication
      .as_ref()
      .ok_or_else(|| {
        KiteError::InvalidReplication("database is not opened in primary role".to_string())
      })?
      .report_replica_progress(replica_id, epoch, applied_log_index)
  }

  /// Run retention pruning on primary replication segments.
  pub fn primary_run_retention(&self) -> Result<PrimaryRetentionOutcome> {
    self
      .primary_replication
      .as_ref()
      .ok_or_else(|| {
        KiteError::InvalidReplication("database is not opened in primary role".to_string())
      })?
      .run_retention()
  }

  /// Replica status surface.
  pub fn replica_replication_status(&self) -> Option<ReplicaReplicationStatus> {
    self
      .replica_replication
      .as_ref()
      .map(|replication| replication.status())
  }

  /// Bootstrap replica state from source primary snapshot.
  pub fn replica_bootstrap_from_snapshot(&self) -> Result<()> {
    let runtime = self.replica_replication.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in replica role".to_string())
    })?;

    let source_db_path = runtime.source_db_path().ok_or_else(|| {
      KiteError::InvalidReplication("replica source db path is not configured".to_string())
    })?;

    let source = open_single_file(
      &source_db_path,
      SingleFileOpenOptions::new()
        .read_only(true)
        .create_if_missing(false)
        .replication_role(ReplicationRole::Disabled),
    )?;

    sync_graph_state(self, &source)?;

    let (epoch, head) = runtime.source_head_position()?;
    runtime.mark_applied(epoch, head)?;
    runtime.clear_error()?;

    close_single_file(source)?;
    Ok(())
  }

  /// Force snapshot reseed for replicas that lost log continuity.
  pub fn replica_reseed_from_snapshot(&self) -> Result<()> {
    self.replica_bootstrap_from_snapshot()
  }

  /// Pull and apply the next batch of replication frames.
  pub fn replica_catch_up_once(&self, max_frames: usize) -> Result<usize> {
    self.replica_catch_up_internal(max_frames, false)
  }

  /// Test helper: request a batch including last-applied frame to verify idempotency.
  pub fn replica_catch_up_once_replaying_last_for_testing(
    &self,
    max_frames: usize,
  ) -> Result<usize> {
    self.replica_catch_up_internal(max_frames, true)
  }

  /// Wait until this DB has applied at least the given token.
  pub fn wait_for_token(&self, token: CommitToken, timeout_ms: u64) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    loop {
      if self.has_token(token) {
        return Ok(true);
      }

      if Instant::now() >= deadline {
        return Ok(false);
      }

      std::thread::sleep(Duration::from_millis(10));
    }
  }

  fn has_token(&self, token: CommitToken) -> bool {
    if let Some(status) = self.primary_replication_status() {
      if let Some(last_token) = status.last_token {
        return last_token >= token;
      }
    }

    if let Some(status) = self.replica_replication_status() {
      let replica_token = CommitToken::new(status.applied_epoch, status.applied_log_index);
      return replica_token >= token;
    }

    false
  }

  fn replica_catch_up_internal(&self, max_frames: usize, replay_last: bool) -> Result<usize> {
    let runtime = self.replica_replication.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in replica role".to_string())
    })?;

    let frames = match runtime.frames_after(max_frames.max(1), replay_last) {
      Ok(frames) => frames,
      Err(err) => {
        if !runtime.status().needs_reseed {
          let _ = runtime.mark_error(err.to_string(), false);
        }
        return Err(err);
      }
    };
    if frames.is_empty() {
      return Ok(0);
    }

    let mut applied = 0usize;
    for frame in frames {
      let (applied_epoch, applied_log_index) = runtime.applied_position();
      let already_applied = applied_epoch > frame.epoch
        || (applied_epoch == frame.epoch && applied_log_index >= frame.log_index);
      if already_applied {
        continue;
      }

      if let Err(err) = apply_replication_frame(self, &frame.payload) {
        let _ = runtime.mark_error(
          format!(
            "replica apply failed at {}:{}: {err}",
            frame.epoch, frame.log_index
          ),
          false,
        );
        return Err(err);
      }

      if let Err(err) = runtime.mark_applied(frame.epoch, frame.log_index) {
        let _ = runtime.mark_error(
          format!(
            "replica cursor persist failed at {}:{}: {err}",
            frame.epoch, frame.log_index
          ),
          false,
        );
        return Err(err);
      }
      applied = applied.saturating_add(1);
    }

    runtime.clear_error()?;
    Ok(applied)
  }

  /// Export latest primary snapshot metadata and optional bytes as transport JSON.
  pub fn primary_export_snapshot_transport_json(&self, include_data: bool) -> Result<String> {
    let status = self.primary_replication_status().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    let snapshot_bytes = std::fs::read(&self.path)?;
    let checksum_crc32c = format!("{:08x}", crc32c(&snapshot_bytes));
    let generated_at_ms = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    let payload = json!({
      "format": "single-file-db-copy",
      "db_path": self.path.to_string_lossy().to_string(),
      "byte_length": snapshot_bytes.len(),
      "checksum_crc32c": checksum_crc32c,
      "generated_at_ms": generated_at_ms,
      "epoch": status.epoch,
      "head_log_index": status.head_log_index,
      "retained_floor": status.retained_floor,
      "start_cursor": ReplicationCursor::new(status.epoch, 0, 0, status.retained_floor).to_string(),
      "data_base64": if include_data {
        Some(BASE64_STANDARD.encode(&snapshot_bytes))
      } else {
        None
      },
    });

    serde_json::to_string(&payload).map_err(|error| {
      KiteError::Serialization(format!("encode replication snapshot export: {error}"))
    })
  }

  /// Export primary replication log frames with cursor paging as transport JSON.
  pub fn primary_export_log_transport_json(
    &self,
    cursor: Option<&str>,
    max_frames: usize,
    max_bytes: usize,
    include_payload: bool,
  ) -> Result<String> {
    if max_frames == 0 {
      return Err(KiteError::InvalidQuery("max_frames must be > 0".into()));
    }
    if max_bytes == 0 {
      return Err(KiteError::InvalidQuery("max_bytes must be > 0".into()));
    }

    let status = self.primary_replication_status().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    let sidecar_path = status.sidecar_path;
    let manifest = ManifestStore::new(sidecar_path.join(REPLICATION_MANIFEST_FILE)).read()?;
    let parsed_cursor = match cursor {
      Some(raw) if !raw.trim().is_empty() => Some(
        ReplicationCursor::from_str(raw)
          .map_err(|error| KiteError::InvalidReplication(format!("invalid cursor: {error}")))?,
      ),
      _ => None,
    };

    let mut segments = manifest.segments.clone();
    segments.sort_by_key(|segment| segment.id);

    let mut frames = Vec::new();
    let mut total_bytes = 0usize;
    let mut next_cursor: Option<String> = None;
    let mut limited = false;

    'outer: for segment in segments {
      let segment_path = sidecar_path.join(format_segment_file_name(segment.id));
      if !segment_path.exists() {
        continue;
      }
      let bytes = std::fs::read(&segment_path)?;
      let mut offset = 0usize;

      while offset + REPLICATION_FRAME_HEADER_BYTES <= bytes.len() {
        let magic = le_u32(&bytes[offset..offset + 4])?;
        if magic != REPLICATION_FRAME_MAGIC {
          break;
        }

        let epoch = le_u64(&bytes[offset + 8..offset + 16])?;
        let log_index = le_u64(&bytes[offset + 16..offset + 24])?;
        let payload_len = le_u32(&bytes[offset + 24..offset + 28])? as usize;
        let payload_start = offset + REPLICATION_FRAME_HEADER_BYTES;
        let payload_end = payload_start.checked_add(payload_len).ok_or_else(|| {
          KiteError::InvalidReplication("replication frame payload overflow".to_string())
        })?;
        if payload_end > bytes.len() {
          return Err(KiteError::InvalidReplication(format!(
            "replication frame truncated in segment {} at byte {}",
            segment.id, offset
          )));
        }

        let frame_bytes = payload_end - offset;
        let frame_offset = offset as u64;
        if frame_after_cursor(parsed_cursor, epoch, segment.id, frame_offset, log_index) {
          if (total_bytes + frame_bytes > max_bytes && !frames.is_empty())
            || frames.len() >= max_frames
          {
            limited = true;
            break 'outer;
          }

          next_cursor = Some(
            ReplicationCursor::new(epoch, segment.id, payload_end as u64, log_index).to_string(),
          );
          let payload_base64 = if include_payload {
            Some(BASE64_STANDARD.encode(&bytes[payload_start..payload_end]))
          } else {
            None
          };

          frames.push(json!({
            "epoch": epoch,
            "log_index": log_index,
            "segment_id": segment.id,
            "segment_offset": frame_offset,
            "bytes": frame_bytes,
            "payload_base64": payload_base64,
          }));
          total_bytes += frame_bytes;
        }

        offset = payload_end;
      }
    }

    let payload = json!({
      "epoch": manifest.epoch,
      "head_log_index": manifest.head_log_index,
      "retained_floor": manifest.retained_floor,
      "cursor": parsed_cursor.map(|value| value.to_string()),
      "next_cursor": next_cursor,
      "eof": !limited,
      "frame_count": frames.len(),
      "total_bytes": total_bytes,
      "frames": frames,
    });

    serde_json::to_string(&payload)
      .map_err(|error| KiteError::Serialization(format!("encode replication log export: {error}")))
  }
}

fn frame_after_cursor(
  cursor: Option<ReplicationCursor>,
  epoch: u64,
  segment_id: u64,
  segment_offset: u64,
  log_index: u64,
) -> bool {
  match cursor {
    None => true,
    Some(cursor) => {
      (epoch, log_index, segment_id, segment_offset)
        > (
          cursor.epoch,
          cursor.log_index,
          cursor.segment_id,
          cursor.segment_offset,
        )
    }
  }
}

fn le_u32(bytes: &[u8]) -> Result<u32> {
  let value: [u8; 4] = bytes
    .try_into()
    .map_err(|_| KiteError::InvalidReplication("invalid frame u32 field".to_string()))?;
  Ok(u32::from_le_bytes(value))
}

fn le_u64(bytes: &[u8]) -> Result<u64> {
  let value: [u8; 8] = bytes
    .try_into()
    .map_err(|_| KiteError::InvalidReplication("invalid frame u64 field".to_string()))?;
  Ok(u64::from_le_bytes(value))
}

fn format_segment_file_name(id: u64) -> String {
  format!("segment-{id:020}.rlog")
}

fn sync_graph_state(replica: &SingleFileDB, source: &SingleFileDB) -> Result<()> {
  let tx_guard = replica.begin_guard(false)?;

  let source_nodes = source.list_nodes();
  let source_node_set: HashSet<_> = source_nodes.iter().copied().collect();

  for &node_id in &source_nodes {
    let source_key = source.node_key(node_id);
    if replica.node_exists(node_id) {
      if replica.node_key(node_id) != source_key {
        let _ = replica.delete_node(node_id)?;
        replica.create_node_with_id(node_id, source_key.as_deref())?;
      }
    } else {
      replica.create_node_with_id(node_id, source_key.as_deref())?;
    }
  }

  for node_id in replica.list_nodes() {
    if !source_node_set.contains(&node_id) {
      let _ = replica.delete_node(node_id)?;
    }
  }

  for &node_id in &source_nodes {
    let source_props = source.node_props(node_id).unwrap_or_default();
    let replica_props = replica.node_props(node_id).unwrap_or_default();
    for (&key_id, value) in &source_props {
      if replica_props.get(&key_id) != Some(value) {
        replica.set_node_prop(node_id, key_id, value.clone())?;
      }
    }
    for &key_id in replica_props.keys() {
      if !source_props.contains_key(&key_id) {
        replica.delete_node_prop(node_id, key_id)?;
      }
    }

    let source_labels: HashSet<_> = source.node_labels(node_id).into_iter().collect();
    let replica_labels: HashSet<_> = replica.node_labels(node_id).into_iter().collect();
    for &label_id in &source_labels {
      if !replica_labels.contains(&label_id) {
        replica.add_node_label(node_id, label_id)?;
      }
    }
    for &label_id in &replica_labels {
      if !source_labels.contains(&label_id) {
        replica.remove_node_label(node_id, label_id)?;
      }
    }
  }

  let mut vector_prop_keys: HashSet<_> = source.vector_stores.read().keys().copied().collect();
  vector_prop_keys.extend(replica.vector_stores.read().keys().copied());
  for &node_id in &source_nodes {
    for &prop_key_id in &vector_prop_keys {
      let source_vector = source.node_vector(node_id, prop_key_id);
      let replica_vector = replica.node_vector(node_id, prop_key_id);
      match (source_vector, replica_vector) {
        (Some(source_value), Some(replica_value)) => {
          if source_value.as_ref() != replica_value.as_ref() {
            replica.set_node_vector(node_id, prop_key_id, source_value.as_ref())?;
          }
        }
        (Some(source_value), None) => {
          replica.set_node_vector(node_id, prop_key_id, source_value.as_ref())?;
        }
        (None, Some(_)) => {
          replica.delete_node_vector(node_id, prop_key_id)?;
        }
        (None, None) => {}
      }
    }
  }

  let source_edges = source.list_edges(None);
  let source_edge_set: HashSet<_> = source_edges
    .iter()
    .map(|edge| (edge.src, edge.etype, edge.dst))
    .collect();

  for edge in &source_edges {
    if !replica.edge_exists(edge.src, edge.etype, edge.dst) {
      replica.add_edge(edge.src, edge.etype, edge.dst)?;
    }
  }

  for edge in replica.list_edges(None) {
    if !source_edge_set.contains(&(edge.src, edge.etype, edge.dst)) {
      replica.delete_edge(edge.src, edge.etype, edge.dst)?;
    }
  }

  for edge in source_edges {
    let source_props = source
      .edge_props(edge.src, edge.etype, edge.dst)
      .unwrap_or_default();
    let replica_props = replica
      .edge_props(edge.src, edge.etype, edge.dst)
      .unwrap_or_default();

    for (&key_id, value) in &source_props {
      if replica_props.get(&key_id) != Some(value) {
        replica.set_edge_prop(edge.src, edge.etype, edge.dst, key_id, value.clone())?;
      }
    }
    for &key_id in replica_props.keys() {
      if !source_props.contains_key(&key_id) {
        replica.delete_edge_prop(edge.src, edge.etype, edge.dst, key_id)?;
      }
    }
  }

  tx_guard.commit()
}

fn apply_replication_frame(db: &SingleFileDB, payload: &[u8]) -> Result<()> {
  let decoded = decode_commit_frame_payload(payload)?;
  let records = parse_wal_records(&decoded.wal_bytes)?;

  if records.is_empty() {
    return Ok(());
  }

  let tx_guard = db.begin_guard(false)?;
  for record in &records {
    apply_wal_record_idempotent(db, record)?;
  }

  tx_guard.commit()
}

fn parse_wal_records(wal_bytes: &[u8]) -> Result<Vec<ParsedWalRecord>> {
  let mut offset = 0usize;
  let mut records = Vec::new();

  while offset < wal_bytes.len() {
    let record = parse_wal_record(wal_bytes, offset).ok_or_else(|| {
      KiteError::InvalidReplication(format!(
        "invalid WAL payload in replication frame at offset {offset}"
      ))
    })?;

    if record.record_end <= offset {
      return Err(KiteError::InvalidReplication(
        "non-progressing WAL record parse in replication payload".to_string(),
      ));
    }

    offset = record.record_end;
    records.push(record);
  }

  Ok(records)
}

fn apply_wal_record_idempotent(db: &SingleFileDB, record: &ParsedWalRecord) -> Result<()> {
  match record.record_type {
    WalRecordType::Begin | WalRecordType::Commit | WalRecordType::Rollback => Ok(()),
    WalRecordType::CreateNode => {
      let data = parse_create_node_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid CreateNode replication payload".to_string())
      })?;

      if db.node_exists(data.node_id) {
        if db.node_key(data.node_id) == data.key {
          return Ok(());
        }
        return Err(KiteError::InvalidReplication(format!(
          "create-node replay key mismatch for node {}",
          data.node_id
        )));
      }

      db.create_node_with_id(data.node_id, data.key.as_deref())?;
      Ok(())
    }
    WalRecordType::CreateNodesBatch => {
      let entries = parse_create_nodes_batch_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid CreateNodesBatch replication payload".to_string())
      })?;

      for entry in entries {
        if db.node_exists(entry.node_id) {
          if db.node_key(entry.node_id) != entry.key {
            return Err(KiteError::InvalidReplication(format!(
              "create-nodes-batch replay key mismatch for node {}",
              entry.node_id
            )));
          }
          continue;
        }

        db.create_node_with_id(entry.node_id, entry.key.as_deref())?;
      }

      Ok(())
    }
    WalRecordType::DeleteNode => {
      let data = parse_delete_node_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DeleteNode replication payload".to_string())
      })?;
      if db.node_exists(data.node_id) {
        let _ = db.delete_node(data.node_id)?;
      }
      Ok(())
    }
    WalRecordType::AddEdge => {
      let data = parse_add_edge_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdge replication payload".to_string())
      })?;
      if !db.edge_exists(data.src, data.etype, data.dst) {
        db.add_edge(data.src, data.etype, data.dst)?;
      }
      Ok(())
    }
    WalRecordType::DeleteEdge => {
      let data = parse_delete_edge_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DeleteEdge replication payload".to_string())
      })?;
      if db.edge_exists(data.src, data.etype, data.dst) {
        db.delete_edge(data.src, data.etype, data.dst)?;
      }
      Ok(())
    }
    WalRecordType::AddEdgesBatch => {
      let batch = parse_add_edges_batch_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgesBatch replication payload".to_string())
      })?;

      for edge in batch {
        if !db.edge_exists(edge.src, edge.etype, edge.dst) {
          db.add_edge(edge.src, edge.etype, edge.dst)?;
        }
      }
      Ok(())
    }
    WalRecordType::AddEdgeProps => {
      let data = parse_add_edge_props_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgeProps replication payload".to_string())
      })?;

      if !db.edge_exists(data.src, data.etype, data.dst) {
        db.add_edge(data.src, data.etype, data.dst)?;
      }

      for (key_id, value) in data.props {
        if db.edge_prop(data.src, data.etype, data.dst, key_id) != Some(value.clone()) {
          db.set_edge_prop(data.src, data.etype, data.dst, key_id, value)?;
        }
      }
      Ok(())
    }
    WalRecordType::AddEdgesPropsBatch => {
      let batch = parse_add_edges_props_batch_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgesPropsBatch replication payload".to_string())
      })?;

      for entry in batch {
        if !db.edge_exists(entry.src, entry.etype, entry.dst) {
          db.add_edge(entry.src, entry.etype, entry.dst)?;
        }

        for (key_id, value) in entry.props {
          if db.edge_prop(entry.src, entry.etype, entry.dst, key_id) != Some(value.clone()) {
            db.set_edge_prop(entry.src, entry.etype, entry.dst, key_id, value)?;
          }
        }
      }

      Ok(())
    }
    WalRecordType::SetNodeProp => {
      let data = parse_set_node_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetNodeProp replication payload".to_string())
      })?;

      if db.node_prop(data.node_id, data.key_id) != Some(data.value.clone()) {
        db.set_node_prop(data.node_id, data.key_id, data.value)?;
      }

      Ok(())
    }
    WalRecordType::DelNodeProp => {
      let data = parse_del_node_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelNodeProp replication payload".to_string())
      })?;

      if db.node_prop(data.node_id, data.key_id).is_some() {
        db.delete_node_prop(data.node_id, data.key_id)?;
      }
      Ok(())
    }
    WalRecordType::SetEdgeProp => {
      let data = parse_set_edge_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetEdgeProp replication payload".to_string())
      })?;

      if db.edge_prop(data.src, data.etype, data.dst, data.key_id) != Some(data.value.clone()) {
        db.set_edge_prop(data.src, data.etype, data.dst, data.key_id, data.value)?;
      }
      Ok(())
    }
    WalRecordType::SetEdgeProps => {
      let data = parse_set_edge_props_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetEdgeProps replication payload".to_string())
      })?;

      for (key_id, value) in data.props {
        if db.edge_prop(data.src, data.etype, data.dst, key_id) != Some(value.clone()) {
          db.set_edge_prop(data.src, data.etype, data.dst, key_id, value)?;
        }
      }
      Ok(())
    }
    WalRecordType::DelEdgeProp => {
      let data = parse_del_edge_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelEdgeProp replication payload".to_string())
      })?;

      if db
        .edge_prop(data.src, data.etype, data.dst, data.key_id)
        .is_some()
      {
        db.delete_edge_prop(data.src, data.etype, data.dst, data.key_id)?;
      }
      Ok(())
    }
    WalRecordType::AddNodeLabel => {
      let data = parse_add_node_label_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddNodeLabel replication payload".to_string())
      })?;

      if !db.node_has_label(data.node_id, data.label_id) {
        db.add_node_label(data.node_id, data.label_id)?;
      }
      Ok(())
    }
    WalRecordType::RemoveNodeLabel => {
      let data = parse_remove_node_label_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid RemoveNodeLabel replication payload".to_string())
      })?;

      if db.node_has_label(data.node_id, data.label_id) {
        db.remove_node_label(data.node_id, data.label_id)?;
      }
      Ok(())
    }
    WalRecordType::SetNodeVector => {
      let data = parse_set_node_vector_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetNodeVector replication payload".to_string())
      })?;

      let current = db.node_vector(data.node_id, data.prop_key_id);
      if current.as_deref().map(|v| v.as_ref()) != Some(data.vector.as_slice()) {
        db.set_node_vector(data.node_id, data.prop_key_id, &data.vector)?;
      }
      Ok(())
    }
    WalRecordType::DelNodeVector => {
      let data = parse_del_node_vector_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelNodeVector replication payload".to_string())
      })?;

      if db.has_node_vector(data.node_id, data.prop_key_id) {
        db.delete_node_vector(data.node_id, data.prop_key_id)?;
      }
      Ok(())
    }
    WalRecordType::DefineLabel | WalRecordType::DefineEtype | WalRecordType::DefinePropkey => {
      // IDs are embedded in mutation records; numeric IDs are sufficient for correctness
      // during V1 replication apply.
      Ok(())
    }
    WalRecordType::BatchVectors | WalRecordType::SealFragment | WalRecordType::CompactFragments => {
      // Vector batch and maintenance records are derived/index-management artifacts.
      // Replica correctness is defined by logical graph + property mutations, including
      // SetNodeVector/DelNodeVector records, so these can be skipped safely.
      Ok(())
    }
  }
}

#[cfg(test)]
mod tests {
  use super::apply_wal_record_idempotent;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::core::wal::record::ParsedWalRecord;
  use crate::types::WalRecordType;

  #[test]
  fn replica_apply_ignores_vector_maintenance_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("replica-apply-vector-maintenance.kitedb");
    let db = open_single_file(&db_path, SingleFileOpenOptions::new()).expect("open db");

    for record_type in [
      WalRecordType::BatchVectors,
      WalRecordType::SealFragment,
      WalRecordType::CompactFragments,
    ] {
      let record = ParsedWalRecord {
        record_type,
        flags: 0,
        txid: 1,
        payload: Vec::new(),
        record_end: 0,
      };
      apply_wal_record_idempotent(&db, &record)
        .expect("derived vector maintenance should be ignored");
    }

    assert_eq!(db.count_nodes(), 0);
    assert_eq!(db.count_edges(), 0);
    close_single_file(db).expect("close db");
  }
}
