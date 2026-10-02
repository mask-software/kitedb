//! Replica-side bootstrap/pull/apply orchestration support.

use super::durability::SidecarSync;
use super::log_store::{ReplicationFrame, SegmentLogStore};
use super::manifest::{ManifestStore, ReplicationManifest};
use super::primary::default_replication_sidecar_path;
use super::progress::upsert_replica_progress_synced;
use super::transport::decode_commit_frame_payload;
use super::types::ReplicationRole;
use crate::error::{KiteError, Result};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

const MANIFEST_FILE_NAME: &str = "manifest.json";
const CURSOR_FILE_NAME: &str = "replica-cursor.json";
const SCHEMA_MAP_FILE_NAME: &str = "replica-schema-map.json";
const SCHEMA_MAP_VERSION: u32 = 1;
const TRANSIENT_MISSING_RESEED_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone)]
pub struct ReplicaReplicationStatus {
  pub role: ReplicationRole,
  pub source_db_path: Option<PathBuf>,
  pub source_sidecar_path: Option<PathBuf>,
  pub applied_epoch: u64,
  pub applied_log_index: u64,
  pub last_error: Option<String>,
  pub needs_reseed: bool,
}

/// Replication head of the source sidecar as published on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePublishedHead {
  pub epoch: u64,
  /// Head in the manifest; frames up to it reach the segments first.
  pub manifest_head: u64,
  /// Newest frame in the segment files: log index and primary txid.
  pub newest_frame: Option<(u64, Option<u64>)>,
}

/// Schema namespace of a translated id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaIdKind {
  Label,
  EdgeType,
  PropertyKey,
}

impl SchemaIdKind {
  pub fn noun(self) -> &'static str {
    match self {
      SchemaIdKind::Label => "label",
      SchemaIdKind::EdgeType => "edge type",
      SchemaIdKind::PropertyKey => "property key",
    }
  }
}

/// Translation from the primary's schema ids to this replica's local ids.
///
/// Entries are trusted only within the source epoch they were learned in: a
/// promoted primary re-announces its whole schema, so a new epoch starts from
/// an empty translation. Persisted before the cursor that depends on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReplicaSchemaMap {
  version: u32,
  epoch: u64,
  labels: BTreeMap<u32, u32>,
  etypes: BTreeMap<u32, u32>,
  propkeys: BTreeMap<u32, u32>,
}

impl ReplicaSchemaMap {
  pub fn for_epoch(epoch: u64) -> Self {
    Self {
      version: SCHEMA_MAP_VERSION,
      epoch,
      ..Self::default()
    }
  }

  /// Enter the epoch of the next frame; a newer epoch drops every entry.
  pub fn enter_epoch(&mut self, epoch: u64) {
    if epoch > self.epoch {
      *self = Self::for_epoch(epoch);
    }
  }

  pub fn get(&self, kind: SchemaIdKind, primary_id: u32) -> Option<u32> {
    self.entries(kind).get(&primary_id).copied()
  }

  pub fn insert(&mut self, kind: SchemaIdKind, primary_id: u32, local_id: u32) {
    let entries = match kind {
      SchemaIdKind::Label => &mut self.labels,
      SchemaIdKind::EdgeType => &mut self.etypes,
      SchemaIdKind::PropertyKey => &mut self.propkeys,
    };
    entries.insert(primary_id, local_id);
  }

  fn entries(&self, kind: SchemaIdKind) -> &BTreeMap<u32, u32> {
    match kind {
      SchemaIdKind::Label => &self.labels,
      SchemaIdKind::EdgeType => &self.etypes,
      SchemaIdKind::PropertyKey => &self.propkeys,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct ReplicaCursorState {
  applied_epoch: u64,
  applied_log_index: u64,
  last_error: Option<String>,
  needs_reseed: bool,
  transient_missing_attempts: u32,
  transient_missing_epoch: u64,
  transient_missing_log_index: u64,
  /// A bootstrap removed stale nodes but has not installed the source state.
  bootstrap_incomplete: bool,
  /// Generation of the source sidecar the cursor indexes into (see
  /// `ReplicationManifest::generation`). `None` until the first apply; a
  /// cursor written before generations existed followed generation 0.
  source_generation: Option<u64>,
}

impl ReplicaCursorState {
  /// The source generation this cursor's position belongs to, if any.
  fn followed_generation(&self) -> Option<u64> {
    let pristine = self.applied_epoch == 0 && self.applied_log_index == 0;
    self
      .source_generation
      .or(if pristine { None } else { Some(0) })
  }
}

#[derive(Debug, Clone, Copy)]
struct SegmentScanHint {
  epoch: u64,
  segment_id: u64,
  next_offset: u64,
  next_log_index: u64,
}

#[derive(Debug)]
pub struct ReplicaReplication {
  local_sidecar_path: PathBuf,
  cursor_state_path: PathBuf,
  schema_map_path: PathBuf,
  replica_id: String,
  source_db_path: Option<PathBuf>,
  source_sidecar_path: Option<PathBuf>,
  state: Mutex<ReplicaCursorState>,
  schema_map: Mutex<ReplicaSchemaMap>,
  scan_hint: Mutex<Option<SegmentScanHint>>,
  /// Source sidecar generation of the frames or snapshot being applied, read
  /// with them; `mark_applied` records it with the cursor.
  applying_generation: Mutex<Option<u64>>,
  /// The replica database's sync policy, for the cursor, schema map and
  /// progress files.
  sync: SidecarSync,
}

impl ReplicaReplication {
  pub fn open(
    replica_db_path: &Path,
    local_sidecar_path: Option<PathBuf>,
    source_db_path: Option<PathBuf>,
    source_sidecar_path: Option<PathBuf>,
  ) -> Result<Self> {
    let local_sidecar_path =
      local_sidecar_path.unwrap_or_else(|| default_replication_sidecar_path(replica_db_path));
    std::fs::create_dir_all(&local_sidecar_path)?;
    let replica_id = normalize_path_for_compare(&local_sidecar_path)
      .to_string_lossy()
      .to_string();

    let cursor_state_path = local_sidecar_path.join(CURSOR_FILE_NAME);
    let state: ReplicaCursorState = load_json_state(&cursor_state_path, "replica cursor state")?;
    let schema_map_path = local_sidecar_path.join(SCHEMA_MAP_FILE_NAME);
    let schema_map: ReplicaSchemaMap = load_json_state(&schema_map_path, "replica schema map")?;
    if schema_map.version > SCHEMA_MAP_VERSION {
      return Err(KiteError::VersionMismatch {
        required: schema_map.version,
        current: SCHEMA_MAP_VERSION,
      });
    }

    let source_db_path = source_db_path.ok_or_else(|| {
      KiteError::InvalidReplication("replica source db path is not configured".to_string())
    })?;
    if !source_db_path.exists() {
      return Err(KiteError::InvalidReplication(format!(
        "replica source db path does not exist: {}",
        source_db_path.display()
      )));
    }
    if source_db_path.is_dir() {
      return Err(KiteError::InvalidReplication(format!(
        "replica source db path must be a file: {}",
        source_db_path.display()
      )));
    }
    if paths_equivalent(replica_db_path, &source_db_path) {
      return Err(KiteError::InvalidReplication(
        "replica source db path must differ from replica db path".to_string(),
      ));
    }

    let source_sidecar_path =
      source_sidecar_path.or_else(|| Some(default_replication_sidecar_path(&source_db_path)));
    if let Some(path) = source_sidecar_path.as_ref() {
      if path.exists() && !path.is_dir() {
        return Err(KiteError::InvalidReplication(format!(
          "replica source sidecar path must be a directory: {}",
          path.display()
        )));
      }
      if paths_equivalent(path, &local_sidecar_path) {
        return Err(KiteError::InvalidReplication(
          "replica source sidecar path must differ from local sidecar path".to_string(),
        ));
      }
    }

    Ok(Self {
      local_sidecar_path,
      cursor_state_path,
      schema_map_path,
      replica_id,
      source_db_path: Some(source_db_path),
      source_sidecar_path,
      state: Mutex::new(state),
      schema_map: Mutex::new(schema_map),
      scan_hint: Mutex::new(None),
      applying_generation: Mutex::new(None),
      sync: SidecarSync::default(),
    })
  }

  /// Sync the replica's state files with the replica database's policy.
  pub fn with_sync(mut self, sync: SidecarSync) -> Self {
    self.sync = sync;
    self
  }

  pub fn source_db_path(&self) -> Option<PathBuf> {
    self.source_db_path.clone()
  }

  pub fn source_sidecar_path(&self) -> Option<PathBuf> {
    self.source_sidecar_path.clone()
  }

  pub fn applied_position(&self) -> (u64, u64) {
    let state = self.state.lock();
    (state.applied_epoch, state.applied_log_index)
  }

  pub fn source_head_position(&self) -> Result<(u64, u64)> {
    let source_sidecar_path = self.source_sidecar_path.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("replica source sidecar path is not configured".to_string())
    })?;

    let manifest = ManifestStore::new(source_sidecar_path.join(MANIFEST_FILE_NAME)).read()?;
    Ok((manifest.epoch, manifest.head_log_index))
  }

  /// Manifest head plus the newest frame in the source segment files. A
  /// buffered (Normal/Off) primary writes frames before it persists the
  /// manifest, so the newest frame can be ahead of the manifest head.
  ///
  /// A snapshot bootstrap anchors its cursor here, so the source generation
  /// read here is the one its `mark_applied` records.
  pub fn source_published_head(&self) -> Result<SourcePublishedHead> {
    let source_sidecar_path = self.source_sidecar_path.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("replica source sidecar path is not configured".to_string())
    })?;

    let manifest = ManifestStore::new(source_sidecar_path.join(MANIFEST_FILE_NAME)).read()?;
    *self.applying_generation.lock() = Some(manifest.generation);
    let mut segment_ids: Vec<u64> = manifest.segments.iter().map(|segment| segment.id).collect();
    segment_ids.sort_unstable_by(|left, right| right.cmp(left));
    for segment_id in segment_ids {
      let segment_path = source_sidecar_path.join(segment_file_name(segment_id));
      if !segment_path.exists() {
        continue;
      }
      if let Some(frame) = SegmentLogStore::open(&segment_path)?.read_last_frame()? {
        let txid = decode_commit_frame_payload(&frame.payload)
          .ok()
          .map(|payload| payload.txid);
        return Ok(SourcePublishedHead {
          epoch: manifest.epoch,
          manifest_head: manifest.head_log_index,
          newest_frame: Some((frame.log_index, txid)),
        });
      }
    }

    Ok(SourcePublishedHead {
      epoch: manifest.epoch,
      manifest_head: manifest.head_log_index,
      newest_frame: None,
    })
  }

  pub fn schema_map(&self) -> ReplicaSchemaMap {
    self.schema_map.lock().clone()
  }

  /// Durably replace the schema translation. Callers store it before the
  /// cursor that depends on it, so a crash can only leave the translation
  /// ahead of the cursor; replaying defines is idempotent.
  pub fn store_schema_map(&self, schema_map: ReplicaSchemaMap) -> Result<()> {
    let mut current = self.schema_map.lock();
    if *current == schema_map {
      return Ok(());
    }
    persist_json_state(
      &self.schema_map_path,
      &schema_map,
      "replica schema map",
      self.sync,
    )?;
    *current = schema_map;
    Ok(())
  }

  /// Move the cursor to `epoch:log_index` in the source generation the
  /// applied frames or snapshot were read from. The cursor never moves back
  /// within one generation; a reseed from a recreated sidecar starts over in
  /// the new one.
  pub fn mark_applied(&self, epoch: u64, log_index: u64) -> Result<()> {
    let mut state = self.state.lock();
    let applying_generation = self.applying_generation.lock().take();
    let new_history =
      applying_generation.is_some_and(|generation| state.followed_generation() != Some(generation));

    if !new_history
      && (state.applied_epoch > epoch
        || (state.applied_epoch == epoch && state.applied_log_index > log_index))
    {
      return Err(KiteError::InvalidReplication(format!(
        "attempted to move replica cursor backwards: {}:{} -> {}:{}",
        state.applied_epoch, state.applied_log_index, epoch, log_index
      )));
    }

    let mut next_state = state.clone();
    next_state.applied_epoch = epoch;
    next_state.applied_log_index = log_index;
    if applying_generation.is_some() {
      next_state.source_generation = applying_generation;
    }
    next_state.last_error = None;
    next_state.needs_reseed = false;
    next_state.bootstrap_incomplete = false;
    clear_transient_missing_state(&mut next_state);
    persist_cursor_state(&self.cursor_state_path, &next_state, self.sync)?;
    *state = next_state;
    drop(state);
    self.report_source_progress(epoch, log_index)
  }

  /// Record, before a bootstrap commits its first change, that the replica
  /// state is partial until the bootstrap marks its cursor applied.
  pub fn mark_bootstrap_incomplete(&self) -> Result<()> {
    let mut state = self.state.lock();
    if state.bootstrap_incomplete {
      return Ok(());
    }
    let mut next_state = state.clone();
    next_state.bootstrap_incomplete = true;
    persist_cursor_state(&self.cursor_state_path, &next_state, self.sync)?;
    *state = next_state;
    Ok(())
  }

  pub fn mark_error(&self, message: impl Into<String>, needs_reseed: bool) -> Result<()> {
    let mut state = self.state.lock();
    let mut next_state = state.clone();
    next_state.last_error = Some(message.into());
    next_state.needs_reseed = needs_reseed;
    // Missing-frame attempts accumulate across pulls until the cursor moves or
    // the gap escalates; recording a transient error must not reset them.
    if needs_reseed {
      clear_transient_missing_state(&mut next_state);
    }
    persist_cursor_state(&self.cursor_state_path, &next_state, self.sync)?;
    *state = next_state;
    Ok(())
  }

  pub fn clear_error(&self) -> Result<()> {
    let mut state = self.state.lock();
    if state.last_error.is_none() && !state.needs_reseed && state.transient_missing_attempts == 0 {
      return Ok(());
    }
    let mut next_state = state.clone();
    next_state.last_error = None;
    next_state.needs_reseed = false;
    clear_transient_missing_state(&mut next_state);
    persist_cursor_state(&self.cursor_state_path, &next_state, self.sync)?;
    *state = next_state;
    Ok(())
  }

  pub fn status(&self) -> ReplicaReplicationStatus {
    let state = self.state.lock();
    ReplicaReplicationStatus {
      role: ReplicationRole::Replica,
      source_db_path: self.source_db_path.clone(),
      source_sidecar_path: self.source_sidecar_path.clone(),
      applied_epoch: state.applied_epoch,
      applied_log_index: state.applied_log_index,
      last_error: state.last_error.clone(),
      needs_reseed: state.needs_reseed,
    }
  }

  pub fn frames_after(
    &self,
    max_frames: usize,
    include_last_applied: bool,
  ) -> Result<Vec<ReplicationFrame>> {
    let source_sidecar_path = self.source_sidecar_path.as_ref().ok_or_else(|| {
      KiteError::InvalidReplication("replica source sidecar path is not configured".to_string())
    })?;

    if self.state.lock().bootstrap_incomplete {
      let message =
        "replica needs reseed: a snapshot bootstrap was interrupted after removing stale nodes"
          .to_string();
      self.mark_error(message.clone(), true)?;
      return Err(KiteError::InvalidReplication(message));
    }

    let (applied_epoch, applied_log_index) = self.applied_position();
    let manifest = ManifestStore::new(source_sidecar_path.join(MANIFEST_FILE_NAME)).read()?;
    let followed_generation = self.state.lock().followed_generation();
    if followed_generation.is_some_and(|generation| generation != manifest.generation) {
      let message = format!(
        "replica needs reseed: the source replication sidecar was recreated (log generation \
         {:016x}, the cursor {applied_epoch}:{applied_log_index} belongs to {:016x})",
        manifest.generation,
        followed_generation.unwrap_or_default()
      );
      self.mark_error(message.clone(), true)?;
      return Err(KiteError::InvalidReplication(message));
    }
    *self.applying_generation.lock() = Some(manifest.generation);
    let expected_next_log = applied_log_index.saturating_add(1);
    if expected_next_log < manifest.retained_floor {
      let message = format!(
        "replica needs reseed: applied log {} is below retained floor {}",
        applied_log_index, manifest.retained_floor
      );
      self.mark_error(message.clone(), true)?;
      return Err(KiteError::InvalidReplication(message));
    }

    let mut scan_hint = self.scan_hint.lock();
    let mut filtered = read_frames_after(
      source_sidecar_path,
      &manifest,
      applied_epoch,
      applied_log_index,
      include_last_applied,
      max_frames,
      &mut scan_hint,
    )?;

    if let Some(first) = filtered.first() {
      // Log indexes run on across epochs (a promotion keeps the head), so a
      // newer epoch that does not continue the cursor's log is a different
      // history.
      if first.epoch > applied_epoch && first.log_index < expected_next_log {
        let message = format!(
          "replica needs reseed: source frame {}:{} does not continue the replica's log at \
           {applied_epoch}:{applied_log_index}",
          first.epoch, first.log_index
        );
        self.mark_error(message.clone(), true)?;
        return Err(KiteError::InvalidReplication(message));
      }
      if first.log_index > expected_next_log {
        let detail = format!(
          "missing log range {}..{}",
          expected_next_log,
          first.log_index.saturating_sub(1)
        );
        return self.transient_gap_error(applied_epoch, expected_next_log, detail);
      }
    }

    // Apply only a contiguous run; the next pull starts at a break and
    // reports it.
    if let Some(last_contiguous) = filtered
      .windows(2)
      .position(|pair| pair[1].log_index != pair[0].log_index.saturating_add(1))
    {
      filtered.truncate(last_contiguous + 1);
    }

    if filtered.is_empty() && manifest.head_log_index > applied_log_index {
      let detail = format!(
        "applied log {} but primary head is {} and required frames are unavailable",
        applied_log_index, manifest.head_log_index
      );
      return self.transient_gap_error(applied_epoch, expected_next_log, detail);
    }

    Ok(filtered)
  }

  pub fn local_sidecar_path(&self) -> &Path {
    &self.local_sidecar_path
  }

  fn report_source_progress(&self, epoch: u64, log_index: u64) -> Result<()> {
    if let Some(source_sidecar_path) = self.source_sidecar_path.as_ref() {
      upsert_replica_progress_synced(
        source_sidecar_path,
        &self.replica_id,
        epoch,
        log_index,
        self.sync,
      )?;
    }
    Ok(())
  }

  fn transient_gap_error(
    &self,
    applied_epoch: u64,
    expected_next_log: u64,
    detail: String,
  ) -> Result<Vec<ReplicationFrame>> {
    let mut state = self.state.lock();
    let mut next_state = state.clone();
    if next_state.transient_missing_epoch != applied_epoch
      || next_state.transient_missing_log_index != expected_next_log
    {
      next_state.transient_missing_attempts = 0;
      next_state.transient_missing_epoch = applied_epoch;
      next_state.transient_missing_log_index = expected_next_log;
    }
    next_state.transient_missing_attempts = next_state.transient_missing_attempts.saturating_add(1);
    let attempts = next_state.transient_missing_attempts;
    let needs_reseed = attempts >= TRANSIENT_MISSING_RESEED_ATTEMPTS;
    let error_message = if needs_reseed {
      format!("replica needs reseed: {detail}")
    } else {
      format!(
        "replica missing frames after {applied_epoch}:{expected_next_log} ({detail}); transient retry {attempts}/{TRANSIENT_MISSING_RESEED_ATTEMPTS}"
      )
    };
    next_state.last_error = Some(error_message.clone());
    next_state.needs_reseed = needs_reseed;
    if needs_reseed {
      clear_transient_missing_state(&mut next_state);
    }
    persist_cursor_state(&self.cursor_state_path, &next_state, self.sync)?;
    *state = next_state;
    Err(KiteError::InvalidReplication(error_message))
  }
}

fn load_json_state<T: DeserializeOwned + Default>(path: &Path, what: &str) -> Result<T> {
  if !path.exists() {
    return Ok(T::default());
  }

  let bytes = std::fs::read(path)?;
  serde_json::from_slice(&bytes)
    .map_err(|error| KiteError::Serialization(format!("decode {what} failed: {error}")))
}

fn persist_cursor_state(path: &Path, state: &ReplicaCursorState, sync: SidecarSync) -> Result<()> {
  persist_json_state(path, state, "replica cursor state", sync)
}

/// Atomic replace: write a temp file, sync it, rename, sync the directory.
fn persist_json_state<T: Serialize>(
  path: &Path,
  state: &T,
  what: &str,
  sync: SidecarSync,
) -> Result<()> {
  let tmp_path = path.with_extension("json.tmp");
  let bytes = serde_json::to_vec(state)
    .map_err(|error| KiteError::Serialization(format!("encode {what} failed: {error}")))?;

  let mut file = OpenOptions::new()
    .create(true)
    .truncate(true)
    .write(true)
    .open(&tmp_path)?;
  file.write_all(&bytes)?;
  sync.sync_file(&file)?;
  std::fs::rename(&tmp_path, path)?;
  sync.sync_parent_dir(path)?;
  Ok(())
}

fn clear_transient_missing_state(state: &mut ReplicaCursorState) {
  state.transient_missing_attempts = 0;
  state.transient_missing_epoch = 0;
  state.transient_missing_log_index = 0;
}

fn read_frames_after(
  sidecar_path: &Path,
  manifest: &ReplicationManifest,
  applied_epoch: u64,
  applied_log_index: u64,
  include_last_applied: bool,
  max_frames: usize,
  scan_hint: &mut Option<SegmentScanHint>,
) -> Result<Vec<ReplicationFrame>> {
  let minimum_log_index = if include_last_applied && applied_log_index > 0 {
    applied_log_index
  } else {
    applied_log_index.saturating_add(1)
  };

  let mut segments = manifest.segments.clone();
  segments.sort_by_key(|segment| segment.id);

  let mut frames = Vec::new();
  for segment in segments {
    // A buffered primary flushes frames into its active segment before it
    // persists the manifest, so the active segment's end index can be stale.
    let sealed = segment.id != manifest.active_segment_id;
    if sealed && segment.end_log_index > 0 && segment.end_log_index < minimum_log_index {
      continue;
    }

    let segment_path = sidecar_path.join(segment_file_name(segment.id));
    if !segment_path.exists() {
      continue;
    }

    let remaining = if max_frames > 0 {
      max_frames.saturating_sub(frames.len())
    } else {
      usize::MAX
    };
    if remaining == 0 {
      break;
    }

    let start_offset = scan_hint
      .as_ref()
      .filter(|hint| {
        hint.epoch == manifest.epoch
          && hint.segment_id == segment.id
          && hint.next_log_index <= minimum_log_index
      })
      .map(|hint| hint.next_offset)
      .unwrap_or(0);

    let (segment_frames, next_offset, last_seen) = SegmentLogStore::open(&segment_path)?
      .read_filtered_from_offset(
        start_offset,
        |frame| {
          frame_is_after_applied(
            frame,
            applied_epoch,
            applied_log_index,
            include_last_applied,
          )
        },
        remaining,
      )?;

    if let Some((last_epoch, last_log_index)) = last_seen {
      *scan_hint = Some(SegmentScanHint {
        epoch: last_epoch,
        segment_id: segment.id,
        next_offset,
        next_log_index: last_log_index.saturating_add(1),
      });
    }
    frames.extend(segment_frames);

    if max_frames > 0 && frames.len() >= max_frames {
      break;
    }
  }

  if frames.len() > 1 {
    frames.sort_by(|left, right| {
      left
        .epoch
        .cmp(&right.epoch)
        .then_with(|| left.log_index.cmp(&right.log_index))
    });
  }

  if max_frames > 0 && frames.len() > max_frames {
    frames.truncate(max_frames);
  }

  Ok(frames)
}

fn frame_is_after_applied(
  frame: &ReplicationFrame,
  applied_epoch: u64,
  applied_log_index: u64,
  include_last_applied: bool,
) -> bool {
  if frame.epoch > applied_epoch {
    return true;
  }
  if frame.epoch < applied_epoch {
    return false;
  }

  if include_last_applied && applied_log_index > 0 {
    frame.log_index >= applied_log_index
  } else {
    frame.log_index > applied_log_index
  }
}

fn segment_file_name(id: u64) -> String {
  format!("segment-{id:020}.rlog")
}

fn normalize_path_for_compare(path: &Path) -> PathBuf {
  let absolute = if path.is_absolute() {
    path.to_path_buf()
  } else {
    match std::env::current_dir() {
      Ok(cwd) => cwd.join(path),
      Err(_) => path.to_path_buf(),
    }
  };
  std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
  normalize_path_for_compare(left) == normalize_path_for_compare(right)
}

#[cfg(test)]
mod tests {
  use super::ReplicaSchemaMap;

  #[test]
  fn schema_map_trusts_entries_only_within_their_epoch() {
    use super::SchemaIdKind::{Label, PropertyKey};

    let mut map = ReplicaSchemaMap::for_epoch(1);
    map.insert(PropertyKey, 1, 7);
    map.insert(Label, 2, 3);

    map.enter_epoch(1);
    assert_eq!(map.get(PropertyKey, 1), Some(7), "same epoch keeps entries");

    map.enter_epoch(2);
    assert_eq!(map.get(PropertyKey, 1), None, "a newer epoch drops entries");
    assert_eq!(map.get(Label, 2), None);

    map.insert(PropertyKey, 1, 9);
    map.enter_epoch(1);
    assert_eq!(
      map.get(PropertyKey, 1),
      Some(9),
      "replaying an older frame keeps entries"
    );
  }
}
