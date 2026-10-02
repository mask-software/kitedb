//! Transport payloads for pull/push replication.
//!
//! A primary exports its replication state for replicas that cannot read its
//! files (HTTP transports): a snapshot (a consistent copy of the database
//! file, with the log position it holds) and pages of log frames. Each comes
//! as a struct with raw bytes ([`SnapshotTransport`], [`LogTransportPage`])
//! and as JSON, a thin serializer over it with the bytes in base64.

use super::types::ReplicationCursor;
use crate::error::{KiteError, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use byteorder::{LittleEndian, ReadBytesExt};
use serde::Serialize;
use std::io::{Cursor, Read};
use std::str::FromStr;

/// `SnapshotTransport::format`: the data is a copy of the database file.
pub const SNAPSHOT_TRANSPORT_FORMAT: &str = "single-file-db-copy";

/// A snapshot of a primary for a replica to start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTransport {
  /// [`SNAPSHOT_TRANSPORT_FORMAT`].
  pub format: &'static str,
  /// Size of the database file copy.
  pub byte_length: u64,
  /// CRC-32 (IEEE) of the copy.
  pub checksum_crc32: u32,
  pub generated_at_ms: u64,
  pub epoch: u64,
  /// The copy holds every commit up to this log index, and none after it.
  pub head_log_index: u64,
  pub retained_floor: u64,
  /// Generation of the sidecar log history `start_cursor` belongs to (see
  /// `ReplicationManifest::generation`).
  pub generation: u64,
  /// Where a replica pulls the log from: right after the head frame.
  pub start_cursor: ReplicationCursor,
  /// The copy, when requested.
  pub data: Option<Vec<u8>>,
}

impl SnapshotTransport {
  /// The JSON transport: these fields, `checksum_crc32c` (the CRC-32 in 8 hex
  /// digits; the name is kept for clients), `generation` in 16 hex digits
  /// (exact in JSON), and the data in base64. The data is dropped once
  /// encoded, so at most the base64 and the JSON text are held besides it.
  pub fn into_json(self) -> Result<String> {
    #[derive(Serialize)]
    struct SnapshotJson<'a> {
      format: &'a str,
      byte_length: u64,
      checksum_crc32c: String,
      generated_at_ms: u64,
      epoch: u64,
      head_log_index: u64,
      retained_floor: u64,
      generation: String,
      start_cursor: String,
      data_base64: Option<String>,
    }

    let data_base64 = self.data.map(|data| BASE64_STANDARD.encode(data));
    serde_json::to_string(&SnapshotJson {
      format: self.format,
      byte_length: self.byte_length,
      checksum_crc32c: format!("{:08x}", self.checksum_crc32),
      generated_at_ms: self.generated_at_ms,
      epoch: self.epoch,
      head_log_index: self.head_log_index,
      retained_floor: self.retained_floor,
      generation: format_generation(self.generation),
      start_cursor: self.start_cursor.to_string(),
      data_base64,
    })
    .map_err(|error| {
      KiteError::Serialization(format!("encode replication snapshot export: {error}"))
    })
  }
}

/// One replication frame in a log page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTransportFrame {
  pub epoch: u64,
  pub log_index: u64,
  pub segment_id: u64,
  pub segment_offset: u64,
  /// Size of the frame in its segment, header included.
  pub bytes: u64,
  /// The frame payload (a commit's WAL records), when requested.
  pub payload: Option<Vec<u8>>,
}

/// A page of replication frames after a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTransportPage {
  pub epoch: u64,
  pub head_log_index: u64,
  pub retained_floor: u64,
  /// Generation of the sidecar log history the frames and cursors belong
  /// to. A replica that sees it change must reseed: the sidecar was
  /// recreated and its log restarted.
  pub generation: u64,
  /// The cursor the page starts after.
  pub cursor: Option<ReplicationCursor>,
  /// The position after the page's last frame, if it has one.
  pub next_cursor: Option<ReplicationCursor>,
  /// No frame after the page was left out for a limit.
  pub eof: bool,
  /// Sum of the frames' `bytes`.
  pub total_bytes: u64,
  pub frames: Vec<LogTransportFrame>,
}

impl LogTransportPage {
  /// The JSON transport: these fields, `frame_count`, `generation` in 16 hex
  /// digits, and each frame's payload in base64 (`payload_base64`).
  pub fn into_json(self) -> Result<String> {
    #[derive(Serialize)]
    struct FrameJson {
      epoch: u64,
      log_index: u64,
      segment_id: u64,
      segment_offset: u64,
      bytes: u64,
      payload_base64: Option<String>,
    }

    #[derive(Serialize)]
    struct PageJson {
      epoch: u64,
      head_log_index: u64,
      retained_floor: u64,
      generation: String,
      cursor: Option<String>,
      next_cursor: Option<String>,
      eof: bool,
      frame_count: usize,
      total_bytes: u64,
      frames: Vec<FrameJson>,
    }

    let frame_count = self.frames.len();
    let frames = self
      .frames
      .into_iter()
      .map(|frame| FrameJson {
        epoch: frame.epoch,
        log_index: frame.log_index,
        segment_id: frame.segment_id,
        segment_offset: frame.segment_offset,
        bytes: frame.bytes,
        payload_base64: frame.payload.map(|payload| BASE64_STANDARD.encode(payload)),
      })
      .collect();
    serde_json::to_string(&PageJson {
      epoch: self.epoch,
      head_log_index: self.head_log_index,
      retained_floor: self.retained_floor,
      generation: format_generation(self.generation),
      cursor: self.cursor.map(|cursor| cursor.to_string()),
      next_cursor: self.next_cursor.map(|cursor| cursor.to_string()),
      eof: self.eof,
      frame_count,
      total_bytes: self.total_bytes,
      frames,
    })
    .map_err(|error| KiteError::Serialization(format!("encode replication log export: {error}")))
  }
}

/// A sidecar generation as transports carry it: 16 hex digits, so a JSON
/// client reads it exactly (a u64 does not fit a JavaScript number).
pub fn format_generation(generation: u64) -> String {
  format!("{generation:016x}")
}

/// Parse a transport cursor (`epoch:segment_id:segment_offset:log_index`);
/// none, or an empty one, starts at the beginning of the log.
pub fn parse_transport_cursor(raw: Option<&str>) -> Result<Option<ReplicationCursor>> {
  match raw {
    Some(raw) if !raw.trim().is_empty() => ReplicationCursor::from_str(raw.trim())
      .map(Some)
      .map_err(|error| KiteError::InvalidReplication(format!("invalid cursor: {error}"))),
    _ => Ok(None),
  }
}

const COMMIT_PAYLOAD_MAGIC: &[u8; 4] = b"RPL1";
const COMMIT_PAYLOAD_HEADER_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFramePayload {
  pub txid: u64,
  pub wal_bytes: Vec<u8>,
}

pub fn build_commit_payload_header(
  txid: u64,
  wal_len: usize,
) -> Result<[u8; COMMIT_PAYLOAD_HEADER_BYTES]> {
  let wal_len = u32::try_from(wal_len).map_err(|_| {
    KiteError::InvalidReplication(format!("replication commit payload too large: {wal_len}"))
  })?;

  let mut bytes = [0u8; COMMIT_PAYLOAD_HEADER_BYTES];
  bytes[..4].copy_from_slice(COMMIT_PAYLOAD_MAGIC);
  bytes[4..12].copy_from_slice(&txid.to_le_bytes());
  bytes[12..16].copy_from_slice(&wal_len.to_le_bytes());
  Ok(bytes)
}

pub fn encode_commit_frame_payload(txid: u64, wal_bytes: &[u8]) -> Result<Vec<u8>> {
  let header = build_commit_payload_header(txid, wal_bytes.len())?;
  let mut bytes = Vec::with_capacity(COMMIT_PAYLOAD_HEADER_BYTES + wal_bytes.len());
  bytes.extend_from_slice(&header);
  bytes.extend_from_slice(wal_bytes);
  Ok(bytes)
}

pub fn decode_commit_frame_payload(payload: &[u8]) -> Result<CommitFramePayload> {
  if payload.len() < COMMIT_PAYLOAD_HEADER_BYTES {
    return Err(KiteError::InvalidReplication(
      "replication commit payload too short".to_string(),
    ));
  }

  if &payload[..4] != COMMIT_PAYLOAD_MAGIC {
    return Err(KiteError::InvalidReplication(
      "replication commit payload has invalid magic".to_string(),
    ));
  }

  let mut cursor = Cursor::new(&payload[4..]);
  let txid = cursor.read_u64::<LittleEndian>()?;
  let wal_len = cursor.read_u32::<LittleEndian>()? as usize;

  let mut wal_bytes = vec![0; wal_len];
  cursor
    .read_exact(&mut wal_bytes)
    .map_err(|_| KiteError::InvalidReplication("replication payload truncated".to_string()))?;

  if cursor.position() as usize != payload.len() - 4 {
    return Err(KiteError::InvalidReplication(
      "replication payload contains unexpected trailing bytes".to_string(),
    ));
  }

  Ok(CommitFramePayload { txid, wal_bytes })
}

#[cfg(test)]
mod tests {
  use super::{decode_commit_frame_payload, encode_commit_frame_payload};

  #[test]
  fn roundtrip_commit_payload() {
    let bytes = encode_commit_frame_payload(77, b"abc").expect("encode");
    let decoded = decode_commit_frame_payload(&bytes).expect("decode");
    assert_eq!(decoded.txid, 77);
    assert_eq!(decoded.wal_bytes, b"abc");
  }

  #[test]
  fn rejects_bad_magic() {
    let mut bytes = encode_commit_frame_payload(1, b"x").expect("encode");
    bytes[0] = b'X';
    assert!(decode_commit_frame_payload(&bytes).is_err());
  }
}
