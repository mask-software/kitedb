//! Transport payloads for pull/push replication.

use crate::error::{KiteError, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Read};

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
