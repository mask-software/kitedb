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
use crate::replication::log_store::ReplicationFrame;
use crate::replication::manifest::ManifestStore;
use crate::replication::primary::{primary_sidecar_needs_repair, PrimaryRetentionOutcome};
use crate::replication::replica::{
  ReplicaReplication, ReplicaReplicationStatus, ReplicaSchemaMap,
  SchemaIdKind::{self, EdgeType, Label, PropertyKey},
};
use crate::replication::transport::{
  decode_commit_frame_payload, parse_transport_cursor, LogTransportFrame, LogTransportPage,
  SnapshotTransport, SNAPSHOT_TRANSPORT_FORMAT,
};
use crate::replication::types::{CommitToken, ReplicationCursor};
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue, TxId, WalRecordType};
use crate::util::crc::{crc32, Crc32Hasher};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::open::{open_replication_source, SyncMode};
use super::recovery::{committed_transactions, scan_wal_records};
use super::transaction::SingleFileTxGuard;
use super::{close_single_file, SingleFileDB};

const REPLICATION_MANIFEST_FILE: &str = "manifest.json";
const REPLICATION_FRAME_MAGIC: u32 = 0x474F_4C52;
const REPLICATION_FRAME_VERSION: u16 = 1;
const REPLICATION_FRAME_FLAG_CRC32_DISABLED: u16 = 0x0001;
const REPLICATION_FRAME_HEADER_BYTES: usize = 32;
const REPLICATION_MAX_FRAME_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const REPLICATION_IO_CHUNK_BYTES: usize = 64 * 1024;
/// Largest snapshot the JSON transport inlines: base64 inside a JSON string
/// holds about 2.7 copies of the data in memory at once.
const REPLICATION_SNAPSHOT_JSON_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Largest snapshot the binary transport inlines.
const REPLICATION_SNAPSHOT_BINARY_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// A snapshot bootstrap commits after this many writes...
const BOOTSTRAP_BATCH_MAX_WRITES: usize = 10_000;
/// ...or once its transaction's WAL records reach this share of the WAL.
const BOOTSTRAP_BATCH_WAL_SHARE: u64 = 8;
/// Catch-up applies a run of frames in one transaction until their WAL
/// records reach this share of the replica's WAL.
const CATCH_UP_BATCH_WAL_SHARE: u64 = 8;
/// Floor of both batch budgets, for small WALs.
const MIN_BATCH_WAL_BYTES: u64 = 16 * 1024;
/// Both header pages, at the largest page size.
const SOURCE_FINGERPRINT_HEAD_BYTES: u64 = 2 * 64 * 1024;
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

  /// Forget a replica's reported progress, so a decommissioned replica stops
  /// holding back retention (the next `primary_run_retention` applies it).
  /// Returns whether the replica had progress recorded. A replica that
  /// reports progress again is tracked again.
  pub fn primary_remove_replica_progress(&self, replica_id: &str) -> Result<bool> {
    self
      .primary_replication
      .as_ref()
      .ok_or_else(|| {
        KiteError::InvalidReplication("database is not opened in primary role".to_string())
      })?
      .remove_replica_progress(replica_id)
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
  ///
  /// The source's state is copied in bounded transactions; the replica is
  /// marked incomplete before the first one commits, until the cursor is set
  /// at the end (once the copy is durable), so catch-up never runs over a
  /// partial copy. The source must
  /// stay quiet for the copy: a change to its file (length, modification
  /// time, header) or to its replication head between the start and the end
  /// makes the attempt retry.
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

      let sync_result = (|| {
        let start = SourceState::read(runtime, &source_db_path)?;
        let bootstrap_position = bootstrap_log_position(runtime, &source)?;
        std::thread::sleep(Duration::from_millis(10));
        start.check_quiet(runtime, &source_db_path, "did not quiesce for")?;

        let mut batch = BootstrapBatch::new(self, runtime);
        remove_stale_nodes(&source, &mut batch)?;
        let schema_map = sync_graph_state(&source, bootstrap_position.0, &mut batch)?;
        batch.finish()?;

        start.check_quiet(runtime, &source_db_path, "advanced during")?;
        std::thread::sleep(Duration::from_millis(10));
        start.check_quiet(runtime, &source_db_path, "did not quiesce for")?;
        Ok((bootstrap_position, schema_map))
      })()
      .and_then(|((epoch, log_index), schema_map)| {
        self.make_applied_durable()?;
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

  /// Apply the frames after the cursor: each contiguous run in one
  /// transaction (split when its WAL records outgrow the batch budget), with
  /// one cursor update at the end, after the applied commits are durable
  /// (`make_applied_durable`). A run that fails is applied again one frame
  /// per transaction, which applies the frames before the failing one, moves
  /// the cursor past them, and names the failing frame.
  fn replica_catch_up_attempt(
    &self,
    runtime: &ReplicaReplication,
    max_frames: usize,
    replay_last: bool,
  ) -> Result<usize> {
    let frames = runtime.frames_after(max_frames, replay_last)?;
    if frames.is_empty() {
      runtime.clear_error()?;
      return Ok(0);
    }

    let (applied_epoch, applied_log_index) = runtime.applied_position();
    let pending: Vec<&ReplicationFrame> = frames
      .iter()
      .filter(|frame| {
        frame.epoch > applied_epoch
          || (frame.epoch == applied_epoch && frame.log_index > applied_log_index)
      })
      .collect();

    let mut progress = ApplyProgress {
      schema_map: runtime.schema_map(),
      position: (applied_epoch, applied_log_index),
      applied: 0,
    };
    let budget = batch_wal_budget(self, CATCH_UP_BATCH_WAL_SHARE);
    let mut outcome = Ok(());
    for run in frame_runs(&pending, budget) {
      let applied = apply_frames(self, run, &mut progress).or_else(|error| {
        if run.len() == 1 {
          return Err(error);
        }
        run
          .iter()
          .try_for_each(|frame| apply_frames(self, std::slice::from_ref(frame), &mut progress))
      });
      if let Err(error) = applied {
        outcome = Err(error);
        break;
      }
    }

    if progress.applied > 0 {
      let (epoch, log_index) = progress.position;
      let persisted = self
        .make_applied_durable()
        .and_then(|()| runtime.store_schema_map(progress.schema_map))
        .and_then(|_| runtime.mark_applied(epoch, log_index));
      if let Err(error) = persisted {
        return Err(match outcome {
          Err(apply_error) => apply_error.into_error(),
          Ok(()) => KiteError::InvalidReplication(format!(
            "replica cursor persist failed at {epoch}:{log_index}: {error}"
          )),
        });
      }
    }
    outcome.map_err(FrameApplyError::into_error)?;

    runtime.clear_error()?;
    Ok(progress.applied)
  }

  /// Make this replica's applied commits durable, before a cursor that
  /// covers them is written: the cursor file is synced at once, and a cursor
  /// that survives a crash its data did not makes catch-up skip that data.
  /// Full-mode commits are durable already. In Normal mode a commit's WAL
  /// and header are written but not synced, and in Off mode not written at
  /// all: write them and sync, once per pull, as close does.
  fn make_applied_durable(&self) -> Result<()> {
    if self.sync_mode == SyncMode::Full {
      return Ok(());
    }
    self.persist_for_close()
  }

  /// Export a snapshot of this primary, with a copy of the database file
  /// when `include_data` (up to 1 GiB).
  ///
  /// The copy is consistent and matches the log position it reports: it is
  /// read under the checkpoint gate and the commit lock, as backups are, so
  /// no commit, checkpoint, optimize or vacuum changes the file meanwhile,
  /// and commits append their frames under that lock. In `SyncMode::Off`
  /// the commits held only in memory are written to the file first. Commits
  /// wait for the copy.
  pub fn primary_export_snapshot_transport(&self, include_data: bool) -> Result<SnapshotTransport> {
    self.export_snapshot_transport(include_data, REPLICATION_SNAPSHOT_BINARY_MAX_BYTES)
  }

  /// [`Self::primary_export_snapshot_transport`] as transport JSON, with the
  /// data in base64 (up to 32 MiB of data).
  pub fn primary_export_snapshot_transport_json(&self, include_data: bool) -> Result<String> {
    self
      .export_snapshot_transport(include_data, REPLICATION_SNAPSHOT_JSON_MAX_BYTES)?
      .into_json()
  }

  fn export_snapshot_transport(
    &self,
    include_data: bool,
    max_data_bytes: u64,
  ) -> Result<SnapshotTransport> {
    let replication = self.primary_replication.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("database is not opened in primary role".to_string())
    })?;
    // The copy holds the checkpoint gate, and a blocking checkpoint that
    // waits for this thread's open transaction to finish would never get it.
    if self.has_transaction() {
      return Err(KiteError::TransactionInProgress);
    }

    let (position, (byte_length, checksum_crc32, data)) = {
      let _checkpoint_gate = self.checkpoint_gate.read();
      let _commit_guard = self.commit_lock.lock();
      if self.sync_mode == SyncMode::Off {
        self.persist_for_close()?;
      }
      (
        replication.snapshot_position(),
        read_database_copy(&self.path, include_data, max_data_bytes)?,
      )
    };

    Ok(SnapshotTransport {
      format: SNAPSHOT_TRANSPORT_FORMAT,
      byte_length,
      checksum_crc32,
      generated_at_ms: SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64,
      epoch: position.epoch,
      head_log_index: position.head_log_index,
      retained_floor: position.retained_floor,
      generation: position.generation,
      start_cursor: position.start_cursor,
      data,
    })
  }

  /// Export a page of this primary's replication frames after `cursor` (from
  /// the start of the retained log when `None`): at most `max_frames`
  /// frames and `max_bytes` bytes, a frame larger than `max_bytes` being an
  /// error. Payloads are included when `include_payload`.
  pub fn primary_export_log_transport(
    &self,
    cursor: Option<ReplicationCursor>,
    max_frames: usize,
    max_bytes: usize,
    include_payload: bool,
  ) -> Result<LogTransportPage> {
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

    let mut segments = manifest.segments.clone();
    segments.sort_by_key(|segment| segment.id);

    let mut frames = Vec::new();
    let mut total_bytes = 0usize;
    let mut next_cursor = None;
    let mut limited = false;

    'outer: for segment in segments {
      let segment_path = sidecar_path.join(format_segment_file_name(segment.id));
      if !segment_path.exists() {
        continue;
      }

      let mut reader = BufReader::new(File::open(&segment_path)?);
      let mut offset = 0u64;
      while let Some(header) = read_frame_header(&mut reader, segment.id, offset)? {
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
          cursor,
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

        let payload = read_frame_payload(
          &mut reader,
          segment.id,
          frame_offset,
          &header,
          include_payload && include_frame,
        )?;

        if include_frame {
          next_cursor = Some(ReplicationCursor::new(
            header.epoch,
            segment.id,
            payload_end,
            header.log_index,
          ));
          frames.push(LogTransportFrame {
            epoch: header.epoch,
            log_index: header.log_index,
            segment_id: segment.id,
            segment_offset: frame_offset,
            bytes: frame_bytes as u64,
            payload,
          });
          total_bytes = total_bytes.saturating_add(frame_bytes);
        }

        offset = payload_end;
      }
    }

    Ok(LogTransportPage {
      epoch: manifest.epoch,
      head_log_index: manifest.head_log_index,
      retained_floor: manifest.retained_floor,
      generation: manifest.generation,
      cursor,
      next_cursor,
      eof: !limited,
      total_bytes: total_bytes as u64,
      frames,
    })
  }

  /// [`Self::primary_export_log_transport`] as transport JSON, with the
  /// payloads in base64. `cursor` is `epoch:segment_id:segment_offset:log_index`.
  pub fn primary_export_log_transport_json(
    &self,
    cursor: Option<&str>,
    max_frames: usize,
    max_bytes: usize,
    include_payload: bool,
  ) -> Result<String> {
    let cursor = parse_transport_cursor(cursor)?;
    self
      .primary_export_log_transport(cursor, max_frames, max_bytes, include_payload)?
      .into_json()
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

/// Size, CRC-32 and (when `include_data`, up to `max_data_bytes`) a copy
/// of the database file. Callers hold the locks that keep writers out.
fn read_database_copy(
  path: &Path,
  include_data: bool,
  max_data_bytes: u64,
) -> Result<(u64, u32, Option<Vec<u8>>)> {
  let mut file = File::open(path)?;
  let len = file.metadata()?.len();
  if include_data && len > max_data_bytes {
    return Err(KiteError::InvalidReplication(format!(
      "snapshot size {len} exceeds max inline payload {max_data_bytes} bytes"
    )));
  }

  if include_data {
    let mut data = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.read_to_end(&mut data)?;
    return Ok((data.len() as u64, crc32(&data), Some(data)));
  }

  let mut reader = BufReader::with_capacity(REPLICATION_IO_CHUNK_BYTES, file);
  let mut hasher = Crc32Hasher::new();
  let mut bytes_read = 0u64;
  let mut chunk = vec![0u8; REPLICATION_IO_CHUNK_BYTES];
  loop {
    let read = reader.read(&mut chunk)?;
    if read == 0 {
      break;
    }
    bytes_read = bytes_read.saturating_add(read as u64);
    hasher.update(&chunk[..read]);
  }
  Ok((bytes_read, hasher.finalize(), None))
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
  capture: bool,
) -> Result<Option<Vec<u8>>> {
  if capture {
    let mut payload = vec![0u8; header.payload_len];
    reader
      .read_exact(&mut payload)
      .map_err(|error| map_frame_payload_read_error(error, segment_id, frame_offset))?;
    if !header.crc_disabled {
      let computed_crc32 = crc32(&payload);
      if computed_crc32 != header.stored_crc32 {
        return Err(KiteError::CrcMismatch {
          stored: header.stored_crc32,
          computed: computed_crc32,
        });
      }
    }
    return Ok(Some(payload));
  }

  let mut hasher = (!header.crc_disabled).then(Crc32Hasher::new);
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

/// What changes whenever the source's database file does: its length, its
/// modification time, and both header pages (every commit and checkpoint
/// rewrites a header). Reads at most 128 KiB, not the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceFingerprint {
  len: u64,
  modified_unix_nanos: Option<u128>,
  header_crc32: u32,
}

impl SourceFingerprint {
  fn read(path: &Path) -> Result<Self> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let modified_unix_nanos = metadata
      .modified()
      .ok()
      .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
      .map(|since| since.as_nanos());
    let mut head = Vec::new();
    file
      .take(SOURCE_FINGERPRINT_HEAD_BYTES)
      .read_to_end(&mut head)?;
    Ok(Self {
      len: metadata.len(),
      modified_unix_nanos,
      header_crc32: crc32(&head),
    })
  }
}

impl std::fmt::Display for SourceFingerprint {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "len={} mtime_ns={} header_crc={:08x}",
      self.len,
      self
        .modified_unix_nanos
        .map_or_else(|| "?".to_string(), |nanos| nanos.to_string()),
      self.header_crc32
    )
  }
}

/// The source's replication head and file at the start of a bootstrap
/// attempt.
struct SourceState {
  head: (u64, u64),
  file: SourceFingerprint,
}

impl SourceState {
  fn read(runtime: &ReplicaReplication, source_db_path: &Path) -> Result<Self> {
    Ok(Self {
      head: runtime.source_head_position()?,
      file: SourceFingerprint::read(source_db_path)?,
    })
  }

  /// Fail (retryably) if the source changed since `self`; `what` completes
  /// "source primary ... snapshot bootstrap".
  fn check_quiet(
    &self,
    runtime: &ReplicaReplication,
    source_db_path: &Path,
    what: &str,
  ) -> Result<()> {
    let now = Self::read(runtime, source_db_path)?;
    if now.head == self.head && now.file == self.file {
      return Ok(());
    }
    Err(KiteError::InvalidReplication(format!(
      "source primary {what} snapshot bootstrap; start={}:{}, observed={}:{}, start_file=[{}], \
       observed_file=[{}]; quiesce writes and retry",
      self.head.0, self.head.1, now.head.0, now.head.1, self.file, now.file
    )))
  }
}

/// The writes of a snapshot bootstrap, committed in bounded transactions:
/// one transaction cannot outgrow the replica's WAL, and memory stays
/// bounded. Before the first commit the replica is marked incomplete
/// (`mark_bootstrap_incomplete`); the bootstrap's `mark_applied` clears it.
struct BootstrapBatch<'a> {
  replica: &'a SingleFileDB,
  runtime: &'a ReplicaReplication,
  tx: Option<SingleFileTxGuard<'a>>,
  writes: usize,
  max_wal_bytes: usize,
}

impl<'a> BootstrapBatch<'a> {
  fn new(replica: &'a SingleFileDB, runtime: &'a ReplicaReplication) -> Self {
    Self {
      replica,
      runtime,
      tx: None,
      writes: 0,
      max_wal_bytes: batch_wal_budget(replica, BOOTSTRAP_BATCH_WAL_SHARE),
    }
  }

  /// Run one write in the batch's transaction (begun on demand), and commit
  /// the batch once it is full.
  fn write<T>(&mut self, write: impl FnOnce(&SingleFileDB) -> Result<T>) -> Result<T> {
    if self.tx.is_none() {
      self.tx = Some(self.replica.begin_replication_apply()?);
    }
    let value = write(self.replica)?;
    self.writes = self.writes.saturating_add(1);
    if self.writes >= BOOTSTRAP_BATCH_MAX_WRITES || self.pending_wal_bytes() >= self.max_wal_bytes {
      self.commit()?;
    }
    Ok(value)
  }

  fn pending_wal_bytes(&self) -> usize {
    self
      .replica
      .current_tx_handle()
      .map_or(0, |tx| tx.lock().pending_wal.len())
  }

  /// Commit the open transaction, if any. The WAL must not fill across
  /// batches either: past half full (when no checkpoint is already running,
  /// as after the auto-checkpoint of the commit), the replica checkpoints,
  /// also with auto-checkpoint off, since the copy has to fit.
  fn commit(&mut self) -> Result<()> {
    let Some(tx) = self.tx.take() else {
      return Ok(());
    };
    self.writes = 0;
    if let Err(error) = self.runtime.mark_bootstrap_incomplete() {
      let _ = tx.rollback();
      return Err(error);
    }
    tx.commit()?;
    if !self.replica.is_checkpoint_running() && self.replica.should_checkpoint(0.5) {
      self.replica.checkpoint()?;
    }
    Ok(())
  }

  fn finish(mut self) -> Result<()> {
    self.commit()
  }
}

/// A transaction's WAL budget: `1/share` of the database's WAL, at least
/// `MIN_BATCH_WAL_BYTES`.
fn batch_wal_budget(db: &SingleFileDB, share: u64) -> usize {
  let capacity = db.wal_stats().capacity;
  usize::try_from((capacity / share.max(1)).max(MIN_BATCH_WAL_BYTES)).unwrap_or(usize::MAX)
}

/// Build the schema translation from names: every source name gets a local
/// id (its existing one, else the source's id when free here).
fn sync_schema_names(
  source: &SingleFileDB,
  epoch: u64,
  batch: &mut BootstrapBatch<'_>,
) -> Result<ReplicaSchemaMap> {
  let labels = sorted_schema_entries(&source.label_ids.read());
  let etypes = sorted_schema_entries(&source.etype_ids.read());
  let propkeys = sorted_schema_entries(&source.propkey_ids.read());
  let mut schema_map = ReplicaSchemaMap::for_epoch(epoch);
  for (id, name) in labels {
    let local_id = batch.write(|replica| replica.ensure_replica_label(&name, id))?;
    schema_map.insert(Label, id, local_id);
  }
  for (id, name) in etypes {
    let local_id = batch.write(|replica| replica.ensure_replica_etype(&name, id))?;
    schema_map.insert(EdgeType, id, local_id);
  }
  for (id, name) in propkeys {
    let local_id = batch.write(|replica| replica.ensure_replica_propkey(&name, id))?;
    schema_map.insert(PropertyKey, id, local_id);
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
/// source or holds a different key. All deletes commit before any create:
/// keys move between nodes, and an id can return with a new key (phase 2
/// recreates it as a fresh node).
fn remove_stale_nodes(source: &SingleFileDB, batch: &mut BootstrapBatch<'_>) -> Result<()> {
  let replica = batch.replica;
  let stale: Vec<NodeId> = replica
    .list_nodes()
    .into_iter()
    .filter(|&node_id| {
      !source.node_exists(node_id) || source.node_key(node_id) != replica.node_key(node_id)
    })
    .collect();
  for node_id in stale {
    batch.write(|replica| replica.delete_node(node_id))?;
  }
  batch.commit()
}

/// Bootstrap phase 2: create the source's nodes and copy schema, properties,
/// labels, vectors, and edges, translating the source's schema ids. Returns
/// the translation for the replica to persist.
fn sync_graph_state(
  source: &SingleFileDB,
  epoch: u64,
  batch: &mut BootstrapBatch<'_>,
) -> Result<ReplicaSchemaMap> {
  let replica = batch.replica;
  let mut schema_map = sync_schema_names(source, epoch, batch)?;

  // Phase 1 removed every node whose key differs, so existing ones match.
  let source_nodes = source.list_nodes();
  for &node_id in &source_nodes {
    if !replica.node_exists(node_id) {
      let key = source.node_key(node_id);
      batch.write(|replica| replica.create_node_with_id(node_id, key.as_deref()))?;
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
        batch.write(|replica| replica.set_node_prop(node_id, key_id, value.clone()))?;
      }
    }
    for &key_id in replica_props.keys() {
      if !source_props.contains_key(&key_id) {
        batch.write(|replica| replica.delete_node_prop(node_id, key_id))?;
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
        batch.write(|replica| replica.add_node_label(node_id, label_id))?;
      }
    }
    for &label_id in &replica_labels {
      if !source_labels.contains(&label_id) {
        batch.write(|replica| replica.remove_node_label(node_id, label_id))?;
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
            batch.write(|replica| {
              replica.set_node_vector(node_id, prop_key_id, source_value.as_ref())
            })?;
          }
        }
        (Some(source_value), None) => {
          batch.write(|replica| {
            replica.set_node_vector(node_id, prop_key_id, source_value.as_ref())
          })?;
        }
        (None, Some(_)) => {
          batch.write(|replica| replica.delete_node_vector(node_id, prop_key_id))?;
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
      batch.write(|replica| replica.add_edge(src, etype, dst))?;
    }
  }

  for edge in replica.list_edges(None) {
    if !source_edge_set.contains(&(edge.src, edge.etype, edge.dst)) {
      batch.write(|replica| replica.delete_edge(edge.src, edge.etype, edge.dst))?;
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
        batch.write(|replica| replica.set_edge_prop(src, etype, dst, key_id, value.clone()))?;
      }
    }
    for &key_id in replica_props.keys() {
      if !source_props.contains_key(&key_id) {
        batch.write(|replica| replica.delete_edge_prop(src, etype, dst, key_id))?;
      }
    }
  }

  Ok(schema_map)
}

/// Catch-up state across the transactions of one pull.
struct ApplyProgress {
  schema_map: ReplicaSchemaMap,
  /// The last applied frame.
  position: (u64, u64),
  applied: usize,
}

/// A frame that failed to apply, and why.
struct FrameApplyError {
  epoch: u64,
  log_index: u64,
  error: KiteError,
}

impl FrameApplyError {
  fn into_error(self) -> KiteError {
    KiteError::InvalidReplication(format!(
      "replica apply failed at {}:{}: {}",
      self.epoch, self.log_index, self.error
    ))
  }
}

/// Split contiguous frames into runs whose payloads fit `budget` bytes; a
/// run holds at least one frame.
fn frame_runs<'f, 'a>(
  frames: &'f [&'a ReplicationFrame],
  budget: usize,
) -> Vec<&'f [&'a ReplicationFrame]> {
  let mut runs = Vec::new();
  let mut start = 0usize;
  let mut bytes = 0usize;
  for (index, frame) in frames.iter().enumerate() {
    if index > start && bytes.saturating_add(frame.payload.len()) > budget {
      runs.push(&frames[start..index]);
      start = index;
      bytes = 0;
    }
    bytes = bytes.saturating_add(frame.payload.len());
  }
  if start < frames.len() {
    runs.push(&frames[start..]);
  }
  runs
}

/// Apply `frames` in one replica transaction. Their defines join the
/// translation, and the progress moves past them, only if it commits; on an
/// error nothing of them is applied.
fn apply_frames(
  db: &SingleFileDB,
  frames: &[&ReplicationFrame],
  progress: &mut ApplyProgress,
) -> std::result::Result<(), FrameApplyError> {
  let failed = |frame: &ReplicationFrame, error| FrameApplyError {
    epoch: frame.epoch,
    log_index: frame.log_index,
    error,
  };
  let (Some(first), Some(last)) = (frames.first(), frames.last()) else {
    return Ok(());
  };
  let decoded = frames
    .iter()
    .map(|frame| {
      decode_commit_frame_payload(&frame.payload)
        .and_then(|payload| parse_wal_records(&payload.wal_bytes))
        .map_err(|error| failed(frame, error))
    })
    .collect::<std::result::Result<Vec<_>, _>>()?;

  let mut schema_map = progress.schema_map.clone();
  if decoded.iter().any(|records| !records.is_empty()) {
    let tx_guard = db
      .begin_replication_apply()
      .map_err(|error| failed(first, error))?;
    for (frame, records) in frames.iter().zip(&decoded) {
      schema_map.enter_epoch(frame.epoch);
      for record in records {
        apply_wal_record_idempotent(db, record, &mut schema_map)
          .map_err(|error| failed(frame, error))?;
      }
    }
    tx_guard.commit().map_err(|error| failed(last, error))?;
  } else {
    for frame in frames {
      schema_map.enter_epoch(frame.epoch);
    }
  }

  progress.schema_map = schema_map;
  progress.position = (last.epoch, last.log_index);
  progress.applied = progress.applied.saturating_add(frames.len());
  Ok(())
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

/// raydb-b4 replication-core: fencing under the commit lock, the snapshot
/// copy, batched replica apply.
#[cfg(test)]
#[path = "b4_replication_tests.rs"]
mod b4_tests;
