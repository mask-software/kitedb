//! WAL recovery for SingleFileDB
//!
//! Handles scanning WAL records and replaying them during database open.

use std::collections::HashMap;

use crate::constants::*;
use crate::core::pager::FilePager;
use crate::core::wal::record::{
  extract_committed_transactions, parse_add_edge_payload, parse_add_node_label_payload,
  parse_create_node_payload, parse_define_etype_payload, parse_define_label_payload,
  parse_define_propkey_payload, parse_del_edge_prop_payload, parse_del_node_prop_payload,
  parse_del_node_vector_payload, parse_delete_edge_payload, parse_delete_node_payload,
  parse_remove_node_label_payload, parse_set_edge_prop_payload, parse_set_node_prop_payload,
  parse_set_node_vector_payload, ParsedWalRecord,
};
use crate::error::Result;
use crate::types::*;

/// Scan WAL records from the circular buffer
pub(crate) fn scan_wal_records(
  pager: &mut FilePager,
  header: &DbHeaderV1,
) -> Result<Vec<ParsedWalRecord>> {
  use crate::core::wal::record::parse_wal_record;

  let mut records = Vec::new();
  let wal_size = header.wal_page_count * header.page_size as u64;

  let mut pos = header.wal_tail;
  let head = header.wal_head;

  // If tail == head, WAL is empty
  if pos == head {
    return Ok(records);
  }

  // Read the WAL area into memory for scanning
  // This is simpler than page-by-page reading for now
  let wal_data = read_wal_area(pager, header)?;

  while pos != head {
    // Handle wrap-around
    let actual_pos = pos % wal_size;

    // Check for skip marker
    if actual_pos + 8 > wal_size {
      // Not enough space for header, wrap to start
      pos = 0;
      continue;
    }

    let offset = actual_pos as usize;
    if offset + 4 > wal_data.len() {
      break;
    }

    let rec_len = u32::from_le_bytes([
      wal_data[offset],
      wal_data[offset + 1],
      wal_data[offset + 2],
      wal_data[offset + 3],
    ]) as usize;

    // Skip marker check
    if rec_len == 0 {
      if offset + 8 <= wal_data.len() {
        let marker = u32::from_le_bytes([
          wal_data[offset + 4],
          wal_data[offset + 5],
          wal_data[offset + 6],
          wal_data[offset + 7],
        ]);
        if marker == 0xFFFFFFFF {
          // Skip to start
          pos = 0;
          continue;
        }
      }
      break; // Invalid record
    }

    // Parse the record
    if let Some(record) = parse_wal_record(&wal_data, offset) {
      let aligned_size = crate::util::binary::align_up(rec_len, WAL_RECORD_ALIGNMENT);
      pos = (actual_pos + aligned_size as u64) % wal_size;
      records.push(record);
    } else {
      break; // Invalid record
    }
  }

  Ok(records)
}

/// Read the entire WAL area into memory
pub(crate) fn read_wal_area(pager: &mut FilePager, header: &DbHeaderV1) -> Result<Vec<u8>> {
  let wal_pages = header.wal_page_count as u32;
  let page_size = header.page_size as usize;
  let mut wal_data = Vec::with_capacity(wal_pages as usize * page_size);

  for i in 0..wal_pages {
    let page_num = header.wal_start_page as u32 + i;
    let page = pager.read_page(page_num)?;
    wal_data.extend_from_slice(&page);
  }

  Ok(wal_data)
}

/// Extract committed transactions from WAL records
pub(crate) fn get_committed_transactions(
  wal_records: &[ParsedWalRecord],
) -> Vec<(TxId, Vec<&ParsedWalRecord>)> {
  extract_committed_transactions(wal_records)
    .into_iter()
    .collect()
}

/// Replay a single WAL record into delta and update allocators/schema
#[allow(clippy::too_many_arguments)]
pub fn replay_wal_record(
  record: &ParsedWalRecord,
  delta: &mut DeltaState,
  next_node_id: &mut u64,
  next_label_id: &mut u32,
  next_etype_id: &mut u32,
  next_propkey_id: &mut u32,
  label_names: &mut HashMap<String, LabelId>,
  label_ids: &mut HashMap<LabelId, String>,
  etype_names: &mut HashMap<String, ETypeId>,
  etype_ids: &mut HashMap<ETypeId, String>,
  propkey_names: &mut HashMap<String, PropKeyId>,
  propkey_ids: &mut HashMap<PropKeyId, String>,
) {
  match record.record_type {
    WalRecordType::CreateNode => {
      if let Some(data) = parse_create_node_payload(&record.payload) {
        delta.create_node(data.node_id, data.key.as_deref());
        if data.node_id >= *next_node_id {
          *next_node_id = data.node_id + 1;
        }
      }
    }
    WalRecordType::DeleteNode => {
      if let Some(data) = parse_delete_node_payload(&record.payload) {
        delta.delete_node(data.node_id);
      }
    }
    WalRecordType::AddEdge => {
      if let Some(data) = parse_add_edge_payload(&record.payload) {
        delta.add_edge(data.src, data.etype, data.dst);
      }
    }
    WalRecordType::DeleteEdge => {
      if let Some(data) = parse_delete_edge_payload(&record.payload) {
        delta.delete_edge(data.src, data.etype, data.dst);
      }
    }
    WalRecordType::SetNodeProp => {
      if let Some(data) = parse_set_node_prop_payload(&record.payload) {
        delta.set_node_prop(data.node_id, data.key_id, data.value);
      }
    }
    WalRecordType::DelNodeProp => {
      if let Some(data) = parse_del_node_prop_payload(&record.payload) {
        delta.delete_node_prop(data.node_id, data.key_id);
      }
    }
    WalRecordType::DefineLabel => {
      if let Some(data) = parse_define_label_payload(&record.payload) {
        delta.define_label(data.label_id, &data.name);
        label_names.insert(data.name.clone(), data.label_id);
        label_ids.insert(data.label_id, data.name);
        if data.label_id >= *next_label_id {
          *next_label_id = data.label_id + 1;
        }
      }
    }
    WalRecordType::DefineEtype => {
      if let Some(data) = parse_define_etype_payload(&record.payload) {
        delta.define_etype(data.label_id, &data.name);
        etype_names.insert(data.name.clone(), data.label_id);
        etype_ids.insert(data.label_id, data.name);
        if data.label_id >= *next_etype_id {
          *next_etype_id = data.label_id + 1;
        }
      }
    }
    WalRecordType::DefinePropkey => {
      if let Some(data) = parse_define_propkey_payload(&record.payload) {
        delta.define_propkey(data.label_id, &data.name);
        propkey_names.insert(data.name.clone(), data.label_id);
        propkey_ids.insert(data.label_id, data.name);
        if data.label_id >= *next_propkey_id {
          *next_propkey_id = data.label_id + 1;
        }
      }
    }
    WalRecordType::AddNodeLabel => {
      if let Some(data) = parse_add_node_label_payload(&record.payload) {
        delta.add_node_label(data.node_id, data.label_id);
      }
    }
    WalRecordType::RemoveNodeLabel => {
      if let Some(data) = parse_remove_node_label_payload(&record.payload) {
        delta.remove_node_label(data.node_id, data.label_id);
      }
    }
    WalRecordType::SetEdgeProp => {
      if let Some(data) = parse_set_edge_prop_payload(&record.payload) {
        delta.set_edge_prop(data.src, data.etype, data.dst, data.key_id, data.value);
      }
    }
    WalRecordType::DelEdgeProp => {
      if let Some(data) = parse_del_edge_prop_payload(&record.payload) {
        delta.delete_edge_prop(data.src, data.etype, data.dst, data.key_id);
      }
    }
    WalRecordType::SetNodeVector => {
      if let Some(data) = parse_set_node_vector_payload(&record.payload) {
        delta
          .pending_vectors
          .insert((data.node_id, data.prop_key_id), Some(data.vector));
      }
    }
    WalRecordType::DelNodeVector => {
      if let Some(data) = parse_del_node_vector_payload(&record.payload) {
        delta
          .pending_vectors
          .insert((data.node_id, data.prop_key_id), None);
      }
    }
    _ => {
      // Other record types (batch vectors, seal fragment, etc.) - skip for now
    }
  }
}
