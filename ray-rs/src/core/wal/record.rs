//! WAL record types and serialization
//!
//! Ported from src/core/wal.ts

use crate::constants::*;
use crate::types::*;
use crate::util::binary::*;
use crate::util::crc::crc32c;

// ============================================================================
// WAL Record
// ============================================================================

/// WAL record for writing
#[derive(Debug, Clone)]
pub struct WalRecord {
  pub record_type: WalRecordType,
  pub txid: TxId,
  pub payload: Vec<u8>,
}

impl WalRecord {
  /// Create a new WAL record
  pub fn new(record_type: WalRecordType, txid: TxId, payload: Vec<u8>) -> Self {
    Self {
      record_type,
      txid,
      payload,
    }
  }

  /// Estimate the size of this record when serialized
  pub fn estimated_size(&self) -> usize {
    let header_size = WAL_RECORD_HEADER_SIZE;
    let crc_size = 4;
    let unpadded = header_size + self.payload.len() + crc_size;
    align_up(unpadded, WAL_RECORD_ALIGNMENT)
  }

  /// Build the WAL record bytes
  pub fn build(&self) -> Vec<u8> {
    let header_size = WAL_RECORD_HEADER_SIZE;
    let crc_size = 4;
    let unpadded = header_size + self.payload.len() + crc_size;
    let pad_len = padding_for(unpadded, WAL_RECORD_ALIGNMENT);
    let total_size = unpadded + pad_len;

    let mut buffer = vec![0u8; total_size];

    // Write header
    write_u32(&mut buffer, 0, unpadded as u32); // recLen
    buffer[4] = self.record_type as u8;
    buffer[5] = 0; // flags
    write_u16(&mut buffer, 6, 0); // reserved
    write_u64(&mut buffer, 8, self.txid);
    write_u32(&mut buffer, 16, self.payload.len() as u32);

    // Write payload
    buffer[WAL_RECORD_HEADER_SIZE..WAL_RECORD_HEADER_SIZE + self.payload.len()]
      .copy_from_slice(&self.payload);

    // Compute CRC (over type + flags + reserved + txid + payloadLen + payload)
    let crc_start = 4; // After recLen
    let crc_end = WAL_RECORD_HEADER_SIZE + self.payload.len();
    let crc_value = crc32c(&buffer[crc_start..crc_end]);
    write_u32(&mut buffer, crc_end, crc_value);

    buffer
  }
}

// ============================================================================
// Parsed WAL Record
// ============================================================================

/// Parsed WAL record from reading
#[derive(Debug, Clone)]
pub struct ParsedWalRecord {
  pub record_type: WalRecordType,
  pub flags: u8,
  pub txid: TxId,
  pub payload: Vec<u8>,
  pub record_end: usize, // Offset after this record (including padding)
}

/// Parse a single WAL record from buffer at given offset
/// Returns None if record is invalid or truncated
pub fn parse_wal_record(buffer: &[u8], offset: usize) -> Option<ParsedWalRecord> {
  if offset + 4 > buffer.len() {
    return None;
  }

  // Read record length
  let rec_len = read_u32(buffer, offset) as usize;
  if rec_len < WAL_RECORD_HEADER_SIZE + 4 {
    return None; // Too small
  }

  // Check if full record is available
  let pad_len = padding_for(rec_len, WAL_RECORD_ALIGNMENT);
  let total_len = rec_len + pad_len;

  if offset + total_len > buffer.len() {
    return None; // Truncated
  }

  // Read header fields
  let record_type_byte = buffer[offset + 4];
  let flags = buffer[offset + 5];
  let txid = read_u64(buffer, offset + 8);
  let payload_len = read_u32(buffer, offset + 16) as usize;

  // Validate payload length
  if WAL_RECORD_HEADER_SIZE + payload_len + 4 != rec_len {
    return None;
  }

  // Extract payload
  let payload_start = offset + WAL_RECORD_HEADER_SIZE;
  let payload = buffer[payload_start..payload_start + payload_len].to_vec();

  // Verify CRC
  let crc_start = offset + 4;
  let crc_end = offset + WAL_RECORD_HEADER_SIZE + payload_len;
  let stored_crc = read_u32(buffer, crc_end);
  let computed_crc = crc32c(&buffer[crc_start..crc_end]);

  if stored_crc != computed_crc {
    return None; // CRC mismatch
  }

  let record_type = WalRecordType::from_u8(record_type_byte)?;

  Some(ParsedWalRecord {
    record_type,
    flags,
    txid,
    payload,
    record_end: offset + total_len,
  })
}

/// Scan WAL buffer and return all valid records
pub fn scan_wal(buffer: &[u8]) -> Vec<ParsedWalRecord> {
  let mut records = Vec::new();
  let mut offset = WAL_HEADER_SIZE;

  while offset < buffer.len() {
    match parse_wal_record(buffer, offset) {
      Some(record) => {
        offset = record.record_end;
        records.push(record);
      }
      None => break, // Invalid or truncated record
    }
  }

  records
}

/// Extract committed transactions from WAL records
/// Returns records grouped by committed transaction
pub fn extract_committed_transactions(
  records: &[ParsedWalRecord],
) -> std::collections::HashMap<TxId, Vec<&ParsedWalRecord>> {
  use std::collections::HashMap;

  let mut pending: HashMap<TxId, Vec<&ParsedWalRecord>> = HashMap::new();
  let mut committed: HashMap<TxId, Vec<&ParsedWalRecord>> = HashMap::new();

  for record in records {
    let txid = record.txid;

    match record.record_type {
      WalRecordType::Begin => {
        pending.insert(txid, Vec::new());
      }
      WalRecordType::Commit => {
        if let Some(tx_records) = pending.remove(&txid) {
          committed.insert(txid, tx_records);
        }
      }
      WalRecordType::Rollback => {
        pending.remove(&txid);
      }
      _ => {
        // Data record - add to pending transaction
        if let Some(tx_pending) = pending.get_mut(&txid) {
          tx_pending.push(record);
        }
      }
    }
  }

  committed
}

// ============================================================================
// Payload Builders
// ============================================================================

/// Build BEGIN payload (empty)
pub fn build_begin_payload() -> Vec<u8> {
  Vec::new()
}

/// Build COMMIT payload (empty)
pub fn build_commit_payload() -> Vec<u8> {
  Vec::new()
}

/// Build ROLLBACK payload (empty)
pub fn build_rollback_payload() -> Vec<u8> {
  Vec::new()
}

/// Build CREATE_NODE payload
pub fn build_create_node_payload(node_id: NodeId, key: Option<&str>) -> Vec<u8> {
  let key_bytes = key.map(|k| k.as_bytes()).unwrap_or(&[]);
  let mut buffer = vec![0u8; 8 + 4 + key_bytes.len()];

  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, key_bytes.len() as u32);
  buffer[12..12 + key_bytes.len()].copy_from_slice(key_bytes);

  buffer
}

/// Build DELETE_NODE payload
pub fn build_delete_node_payload(node_id: NodeId) -> Vec<u8> {
  let mut buffer = vec![0u8; 8];
  write_u64(&mut buffer, 0, node_id);
  buffer
}

/// Build ADD_EDGE payload
pub fn build_add_edge_payload(src: NodeId, etype: ETypeId, dst: NodeId) -> Vec<u8> {
  let mut buffer = vec![0u8; 8 + 4 + 8];
  write_u64(&mut buffer, 0, src);
  write_u32(&mut buffer, 8, etype);
  write_u64(&mut buffer, 12, dst);
  buffer
}

/// Build DELETE_EDGE payload
pub fn build_delete_edge_payload(src: NodeId, etype: ETypeId, dst: NodeId) -> Vec<u8> {
  build_add_edge_payload(src, etype, dst)
}

/// Build DEFINE_LABEL payload
pub fn build_define_label_payload(label_id: LabelId, name: &str) -> Vec<u8> {
  let name_bytes = name.as_bytes();
  let mut buffer = vec![0u8; 4 + 4 + name_bytes.len()];
  write_u32(&mut buffer, 0, label_id);
  write_u32(&mut buffer, 4, name_bytes.len() as u32);
  buffer[8..8 + name_bytes.len()].copy_from_slice(name_bytes);
  buffer
}

/// Build DEFINE_ETYPE payload
pub fn build_define_etype_payload(etype_id: ETypeId, name: &str) -> Vec<u8> {
  build_define_label_payload(etype_id, name)
}

/// Build DEFINE_PROPKEY payload
pub fn build_define_propkey_payload(propkey_id: PropKeyId, name: &str) -> Vec<u8> {
  build_define_label_payload(propkey_id, name)
}

/// Serialize a property value
fn serialize_prop_value(value: &PropValue) -> Vec<u8> {
  match value {
    PropValue::Null => vec![0],
    PropValue::Bool(v) => vec![1, if *v { 1 } else { 0 }],
    PropValue::I64(v) => {
      let mut buf = vec![0u8; 9];
      buf[0] = 2;
      write_i64(&mut buf, 1, *v);
      buf
    }
    PropValue::F64(v) => {
      let mut buf = vec![0u8; 9];
      buf[0] = 3;
      write_f64(&mut buf, 1, *v);
      buf
    }
    PropValue::String(s) => {
      let str_bytes = s.as_bytes();
      let mut buf = vec![0u8; 1 + 4 + str_bytes.len()];
      buf[0] = 4;
      write_u32(&mut buf, 1, str_bytes.len() as u32);
      buf[5..5 + str_bytes.len()].copy_from_slice(str_bytes);
      buf
    }
    PropValue::VectorF32(v) => {
      let mut buf = vec![0u8; 1 + 4 + v.len() * 4];
      buf[0] = 5;
      write_u32(&mut buf, 1, v.len() as u32);
      for (i, val) in v.iter().enumerate() {
        let bytes = val.to_le_bytes();
        buf[5 + i * 4..5 + i * 4 + 4].copy_from_slice(&bytes);
      }
      buf
    }
  }
}

/// Build SET_NODE_PROP payload
pub fn build_set_node_prop_payload(
  node_id: NodeId,
  key_id: PropKeyId,
  value: &PropValue,
) -> Vec<u8> {
  let value_bytes = serialize_prop_value(value);
  let mut buffer = vec![0u8; 8 + 4 + value_bytes.len()];
  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, key_id);
  buffer[12..].copy_from_slice(&value_bytes);
  buffer
}

/// Build DEL_NODE_PROP payload
pub fn build_del_node_prop_payload(node_id: NodeId, key_id: PropKeyId) -> Vec<u8> {
  let mut buffer = vec![0u8; 8 + 4];
  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, key_id);
  buffer
}

/// Build SET_EDGE_PROP payload
pub fn build_set_edge_prop_payload(
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
  value: &PropValue,
) -> Vec<u8> {
  let value_bytes = serialize_prop_value(value);
  let mut buffer = vec![0u8; 8 + 4 + 8 + 4 + value_bytes.len()];
  write_u64(&mut buffer, 0, src);
  write_u32(&mut buffer, 8, etype);
  write_u64(&mut buffer, 12, dst);
  write_u32(&mut buffer, 20, key_id);
  buffer[24..].copy_from_slice(&value_bytes);
  buffer
}

/// Build DEL_EDGE_PROP payload
pub fn build_del_edge_prop_payload(
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
  key_id: PropKeyId,
) -> Vec<u8> {
  let mut buffer = vec![0u8; 8 + 4 + 8 + 4];
  write_u64(&mut buffer, 0, src);
  write_u32(&mut buffer, 8, etype);
  write_u64(&mut buffer, 12, dst);
  write_u32(&mut buffer, 20, key_id);
  buffer
}

/// Build SET_NODE_VECTOR payload
pub fn build_set_node_vector_payload(
  node_id: NodeId,
  prop_key_id: PropKeyId,
  vector: &[f32],
) -> Vec<u8> {
  let dimensions = vector.len();
  let mut buffer = vec![0u8; 8 + 4 + 4 + dimensions * 4];
  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, prop_key_id);
  write_u32(&mut buffer, 12, dimensions as u32);
  for (i, val) in vector.iter().enumerate() {
    let bytes = val.to_le_bytes();
    buffer[16 + i * 4..16 + i * 4 + 4].copy_from_slice(&bytes);
  }
  buffer
}

/// Build DEL_NODE_VECTOR payload
pub fn build_del_node_vector_payload(node_id: NodeId, prop_key_id: PropKeyId) -> Vec<u8> {
  let mut buffer = vec![0u8; 8 + 4];
  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, prop_key_id);
  buffer
}

/// Build ADD_NODE_LABEL payload
pub fn build_add_node_label_payload(node_id: NodeId, label_id: LabelId) -> Vec<u8> {
  let mut buffer = vec![0u8; 8 + 4];
  write_u64(&mut buffer, 0, node_id);
  write_u32(&mut buffer, 8, label_id);
  buffer
}

/// Build REMOVE_NODE_LABEL payload
pub fn build_remove_node_label_payload(node_id: NodeId, label_id: LabelId) -> Vec<u8> {
  build_add_node_label_payload(node_id, label_id)
}

// ============================================================================
// Payload Parsers
// ============================================================================

/// Parsed CREATE_NODE data
#[derive(Debug, Clone)]
pub struct CreateNodeData {
  pub node_id: NodeId,
  pub key: Option<String>,
}

/// Parse CREATE_NODE payload
pub fn parse_create_node_payload(payload: &[u8]) -> Option<CreateNodeData> {
  if payload.len() < 12 {
    return None;
  }
  let node_id = read_u64(payload, 0);
  let key_len = read_u32(payload, 8) as usize;
  let key = if key_len > 0 && payload.len() >= 12 + key_len {
    String::from_utf8(payload[12..12 + key_len].to_vec()).ok()
  } else {
    None
  };
  Some(CreateNodeData { node_id, key })
}

/// Parsed DELETE_NODE data
#[derive(Debug, Clone)]
pub struct DeleteNodeData {
  pub node_id: NodeId,
}

/// Parse DELETE_NODE payload
pub fn parse_delete_node_payload(payload: &[u8]) -> Option<DeleteNodeData> {
  if payload.len() < 8 {
    return None;
  }
  Some(DeleteNodeData {
    node_id: read_u64(payload, 0),
  })
}

/// Parsed ADD_EDGE data
#[derive(Debug, Clone)]
pub struct AddEdgeData {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
}

/// Parse ADD_EDGE payload
pub fn parse_add_edge_payload(payload: &[u8]) -> Option<AddEdgeData> {
  if payload.len() < 20 {
    return None;
  }
  Some(AddEdgeData {
    src: read_u64(payload, 0),
    etype: read_u32(payload, 8),
    dst: read_u64(payload, 12),
  })
}

/// Parse DELETE_EDGE payload (same format as ADD_EDGE)
pub fn parse_delete_edge_payload(payload: &[u8]) -> Option<AddEdgeData> {
  parse_add_edge_payload(payload)
}

/// Parsed DEFINE_LABEL data
#[derive(Debug, Clone)]
pub struct DefineLabelData {
  pub label_id: LabelId,
  pub name: String,
}

/// Parse DEFINE_LABEL payload
pub fn parse_define_label_payload(payload: &[u8]) -> Option<DefineLabelData> {
  if payload.len() < 8 {
    return None;
  }
  let label_id = read_u32(payload, 0);
  let name_len = read_u32(payload, 4) as usize;
  if payload.len() < 8 + name_len {
    return None;
  }
  let name = String::from_utf8(payload[8..8 + name_len].to_vec()).ok()?;
  Some(DefineLabelData { label_id, name })
}

/// Parse DEFINE_ETYPE payload (same format as DEFINE_LABEL)
pub fn parse_define_etype_payload(payload: &[u8]) -> Option<DefineLabelData> {
  parse_define_label_payload(payload)
}

/// Parse DEFINE_PROPKEY payload (same format as DEFINE_LABEL)
pub fn parse_define_propkey_payload(payload: &[u8]) -> Option<DefineLabelData> {
  parse_define_label_payload(payload)
}

/// Parsed ADD_NODE_LABEL data
#[derive(Debug, Clone)]
pub struct AddNodeLabelData {
  pub node_id: NodeId,
  pub label_id: LabelId,
}

/// Parse ADD_NODE_LABEL payload
pub fn parse_add_node_label_payload(payload: &[u8]) -> Option<AddNodeLabelData> {
  if payload.len() < 12 {
    return None;
  }
  Some(AddNodeLabelData {
    node_id: read_u64(payload, 0),
    label_id: read_u32(payload, 8),
  })
}

/// Parse REMOVE_NODE_LABEL payload (same format as ADD_NODE_LABEL)
pub fn parse_remove_node_label_payload(payload: &[u8]) -> Option<AddNodeLabelData> {
  parse_add_node_label_payload(payload)
}

/// Parse a property value from payload
fn parse_prop_value(payload: &[u8], offset: usize) -> Option<(PropValue, usize)> {
  if offset >= payload.len() {
    return None;
  }

  let tag = payload[offset];
  match PropValueTag::from_u8(tag)? {
    PropValueTag::Null => Some((PropValue::Null, 1)),
    PropValueTag::Bool => {
      if offset + 2 > payload.len() {
        return None;
      }
      Some((PropValue::Bool(payload[offset + 1] != 0), 2))
    }
    PropValueTag::I64 => {
      if offset + 9 > payload.len() {
        return None;
      }
      Some((PropValue::I64(read_i64(payload, offset + 1)), 9))
    }
    PropValueTag::F64 => {
      if offset + 9 > payload.len() {
        return None;
      }
      Some((PropValue::F64(read_f64(payload, offset + 1)), 9))
    }
    PropValueTag::String => {
      if offset + 5 > payload.len() {
        return None;
      }
      let str_len = read_u32(payload, offset + 1) as usize;
      if offset + 5 + str_len > payload.len() {
        return None;
      }
      let s = String::from_utf8(payload[offset + 5..offset + 5 + str_len].to_vec()).ok()?;
      Some((PropValue::String(s), 5 + str_len))
    }
    PropValueTag::VectorF32 => {
      if offset + 5 > payload.len() {
        return None;
      }
      let dimensions = read_u32(payload, offset + 1) as usize;
      if offset + 5 + dimensions * 4 > payload.len() {
        return None;
      }
      let mut vector = Vec::with_capacity(dimensions);
      for i in 0..dimensions {
        let bytes = [
          payload[offset + 5 + i * 4],
          payload[offset + 5 + i * 4 + 1],
          payload[offset + 5 + i * 4 + 2],
          payload[offset + 5 + i * 4 + 3],
        ];
        vector.push(f32::from_le_bytes(bytes));
      }
      Some((PropValue::VectorF32(vector), 5 + dimensions * 4))
    }
  }
}

/// Parsed SET_NODE_PROP data
#[derive(Debug, Clone)]
pub struct SetNodePropData {
  pub node_id: NodeId,
  pub key_id: PropKeyId,
  pub value: PropValue,
}

/// Parse SET_NODE_PROP payload
pub fn parse_set_node_prop_payload(payload: &[u8]) -> Option<SetNodePropData> {
  if payload.len() < 12 {
    return None;
  }
  let node_id = read_u64(payload, 0);
  let key_id = read_u32(payload, 8);
  let (value, _) = parse_prop_value(payload, 12)?;
  Some(SetNodePropData {
    node_id,
    key_id,
    value,
  })
}

/// Parsed DEL_NODE_PROP data
#[derive(Debug, Clone)]
pub struct DelNodePropData {
  pub node_id: NodeId,
  pub key_id: PropKeyId,
}

/// Parse DEL_NODE_PROP payload
pub fn parse_del_node_prop_payload(payload: &[u8]) -> Option<DelNodePropData> {
  if payload.len() < 12 {
    return None;
  }
  Some(DelNodePropData {
    node_id: read_u64(payload, 0),
    key_id: read_u32(payload, 8),
  })
}

/// Parsed SET_NODE_VECTOR data
#[derive(Debug, Clone)]
pub struct SetNodeVectorData {
  pub node_id: NodeId,
  pub prop_key_id: PropKeyId,
  pub dimensions: usize,
  pub vector: Vec<f32>,
}

/// Parse SET_NODE_VECTOR payload
pub fn parse_set_node_vector_payload(payload: &[u8]) -> Option<SetNodeVectorData> {
  if payload.len() < 16 {
    return None;
  }
  let node_id = read_u64(payload, 0);
  let prop_key_id = read_u32(payload, 8);
  let dimensions = read_u32(payload, 12) as usize;

  if payload.len() < 16 + dimensions * 4 {
    return None;
  }

  let mut vector = Vec::with_capacity(dimensions);
  for i in 0..dimensions {
    let bytes = [
      payload[16 + i * 4],
      payload[16 + i * 4 + 1],
      payload[16 + i * 4 + 2],
      payload[16 + i * 4 + 3],
    ];
    vector.push(f32::from_le_bytes(bytes));
  }

  Some(SetNodeVectorData {
    node_id,
    prop_key_id,
    dimensions,
    vector,
  })
}

/// Parsed DEL_NODE_VECTOR data
#[derive(Debug, Clone)]
pub struct DelNodeVectorData {
  pub node_id: NodeId,
  pub prop_key_id: PropKeyId,
}

/// Parse DEL_NODE_VECTOR payload
pub fn parse_del_node_vector_payload(payload: &[u8]) -> Option<DelNodeVectorData> {
  if payload.len() < 12 {
    return None;
  }
  Some(DelNodeVectorData {
    node_id: read_u64(payload, 0),
    prop_key_id: read_u32(payload, 8),
  })
}

/// Parsed SET_EDGE_PROP data
#[derive(Debug, Clone)]
pub struct SetEdgePropData {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
  pub key_id: PropKeyId,
  pub value: PropValue,
}

/// Parse SET_EDGE_PROP payload
/// Format: src (8) + etype (4) + dst (8) + key_id (4) + value (variable)
pub fn parse_set_edge_prop_payload(payload: &[u8]) -> Option<SetEdgePropData> {
  if payload.len() < 24 {
    return None;
  }
  let src = read_u64(payload, 0);
  let etype = read_u32(payload, 8);
  let dst = read_u64(payload, 12);
  let key_id = read_u32(payload, 20);
  let (value, _) = parse_prop_value(payload, 24)?;
  Some(SetEdgePropData {
    src,
    etype,
    dst,
    key_id,
    value,
  })
}

/// Parsed DEL_EDGE_PROP data
#[derive(Debug, Clone)]
pub struct DelEdgePropData {
  pub src: NodeId,
  pub etype: ETypeId,
  pub dst: NodeId,
  pub key_id: PropKeyId,
}

/// Parse DEL_EDGE_PROP payload
/// Format: src (8) + etype (4) + dst (8) + key_id (4)
pub fn parse_del_edge_prop_payload(payload: &[u8]) -> Option<DelEdgePropData> {
  if payload.len() < 24 {
    return None;
  }
  Some(DelEdgePropData {
    src: read_u64(payload, 0),
    etype: read_u32(payload, 8),
    dst: read_u64(payload, 12),
    key_id: read_u32(payload, 20),
  })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_wal_record_roundtrip() {
    let record = WalRecord::new(
      WalRecordType::CreateNode,
      42,
      build_create_node_payload(123, Some("test_key")),
    );

    let bytes = record.build();
    let parsed = parse_wal_record(&bytes, 0).unwrap();

    assert_eq!(parsed.record_type, WalRecordType::CreateNode);
    assert_eq!(parsed.txid, 42);

    let data = parse_create_node_payload(&parsed.payload).unwrap();
    assert_eq!(data.node_id, 123);
    assert_eq!(data.key, Some("test_key".to_string()));
  }

  #[test]
  fn test_edge_payload() {
    let payload = build_add_edge_payload(1, 100, 2);
    let data = parse_add_edge_payload(&payload).unwrap();

    assert_eq!(data.src, 1);
    assert_eq!(data.etype, 100);
    assert_eq!(data.dst, 2);
  }

  #[test]
  fn test_prop_value_string() {
    let value = PropValue::String("hello world".to_string());
    let payload = build_set_node_prop_payload(42, 5, &value);
    let data = parse_set_node_prop_payload(&payload).unwrap();

    assert_eq!(data.node_id, 42);
    assert_eq!(data.key_id, 5);
    assert_eq!(data.value, PropValue::String("hello world".to_string()));
  }

  #[test]
  fn test_prop_value_i64() {
    let value = PropValue::I64(-12345);
    let payload = build_set_node_prop_payload(1, 2, &value);
    let data = parse_set_node_prop_payload(&payload).unwrap();
    assert_eq!(data.value, PropValue::I64(-12345));
  }

  #[test]
  fn test_vector_payload() {
    let vector = vec![1.0, 2.0, 3.0, 4.0];
    let payload = build_set_node_vector_payload(100, 10, &vector);
    let data = parse_set_node_vector_payload(&payload).unwrap();

    assert_eq!(data.node_id, 100);
    assert_eq!(data.prop_key_id, 10);
    assert_eq!(data.dimensions, 4);
    assert_eq!(data.vector, vector);
  }
}
