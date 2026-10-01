//! Replica-side operations and token wait helpers.

use crate::core::wal::record::{
  parse_add_edge_payload, parse_add_edge_props_payload, parse_add_edges_batch_payload,
  parse_add_edges_props_batch_payload, parse_add_node_label_payload, parse_create_node_payload,
  parse_create_nodes_batch_payload, parse_define_etype_payload, parse_define_label_payload,
  parse_define_propkey_payload, parse_del_edge_prop_payload, parse_del_node_prop_payload,
  parse_del_node_vector_payload, parse_delete_edge_payload, parse_delete_node_payload,
  parse_remove_node_label_payload, parse_set_edge_prop_payload, parse_set_edge_props_payload,
  parse_set_node_prop_payload, parse_set_node_vector_payload, parse_wal_record, ParsedWalRecord,
};
use crate::error::{KiteError, Result};
use crate::replication::manifest::ManifestStore;
use crate::replication::primary::{primary_sidecar_needs_repair, PrimaryRetentionOutcome};
use crate::replication::replica::{
  ReplicaReplicationStatus, ReplicaSchemaMap,
  SchemaIdKind::{self, EdgeType, Label, PropertyKey},
};
use crate::replication::transport::decode_commit_frame_payload;
use crate::replication::types::{CommitToken, ReplicationCursor};
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue, TxId, WalRecordType};
use crate::util::crc::{crc32c, Crc32cHasher};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use super::open::open_replication_source;
use super::recovery::{committed_transactions, scan_wal_records};
use super::{close_single_file, SingleFileDB};

const REPLICATION_MANIFEST_FILE: &str = "manifest.json";
const REPLICATION_FRAME_MAGIC: u32 = 0x474F_4C52;
const REPLICATION_FRAME_VERSION: u16 = 1;
const REPLICATION_FRAME_FLAG_CRC32_DISABLED: u16 = 0x0001;
const REPLICATION_FRAME_HEADER_BYTES: usize = 32;
const REPLICATION_MAX_FRAME_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const REPLICATION_IO_CHUNK_BYTES: usize = 64 * 1024;
const REPLICATION_SNAPSHOT_INLINE_MAX_BYTES: u64 = 32 * 1024 * 1024;
const REPLICA_CATCH_UP_MAX_ATTEMPTS: usize = 5;
const REPLICA_CATCH_UP_INITIAL_BACKOFF_MS: u64 = 10;
const REPLICA_CATCH_UP_MAX_BACKOFF_MS: u64 = 160;
const REPLICA_BOOTSTRAP_MAX_ATTEMPTS: usize = 20;
const REPLICA_BOOTSTRAP_INITIAL_BACKOFF_MS: u64 = 10;
const REPLICA_BOOTSTRAP_MAX_BACKOFF_MS: u64 = 320;
const SOURCE_FRAME_UNPUBLISHED_ERROR: &str =
  "source primary has not published the replication frame for its last commit";

impl SingleFileDB {
  /// Promote this primary instance to the next replication epoch.
  ///
  /// The new epoch opens with a frame that re-announces every schema name
  /// and id. Replicas translate the primary's schema ids by name and trust a
  /// translation only within the epoch that announced it.
  pub fn primary_promote_to_next_epoch(&self) -> Result<u64> {
    let replication = self.primary_replication.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    // The announcement commits its own transaction.
    if self.has_transaction() {
      return Err(KiteError::TransactionInProgress);
    }

    let promotion = replication.promote()?;
    if promotion.promoted {
      self.announce_schema()?;
    }
    Ok(promotion.epoch)
  }

  fn announce_schema(&self) -> Result<()> {
    let has_schema = !self.label_ids.read().is_empty()
      || !self.etype_ids.read().is_empty()
      || !self.propkey_ids.read().is_empty();
    if !has_schema {
      return Ok(());
    }

    let tx_guard = self.begin_guard(false)?;
    self.log_committed_schema()?;
    match tx_guard.commit() {
      // A racing instance promoted past this one; the winner announces.
      Err(error) if is_stale_primary_error(&error) => Ok(()),
      result => result,
    }
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

    let mut attempts = 0usize;
    let mut backoff_ms = REPLICA_BOOTSTRAP_INITIAL_BACKOFF_MS;
    loop {
      attempts = attempts.saturating_add(1);
      let source = match open_replication_source(&source_db_path) {
        Ok(source) => source,
        Err(error)
          if is_bootstrap_retryable_error(&error) && attempts < REPLICA_BOOTSTRAP_MAX_ATTEMPTS =>
        {
          std::thread::sleep(Duration::from_millis(backoff_ms));
          backoff_ms = backoff_ms
            .saturating_mul(2)
            .min(REPLICA_BOOTSTRAP_MAX_BACKOFF_MS);
          continue;
        }
        Err(error) => return Err(error),
      };

      let bootstrap_start = runtime.source_head_position()?;
      let bootstrap_source_fingerprint = source_db_fingerprint(&source_db_path)?;
      let sync_result = (|| {
        let bootstrap_position = bootstrap_log_position(runtime, &source)?;
        std::thread::sleep(Duration::from_millis(10));
        let quiesce_head = runtime.source_head_position()?;
        let quiesce_fingerprint = source_db_fingerprint(&source_db_path)?;
        if quiesce_head != bootstrap_start || quiesce_fingerprint != bootstrap_source_fingerprint {
          return Err(KiteError::InvalidReplication(format!(
            "source primary did not quiesce for snapshot bootstrap; start={}:{}, observed={}:{}, start_crc={:08x}, observed_crc={:08x}; quiesce writes and retry",
            bootstrap_start.0,
            bootstrap_start.1,
            quiesce_head.0,
            quiesce_head.1,
            bootstrap_source_fingerprint.1,
            quiesce_fingerprint.1
          )));
        }
        remove_stale_nodes(self, &source, runtime)?;
        let schema_map = sync_graph_state(self, &source, bootstrap_position.0, || {
          let bootstrap_end = runtime.source_head_position()?;
          let bootstrap_end_fingerprint = source_db_fingerprint(&source_db_path)?;
          if bootstrap_end != bootstrap_start
            || bootstrap_end_fingerprint != bootstrap_source_fingerprint
          {
            return Err(KiteError::InvalidReplication(format!(
              "source primary advanced during snapshot bootstrap; start={}:{}, end={}:{}, start_crc={:08x}, end_crc={:08x}; quiesce writes and retry",
              bootstrap_start.0,
              bootstrap_start.1,
              bootstrap_end.0,
              bootstrap_end.1,
              bootstrap_source_fingerprint.1,
              bootstrap_end_fingerprint.1
            )));
          }
          std::thread::sleep(Duration::from_millis(10));
          let quiesce_head = runtime.source_head_position()?;
          let quiesce_fingerprint = source_db_fingerprint(&source_db_path)?;
          if quiesce_head != bootstrap_start || quiesce_fingerprint != bootstrap_source_fingerprint {
            return Err(KiteError::InvalidReplication(format!(
              "source primary did not quiesce for snapshot bootstrap; start={}:{}, observed={}:{}, start_crc={:08x}, observed_crc={:08x}; quiesce writes and retry",
              bootstrap_start.0,
              bootstrap_start.1,
              quiesce_head.0,
              quiesce_head.1,
              bootstrap_source_fingerprint.1,
              quiesce_fingerprint.1
            )));
          }
          Ok(())
        })?;
        Ok((bootstrap_position, schema_map))
      })()
      .and_then(|((epoch, log_index), schema_map)| {
        runtime.store_schema_map(schema_map)?;
        runtime.mark_applied(epoch, log_index)?;
        runtime.clear_error()
      });

      let close_result = close_single_file(source);
      if let Err(error) = sync_result {
        if is_bootstrap_retryable_error(&error) && attempts < REPLICA_BOOTSTRAP_MAX_ATTEMPTS {
          std::thread::sleep(Duration::from_millis(backoff_ms));
          backoff_ms = backoff_ms
            .saturating_mul(2)
            .min(REPLICA_BOOTSTRAP_MAX_BACKOFF_MS);
          continue;
        }
        let _ = runtime.mark_error(error.to_string(), false);
        return Err(error);
      }
      close_result?;
      return Ok(());
    }
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

    let mut attempts = 0usize;
    let mut backoff_ms = REPLICA_CATCH_UP_INITIAL_BACKOFF_MS;
    loop {
      attempts = attempts.saturating_add(1);
      match self.replica_catch_up_attempt(runtime, max_frames.max(1), replay_last) {
        Ok(applied) => return Ok(applied),
        Err(error) => {
          if runtime.status().needs_reseed {
            return Err(error);
          }
          if is_reseed_error(&error) {
            let _ = runtime.mark_error(error.to_string(), true);
            return Err(error);
          }

          if attempts >= REPLICA_CATCH_UP_MAX_ATTEMPTS {
            let _ = runtime.mark_error(error.to_string(), false);
            return Err(error);
          }

          std::thread::sleep(Duration::from_millis(backoff_ms));
          backoff_ms = backoff_ms
            .saturating_mul(2)
            .min(REPLICA_CATCH_UP_MAX_BACKOFF_MS);
        }
      }
    }
  }

  fn replica_catch_up_attempt(
    &self,
    runtime: &crate::replication::replica::ReplicaReplication,
    max_frames: usize,
    replay_last: bool,
  ) -> Result<usize> {
    let frames = runtime.frames_after(max_frames, replay_last)?;
    if frames.is_empty() {
      runtime.clear_error()?;
      return Ok(0);
    }

    let (mut applied_epoch, mut applied_log_index) = runtime.applied_position();
    let mut schema_map = runtime.schema_map();
    let mut applied = 0usize;
    for frame in frames {
      let already_applied = applied_epoch > frame.epoch
        || (applied_epoch == frame.epoch && applied_log_index >= frame.log_index);
      if already_applied {
        continue;
      }

      // The frame's defines join the translation only if the frame commits.
      let mut frame_schema_map = schema_map.clone();
      frame_schema_map.enter_epoch(frame.epoch);
      if let Err(error) = apply_replication_frame(self, &frame.payload, &mut frame_schema_map) {
        if applied > 0 {
          let _ = runtime
            .store_schema_map(schema_map)
            .and_then(|_| runtime.mark_applied(applied_epoch, applied_log_index));
        }
        return Err(KiteError::InvalidReplication(format!(
          "replica apply failed at {}:{}: {error}",
          frame.epoch, frame.log_index
        )));
      }

      schema_map = frame_schema_map;
      applied_epoch = frame.epoch;
      applied_log_index = frame.log_index;
      applied = applied.saturating_add(1);
    }

    if applied > 0 {
      runtime
        .store_schema_map(schema_map)
        .and_then(|_| runtime.mark_applied(applied_epoch, applied_log_index))
        .map_err(|error| {
          KiteError::InvalidReplication(format!(
            "replica cursor persist failed at {applied_epoch}:{applied_log_index}: {error}"
          ))
        })?;
    }

    runtime.clear_error()?;
    Ok(applied)
  }

  /// Export latest primary snapshot metadata and optional bytes as transport JSON.
  pub fn primary_export_snapshot_transport_json(&self, include_data: bool) -> Result<String> {
    let status = self.primary_replication_status().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    let (byte_length, checksum_crc32c, data_base64) =
      read_snapshot_transport_payload(&self.path, include_data)?;
    let generated_at_ms = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis() as u64;

    let payload = json!({
      "format": "single-file-db-copy",
      "db_path": self.path.to_string_lossy().to_string(),
      "byte_length": byte_length,
      "checksum_crc32c": checksum_crc32c,
      "generated_at_ms": generated_at_ms,
      "epoch": status.epoch,
      "head_log_index": status.head_log_index,
      "retained_floor": status.retained_floor,
      "start_cursor": ReplicationCursor::new(status.epoch, 0, 0, status.retained_floor).to_string(),
      "data_base64": data_base64,
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

    let primary_replication = self.primary_replication.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    primary_replication.flush_for_transport_export()?;
    let status = primary_replication.status();
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

      let mut reader = BufReader::new(File::open(&segment_path)?);
      let mut offset = 0u64;
      loop {
        let Some(header) = read_frame_header(&mut reader, segment.id, offset)? else {
          break;
        };

        let frame_offset = offset;
        let frame_bytes = REPLICATION_FRAME_HEADER_BYTES
          .checked_add(header.payload_len)
          .ok_or_else(|| {
            KiteError::InvalidReplication("replication frame payload overflow".to_string())
          })?;
        let payload_end = frame_offset
          .checked_add(frame_bytes as u64)
          .ok_or_else(|| {
            KiteError::InvalidReplication("replication frame payload overflow".to_string())
          })?;

        let include_frame = frame_after_cursor(
          parsed_cursor,
          header.epoch,
          segment.id,
          frame_offset,
          header.log_index,
        );
        if include_frame {
          if frame_bytes > max_bytes {
            return Err(KiteError::InvalidQuery(
              format!("max_bytes budget {max_bytes} is smaller than frame size {frame_bytes}")
                .into(),
            ));
          }
          if frames.len() >= max_frames || total_bytes.saturating_add(frame_bytes) > max_bytes {
            limited = true;
            break 'outer;
          }
        }

        let payload_base64 = read_frame_payload(
          &mut reader,
          segment.id,
          frame_offset,
          &header,
          include_payload && include_frame,
        )?;

        if include_frame {
          next_cursor = Some(
            ReplicationCursor::new(header.epoch, segment.id, payload_end, header.log_index)
              .to_string(),
          );
          frames.push(json!({
            "epoch": header.epoch,
            "log_index": header.log_index,
            "segment_id": segment.id,
            "segment_offset": frame_offset,
            "bytes": frame_bytes,
            "payload_base64": payload_base64,
          }));
          total_bytes = total_bytes.saturating_add(frame_bytes);
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

fn is_stale_primary_error(error: &KiteError) -> bool {
  matches!(
    error,
    KiteError::InvalidReplication(message) if message.contains("stale primary")
  )
}

fn is_reseed_error(error: &KiteError) -> bool {
  matches!(
    error,
    KiteError::InvalidReplication(message) if message.to_ascii_lowercase().contains("reseed")
  )
}

fn is_bootstrap_quiesce_error(error: &KiteError) -> bool {
  match error {
    KiteError::InvalidReplication(message) => {
      message.contains("source primary advanced during snapshot bootstrap")
        || message.contains("source primary did not quiesce for snapshot bootstrap")
        || message.contains(SOURCE_FRAME_UNPUBLISHED_ERROR)
    }
    _ => false,
  }
}

/// Log position that matches the copied source state. A buffered primary
/// publishes a commit's frame shortly after the commit, so wait until the
/// sidecar's newest frame belongs to the source's newest WAL commit; pinning
/// an older position would make catch-up replay frames the copy already holds.
/// Frames missing below the manifest head were lost, not unpublished: the
/// manifest head is used and catch-up reports the gap.
fn bootstrap_log_position(
  runtime: &crate::replication::replica::ReplicaReplication,
  source: &SingleFileDB,
) -> Result<(u64, u64)> {
  let source_sidecar_path = runtime.source_sidecar_path().ok_or_else(|| {
    KiteError::InvalidReplication("replica source sidecar path is not configured".to_string())
  })?;
  if primary_sidecar_needs_repair(&source_sidecar_path)? {
    return Err(KiteError::InvalidReplication(
      "source primary replication sidecar needs repair/resync; it cannot anchor a snapshot bootstrap"
        .to_string(),
    ));
  }

  let published = runtime.source_published_head()?;
  let (newest_index, newest_txid) = published.newest_frame.unwrap_or((0, None));
  if newest_index < published.manifest_head {
    return Ok((published.epoch, published.manifest_head));
  }
  if let Some(txid) = source_last_committed_txid(source)? {
    if newest_txid != Some(txid) {
      return Err(KiteError::InvalidReplication(format!(
        "{SOURCE_FRAME_UNPUBLISHED_ERROR} (source txid {txid}, newest sidecar frame txid {}); quiesce writes and retry",
        newest_txid.map_or_else(|| "none".to_string(), |txid| txid.to_string())
      )));
    }
  }
  Ok((published.epoch, newest_index))
}

/// Newest committed transaction in the source WAL, or `None` once a
/// checkpoint has folded every commit into the snapshot.
fn source_last_committed_txid(source: &SingleFileDB) -> Result<Option<TxId>> {
  let header = source.header.read().clone();
  if header.wal_head == 0 {
    return Ok(None);
  }
  let mut pager = source.pager.lock();
  let records = scan_wal_records(&mut pager, &header)?;
  Ok(
    committed_transactions(&records)
      .last()
      .map(|(txid, _)| *txid),
  )
}

fn is_bootstrap_retryable_error(error: &KiteError) -> bool {
  is_bootstrap_quiesce_error(error)
    || matches!(
      error,
      KiteError::CrcMismatch { .. }
        | KiteError::InvalidMagic { .. }
        | KiteError::InvalidSnapshot(_)
        | KiteError::InvalidWal(_)
        | KiteError::Io(_)
    )
}

fn read_snapshot_transport_payload(
  path: &Path,
  include_data: bool,
) -> Result<(u64, String, Option<String>)> {
  let metadata = std::fs::metadata(path)?;
  if include_data && metadata.len() > REPLICATION_SNAPSHOT_INLINE_MAX_BYTES {
    return Err(KiteError::InvalidReplication(format!(
      "snapshot size {} exceeds max inline payload {} bytes",
      metadata.len(),
      REPLICATION_SNAPSHOT_INLINE_MAX_BYTES
    )));
  }

  let mut reader = BufReader::new(File::open(path)?);
  let mut hasher = Crc32cHasher::new();
  let mut bytes_read = 0u64;
  let mut chunk = [0u8; REPLICATION_IO_CHUNK_BYTES];

  if include_data {
    let mut encoder = base64::write::EncoderWriter::new(Vec::new(), &BASE64_STANDARD);
    loop {
      let read = reader.read(&mut chunk)?;
      if read == 0 {
        break;
      }

      let payload = &chunk[..read];
      bytes_read = bytes_read.saturating_add(read as u64);
      if bytes_read > REPLICATION_SNAPSHOT_INLINE_MAX_BYTES {
        return Err(KiteError::InvalidReplication(format!(
          "snapshot size {bytes_read} exceeds max inline payload {REPLICATION_SNAPSHOT_INLINE_MAX_BYTES} bytes"
        )));
      }
      hasher.update(payload);
      encoder.write_all(payload)?;
    }

    let encoded = String::from_utf8(encoder.finish()?).map_err(|error| {
      KiteError::Serialization(format!("snapshot base64 encoding failed: {error}"))
    })?;
    return Ok((
      bytes_read,
      format!("{:08x}", hasher.finalize()),
      Some(encoded),
    ));
  }

  loop {
    let read = reader.read(&mut chunk)?;
    if read == 0 {
      break;
    }
    bytes_read = bytes_read.saturating_add(read as u64);
    hasher.update(&chunk[..read]);
  }

  Ok((bytes_read, format!("{:08x}", hasher.finalize()), None))
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

fn le_u16(bytes: &[u8]) -> Result<u16> {
  let value: [u8; 2] = bytes
    .try_into()
    .map_err(|_| KiteError::InvalidReplication("invalid frame u16 field".to_string()))?;
  Ok(u16::from_le_bytes(value))
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

#[derive(Debug, Clone, Copy)]
struct ParsedFrameHeader {
  epoch: u64,
  log_index: u64,
  payload_len: usize,
  stored_crc32: u32,
  crc_disabled: bool,
}

fn read_frame_header(
  reader: &mut BufReader<File>,
  segment_id: u64,
  frame_offset: u64,
) -> Result<Option<ParsedFrameHeader>> {
  let mut header_bytes = [0u8; REPLICATION_FRAME_HEADER_BYTES];
  let mut filled = 0usize;
  while filled < REPLICATION_FRAME_HEADER_BYTES {
    let read = reader.read(&mut header_bytes[filled..])?;
    if read == 0 {
      if filled == 0 {
        return Ok(None);
      }
      return Err(KiteError::InvalidReplication(format!(
        "replication frame truncated in segment {segment_id} at byte {frame_offset}"
      )));
    }
    filled = filled.saturating_add(read);
  }

  parse_frame_header(&header_bytes, segment_id, frame_offset).map(Some)
}

fn parse_frame_header(
  header_bytes: &[u8; REPLICATION_FRAME_HEADER_BYTES],
  segment_id: u64,
  frame_offset: u64,
) -> Result<ParsedFrameHeader> {
  let magic = le_u32(&header_bytes[0..4])?;
  if magic != REPLICATION_FRAME_MAGIC {
    return Err(KiteError::InvalidReplication(format!(
      "invalid replication frame magic 0x{magic:08X} in segment {segment_id} at byte {frame_offset}"
    )));
  }

  let version = le_u16(&header_bytes[4..6])?;
  if version != REPLICATION_FRAME_VERSION {
    return Err(KiteError::VersionMismatch {
      required: version as u32,
      current: REPLICATION_FRAME_VERSION as u32,
    });
  }

  let flags = le_u16(&header_bytes[6..8])?;
  if flags & !REPLICATION_FRAME_FLAG_CRC32_DISABLED != 0 {
    return Err(KiteError::InvalidReplication(format!(
      "unsupported replication frame flags 0x{flags:04X} in segment {segment_id} at byte {frame_offset}"
    )));
  }

  let payload_len = le_u32(&header_bytes[24..28])? as usize;
  if payload_len > REPLICATION_MAX_FRAME_PAYLOAD_BYTES {
    return Err(KiteError::InvalidReplication(format!(
      "frame payload exceeds limit: {payload_len}"
    )));
  }

  Ok(ParsedFrameHeader {
    epoch: le_u64(&header_bytes[8..16])?,
    log_index: le_u64(&header_bytes[16..24])?,
    payload_len,
    stored_crc32: le_u32(&header_bytes[28..32])?,
    crc_disabled: (flags & REPLICATION_FRAME_FLAG_CRC32_DISABLED) != 0,
  })
}

fn read_frame_payload(
  reader: &mut BufReader<File>,
  segment_id: u64,
  frame_offset: u64,
  header: &ParsedFrameHeader,
  capture_base64: bool,
) -> Result<Option<String>> {
  if capture_base64 {
    let mut payload = vec![0u8; header.payload_len];
    reader
      .read_exact(&mut payload)
      .map_err(|error| map_frame_payload_read_error(error, segment_id, frame_offset))?;
    if !header.crc_disabled {
      let computed_crc32 = crc32c(&payload);
      if computed_crc32 != header.stored_crc32 {
        return Err(KiteError::CrcMismatch {
          stored: header.stored_crc32,
          computed: computed_crc32,
        });
      }
    }
    return Ok(Some(BASE64_STANDARD.encode(payload)));
  }

  let mut hasher = (!header.crc_disabled).then(Crc32cHasher::new);
  consume_payload_stream(reader, header.payload_len, |chunk| {
    if let Some(hasher) = hasher.as_mut() {
      hasher.update(chunk);
    }
  })
  .map_err(|error| map_frame_payload_read_error(error, segment_id, frame_offset))?;

  if let Some(hasher) = hasher {
    let computed_crc32 = hasher.finalize();
    if computed_crc32 != header.stored_crc32 {
      return Err(KiteError::CrcMismatch {
        stored: header.stored_crc32,
        computed: computed_crc32,
      });
    }
  }

  Ok(None)
}

fn consume_payload_stream(
  reader: &mut BufReader<File>,
  payload_len: usize,
  mut visit: impl FnMut(&[u8]),
) -> std::io::Result<()> {
  let mut remaining = payload_len;
  let mut chunk = [0u8; REPLICATION_IO_CHUNK_BYTES];
  while remaining > 0 {
    let want = remaining.min(chunk.len());
    let read = reader.read(&mut chunk[..want])?;
    if read == 0 {
      return Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "replication frame payload truncated",
      ));
    }
    visit(&chunk[..read]);
    remaining -= read;
  }
  Ok(())
}

fn map_frame_payload_read_error(
  error: std::io::Error,
  segment_id: u64,
  frame_offset: u64,
) -> KiteError {
  if error.kind() == std::io::ErrorKind::UnexpectedEof {
    KiteError::InvalidReplication(format!(
      "replication frame truncated in segment {segment_id} at byte {frame_offset}"
    ))
  } else {
    KiteError::Io(error)
  }
}

fn source_db_fingerprint(path: &Path) -> Result<(u64, u32)> {
  let mut reader = BufReader::new(File::open(path)?);
  let mut hasher = Crc32cHasher::new();
  let mut chunk = [0u8; REPLICATION_IO_CHUNK_BYTES];
  let mut bytes = 0u64;

  loop {
    let read = reader.read(&mut chunk)?;
    if read == 0 {
      break;
    }
    hasher.update(&chunk[..read]);
    bytes = bytes.saturating_add(read as u64);
  }

  Ok((bytes, hasher.finalize()))
}

/// Build the schema translation from names: every source name gets a local
/// id (its existing one, else the source's id when free here).
fn sync_schema_names(
  replica: &SingleFileDB,
  source: &SingleFileDB,
  epoch: u64,
) -> Result<ReplicaSchemaMap> {
  let labels = sorted_schema_entries(&source.label_ids.read());
  let etypes = sorted_schema_entries(&source.etype_ids.read());
  let propkeys = sorted_schema_entries(&source.propkey_ids.read());
  let mut schema_map = ReplicaSchemaMap::for_epoch(epoch);
  for (id, name) in labels {
    schema_map.insert(Label, id, replica.ensure_replica_label(&name, id)?);
  }
  for (id, name) in etypes {
    schema_map.insert(EdgeType, id, replica.ensure_replica_etype(&name, id)?);
  }
  for (id, name) in propkeys {
    schema_map.insert(PropertyKey, id, replica.ensure_replica_propkey(&name, id)?);
  }
  Ok(schema_map)
}

fn sorted_schema_entries(ids: &HashMap<u32, String>) -> Vec<(u32, String)> {
  let mut entries: Vec<_> = ids.iter().map(|(&id, name)| (id, name.clone())).collect();
  entries.sort_unstable_by_key(|&(id, _)| id);
  entries
}

fn translate_prop_map(
  db: &SingleFileDB,
  schema_map: &mut ReplicaSchemaMap,
  props: HashMap<PropKeyId, PropValue>,
) -> Result<HashMap<PropKeyId, PropValue>> {
  props
    .into_iter()
    .map(|(key_id, value)| Ok((local_schema_id(db, schema_map, PropertyKey, key_id)?, value)))
    .collect()
}

/// Bootstrap phase 1: delete every replica node that is missing from the
/// source or holds a different key, in its own transaction. All deletes must
/// precede all creates (keys move between nodes and an id can return with a
/// new key), and the delta cannot delete and recreate one id inside a single
/// transaction. Until the bootstrap completes, catch-up refuses to run over
/// the partially removed state.
///
/// Recreating an id whose deleted copy lives in the snapshot leaves the new
/// node hidden behind the delete tombstone, so such deletes are folded into
/// a new snapshot before phase 2 recreates the ids.
fn remove_stale_nodes(
  replica: &SingleFileDB,
  source: &SingleFileDB,
  runtime: &crate::replication::replica::ReplicaReplication,
) -> Result<()> {
  let stale: Vec<NodeId> = replica
    .list_nodes()
    .into_iter()
    .filter(|&node_id| {
      !source.node_exists(node_id) || source.node_key(node_id) != replica.node_key(node_id)
    })
    .collect();
  if stale.is_empty() {
    return Ok(());
  }
  let recreates_snapshot_node = {
    let snapshot = replica.snapshot.read();
    stale.iter().any(|&node_id| {
      source.node_exists(node_id)
        && snapshot
          .as_ref()
          .is_some_and(|snapshot| snapshot.phys_node(node_id).is_some())
    })
  };

  runtime.mark_bootstrap_incomplete()?;
  let tx_guard = replica.begin_guard(false)?;
  for node_id in stale {
    replica.delete_node(node_id)?;
  }
  tx_guard.commit()?;
  if recreates_snapshot_node {
    replica.checkpoint()?;
  }
  Ok(())
}

/// Bootstrap phase 2: create the source's nodes and copy schema, properties,
/// labels, vectors, and edges, translating the source's schema ids. Returns
/// the translation for the replica to persist.
fn sync_graph_state<F>(
  replica: &SingleFileDB,
  source: &SingleFileDB,
  epoch: u64,
  before_commit: F,
) -> Result<ReplicaSchemaMap>
where
  F: FnOnce() -> Result<()>,
{
  let tx_guard = replica.begin_guard(false)?;

  let mut schema_map = sync_schema_names(replica, source, epoch)?;

  // Phase 1 removed every node whose key differs, so existing ones match.
  let source_nodes = source.list_nodes();
  for &node_id in &source_nodes {
    if !replica.node_exists(node_id) {
      replica.create_node_with_id(node_id, source.node_key(node_id).as_deref())?;
    }
  }

  for &node_id in &source_nodes {
    let source_props = translate_prop_map(
      replica,
      &mut schema_map,
      source.node_props(node_id).unwrap_or_default(),
    )?;
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

    let source_labels = source
      .node_labels(node_id)
      .into_iter()
      .map(|label_id| local_schema_id(replica, &mut schema_map, Label, label_id))
      .collect::<Result<HashSet<_>>>()?;
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

  // Local vector key -> source key; replica-only keys have no source values.
  let mut vector_prop_keys: HashMap<PropKeyId, Option<PropKeyId>> = replica
    .vector_prop_keys()
    .into_iter()
    .map(|local_key| (local_key, None))
    .collect();
  for source_key in source.vector_prop_keys() {
    vector_prop_keys.insert(
      local_schema_id(replica, &mut schema_map, PropertyKey, source_key)?,
      Some(source_key),
    );
  }
  for &node_id in &source_nodes {
    for (&prop_key_id, &source_key) in &vector_prop_keys {
      let source_vector = source_key.and_then(|key| source.node_vector(node_id, key));
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

  // (src, source etype, local etype, dst)
  let source_edges = source
    .list_edges(None)
    .into_iter()
    .map(|edge| {
      Ok((
        edge.src,
        edge.etype,
        local_schema_id(replica, &mut schema_map, EdgeType, edge.etype)?,
        edge.dst,
      ))
    })
    .collect::<Result<Vec<_>>>()?;
  let source_edge_set: HashSet<_> = source_edges
    .iter()
    .map(|&(src, _, etype, dst)| (src, etype, dst))
    .collect();

  // A dangling source edge (legacy data) has no live endpoint to attach to.
  for &(src, _, etype, dst) in &source_edges {
    if endpoints_exist(replica, src, dst) && !replica.edge_exists(src, etype, dst) {
      replica.add_edge(src, etype, dst)?;
    }
  }

  for edge in replica.list_edges(None) {
    if !source_edge_set.contains(&(edge.src, edge.etype, edge.dst)) {
      replica.delete_edge(edge.src, edge.etype, edge.dst)?;
    }
  }

  for (src, source_etype, etype, dst) in source_edges {
    if !edge_is_live(replica, src, etype, dst) {
      continue;
    }
    let source_props = translate_prop_map(
      replica,
      &mut schema_map,
      source
        .edge_props(src, source_etype, dst)
        .unwrap_or_default(),
    )?;
    let replica_props = replica.edge_props(src, etype, dst).unwrap_or_default();

    for (&key_id, value) in &source_props {
      if replica_props.get(&key_id) != Some(value) {
        replica.set_edge_prop(src, etype, dst, key_id, value.clone())?;
      }
    }
    for &key_id in replica_props.keys() {
      if !source_props.contains_key(&key_id) {
        replica.delete_edge_prop(src, etype, dst, key_id)?;
      }
    }
  }

  before_commit()?;
  tx_guard.commit()?;
  Ok(schema_map)
}

fn apply_replication_frame(
  db: &SingleFileDB,
  payload: &[u8],
  schema_map: &mut ReplicaSchemaMap,
) -> Result<()> {
  let decoded = decode_commit_frame_payload(payload)?;
  let records = parse_wal_records(&decoded.wal_bytes)?;

  if records.is_empty() {
    return Ok(());
  }

  let tx_guard = db.begin_guard(false)?;
  for record in &records {
    apply_wal_record_idempotent(db, record, schema_map)?;
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

/// A node's key is held by a different live node. Keys are unique, so a
/// replayed create for this node was superseded by a later delete that the
/// replica already applied; recreating it would steal the key.
fn key_held_by_other_node(db: &SingleFileDB, node_id: NodeId, key: Option<&str>) -> bool {
  key
    .and_then(|key| db.node_by_key(key))
    .is_some_and(|holder| holder != node_id)
}

fn endpoints_exist(db: &SingleFileDB, src: NodeId, dst: NodeId) -> bool {
  db.node_exists(src) && db.node_exists(dst)
}

fn edge_is_live(db: &SingleFileDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
  endpoints_exist(db, src, dst) && db.edge_exists(src, etype, dst)
}

/// Apply one primary WAL record so that applying it again over newer state is
/// a no-op. The replica cursor file is written after the frame commits, so a
/// crash in between replays frames the replica already holds; a later delete
/// may have removed the entities they touch. On the primary, keys are unique
/// and edges, labels, and properties belong to live nodes, so a record that
/// reaches a missing node or edge comes from such a replay and is skipped
/// (before any write call) instead of resurrecting it. The replayed suffix
/// then converges on the primary's state.
///
/// Schema ids in the record are the primary's; `schema_map` translates them
/// to this replica's ids and learns new names from define records.
fn apply_wal_record_idempotent(
  db: &SingleFileDB,
  record: &ParsedWalRecord,
  schema_map: &mut ReplicaSchemaMap,
) -> Result<()> {
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
      if key_held_by_other_node(db, data.node_id, data.key.as_deref()) {
        return Ok(());
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
        if key_held_by_other_node(db, entry.node_id, entry.key.as_deref()) {
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
        db.delete_node(data.node_id)?;
      }
      Ok(())
    }
    WalRecordType::AddEdge => {
      let data = parse_add_edge_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdge replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      if endpoints_exist(db, data.src, data.dst) && !db.edge_exists(data.src, etype, data.dst) {
        db.add_edge(data.src, etype, data.dst)?;
      }
      Ok(())
    }
    WalRecordType::DeleteEdge => {
      let data = parse_delete_edge_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DeleteEdge replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      if db.edge_exists(data.src, etype, data.dst) {
        db.delete_edge(data.src, etype, data.dst)?;
      }
      Ok(())
    }
    WalRecordType::AddEdgesBatch => {
      let batch = parse_add_edges_batch_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgesBatch replication payload".to_string())
      })?;

      for edge in batch {
        let etype = local_schema_id(db, schema_map, EdgeType, edge.etype)?;
        if endpoints_exist(db, edge.src, edge.dst) && !db.edge_exists(edge.src, etype, edge.dst) {
          db.add_edge(edge.src, etype, edge.dst)?;
        }
      }
      Ok(())
    }
    WalRecordType::AddEdgeProps => {
      let data = parse_add_edge_props_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgeProps replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      apply_edge_with_props(db, schema_map, data.src, etype, data.dst, data.props)
    }
    WalRecordType::AddEdgesPropsBatch => {
      let batch = parse_add_edges_props_batch_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddEdgesPropsBatch replication payload".to_string())
      })?;

      for entry in batch {
        let etype = local_schema_id(db, schema_map, EdgeType, entry.etype)?;
        apply_edge_with_props(db, schema_map, entry.src, etype, entry.dst, entry.props)?;
      }
      Ok(())
    }
    WalRecordType::SetNodeProp => {
      let data = parse_set_node_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetNodeProp replication payload".to_string())
      })?;
      let key_id = local_schema_id(db, schema_map, PropertyKey, data.key_id)?;

      if db.node_exists(data.node_id)
        && db.node_prop(data.node_id, key_id).as_ref() != Some(&data.value)
      {
        db.set_node_prop(data.node_id, key_id, data.value)?;
      }
      Ok(())
    }
    WalRecordType::DelNodeProp => {
      let data = parse_del_node_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelNodeProp replication payload".to_string())
      })?;
      let key_id = local_schema_id(db, schema_map, PropertyKey, data.key_id)?;

      if db.node_prop(data.node_id, key_id).is_some() {
        db.delete_node_prop(data.node_id, key_id)?;
      }
      Ok(())
    }
    WalRecordType::SetEdgeProp => {
      let data = parse_set_edge_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetEdgeProp replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      let key_id = local_schema_id(db, schema_map, PropertyKey, data.key_id)?;

      if edge_is_live(db, data.src, etype, data.dst)
        && db.edge_prop(data.src, etype, data.dst, key_id).as_ref() != Some(&data.value)
      {
        db.set_edge_prop(data.src, etype, data.dst, key_id, data.value)?;
      }
      Ok(())
    }
    WalRecordType::SetEdgeProps => {
      let data = parse_set_edge_props_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetEdgeProps replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      let props = translate_props(db, schema_map, data.props)?;

      if !edge_is_live(db, data.src, etype, data.dst) {
        return Ok(());
      }
      for (key_id, value) in props {
        if db.edge_prop(data.src, etype, data.dst, key_id).as_ref() != Some(&value) {
          db.set_edge_prop(data.src, etype, data.dst, key_id, value)?;
        }
      }
      Ok(())
    }
    WalRecordType::DelEdgeProp => {
      let data = parse_del_edge_prop_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelEdgeProp replication payload".to_string())
      })?;
      let etype = local_schema_id(db, schema_map, EdgeType, data.etype)?;
      let key_id = local_schema_id(db, schema_map, PropertyKey, data.key_id)?;

      if db.edge_prop(data.src, etype, data.dst, key_id).is_some() {
        db.delete_edge_prop(data.src, etype, data.dst, key_id)?;
      }
      Ok(())
    }
    WalRecordType::AddNodeLabel => {
      let data = parse_add_node_label_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid AddNodeLabel replication payload".to_string())
      })?;
      let label_id = local_schema_id(db, schema_map, Label, data.label_id)?;

      if db.node_exists(data.node_id) && !db.node_has_label(data.node_id, label_id) {
        db.add_node_label(data.node_id, label_id)?;
      }
      Ok(())
    }
    WalRecordType::RemoveNodeLabel => {
      let data = parse_remove_node_label_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid RemoveNodeLabel replication payload".to_string())
      })?;
      let label_id = local_schema_id(db, schema_map, Label, data.label_id)?;

      if db.node_has_label(data.node_id, label_id) {
        db.remove_node_label(data.node_id, label_id)?;
      }
      Ok(())
    }
    WalRecordType::SetNodeVector => {
      let data = parse_set_node_vector_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid SetNodeVector replication payload".to_string())
      })?;
      let prop_key_id = local_schema_id(db, schema_map, PropertyKey, data.prop_key_id)?;

      if !db.node_exists(data.node_id) {
        return Ok(());
      }
      let current = db.node_vector(data.node_id, prop_key_id);
      if current.as_deref() != Some(data.vector.as_slice()) {
        db.set_node_vector(data.node_id, prop_key_id, &data.vector)?;
      }
      Ok(())
    }
    WalRecordType::DelNodeVector => {
      let data = parse_del_node_vector_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DelNodeVector replication payload".to_string())
      })?;
      let prop_key_id = local_schema_id(db, schema_map, PropertyKey, data.prop_key_id)?;

      if db.has_node_vector(data.node_id, prop_key_id) {
        db.delete_node_vector(data.node_id, prop_key_id)?;
      }
      Ok(())
    }
    WalRecordType::DefineLabel => {
      let data = parse_define_label_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DefineLabel replication payload".to_string())
      })?;
      let local_id = db.ensure_replica_label(&data.name, data.label_id)?;
      schema_map.insert(Label, data.label_id, local_id);
      Ok(())
    }
    WalRecordType::DefineEtype => {
      let data = parse_define_etype_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DefineEtype replication payload".to_string())
      })?;
      let local_id = db.ensure_replica_etype(&data.name, data.label_id)?;
      schema_map.insert(EdgeType, data.label_id, local_id);
      Ok(())
    }
    WalRecordType::DefinePropkey => {
      let data = parse_define_propkey_payload(&record.payload).ok_or_else(|| {
        KiteError::InvalidReplication("invalid DefinePropkey replication payload".to_string())
      })?;
      let local_id = db.ensure_replica_propkey(&data.name, data.label_id)?;
      schema_map.insert(PropertyKey, data.label_id, local_id);
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

/// Local id for a primary schema id. Named ids translate through the map. The
/// raw API also accepts ids that were never named; such an id keeps its
/// number, but only while no local name claims that number, so data never
/// lands under a different name. Anything else needs a reseed.
fn local_schema_id(
  db: &SingleFileDB,
  schema_map: &mut ReplicaSchemaMap,
  kind: SchemaIdKind,
  primary_id: u32,
) -> Result<u32> {
  if let Some(local_id) = schema_map.get(kind, primary_id) {
    return Ok(local_id);
  }
  let locally_named = match kind {
    Label => db.label_name(primary_id).is_some(),
    EdgeType => db.etype_name(primary_id).is_some(),
    PropertyKey => db.propkey_name(primary_id).is_some(),
  };
  if locally_named {
    return Err(KiteError::InvalidReplication(format!(
      "replica has no translation for primary {} id {primary_id} and that id names another \
       local {0}; reseed required",
      kind.noun()
    )));
  }
  schema_map.insert(kind, primary_id, primary_id);
  Ok(primary_id)
}

fn translate_props(
  db: &SingleFileDB,
  schema_map: &mut ReplicaSchemaMap,
  props: Vec<(PropKeyId, PropValue)>,
) -> Result<Vec<(PropKeyId, PropValue)>> {
  props
    .into_iter()
    .map(|(key_id, value)| Ok((local_schema_id(db, schema_map, PropertyKey, key_id)?, value)))
    .collect()
}

/// Add an edge (translated etype) with props in primary ids, unless an
/// endpoint was already deleted by newer state.
fn apply_edge_with_props(
  db: &SingleFileDB,
  schema_map: &mut ReplicaSchemaMap,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  props: Vec<(PropKeyId, PropValue)>,
) -> Result<()> {
  let props = translate_props(db, schema_map, props)?;
  if !endpoints_exist(db, src, dst) {
    return Ok(());
  }
  if !db.edge_exists(src, etype, dst) {
    db.add_edge(src, etype, dst)?;
  }
  for (key_id, value) in props {
    if db.edge_prop(src, etype, dst, key_id).as_ref() != Some(&value) {
      db.set_edge_prop(src, etype, dst, key_id, value)?;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::apply_wal_record_idempotent;
  use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
  use crate::core::wal::record::ParsedWalRecord;
  use crate::replication::replica::ReplicaSchemaMap;
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
      apply_wal_record_idempotent(&db, &record, &mut ReplicaSchemaMap::for_epoch(1))
        .expect("derived vector maintenance should be ignored");
    }

    assert_eq!(db.count_nodes(), 0);
    assert_eq!(db.count_edges(), 0);
    close_single_file(db).expect("close db");
  }
}
