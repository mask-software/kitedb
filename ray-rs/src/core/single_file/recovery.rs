//! WAL recovery for SingleFileDB
//!
//! Handles scanning WAL records and replaying them during database open.

use std::collections::HashMap;

use crate::core::pager::FilePager;
use crate::core::snapshot::reader::SnapshotData;
use crate::core::wal::record::{
  extract_committed_transactions_in_order, parse_add_edge_payload, parse_add_edge_props_payload,
  parse_add_edges_batch_payload, parse_add_edges_props_batch_payload, parse_add_node_label_payload,
  parse_create_node_payload, parse_create_nodes_batch_payload, parse_define_etype_payload,
  parse_define_label_payload, parse_define_propkey_payload, parse_del_edge_prop_payload,
  parse_del_node_prop_payload, parse_del_node_vector_payload, parse_delete_edge_payload,
  parse_delete_node_payload, parse_remove_node_label_payload, parse_set_edge_prop_payload,
  parse_set_edge_props_payload, parse_set_node_prop_payload, parse_set_node_vector_payload,
  ParsedWalRecord,
};
use crate::error::{KiteError, Result};
use crate::types::*;

/// Scan WAL records from the WAL area (linear), from the tail up to the
/// first record that does not parse with its region's salt or does not end by
/// the head.
///
/// Only the bytes from the tail to the head are read, with one positioned
/// read: nothing past the head belongs to a record the header names.
pub(crate) fn scan_wal_records(
  pager: &mut FilePager,
  header: &DbHeaderV1,
) -> Result<Vec<ParsedWalRecord>> {
  use crate::core::wal::buffer::header_salt_at;
  use crate::core::wal::record::parse_wal_record_with_salt;

  let mut records = Vec::new();
  let wal_size = header.wal_page_count * header.page_size as u64;
  let (tail, head) = (header.wal_tail, header.wal_head);

  if head < tail {
    return Err(KiteError::InvalidWal(
      "WAL head cannot be behind tail in linear mode".to_string(),
    ));
  }
  if head > wal_size {
    return Err(KiteError::InvalidWal(
      "WAL head exceeds WAL size".to_string(),
    ));
  }

  // If tail == head, WAL is empty
  if tail == head {
    return Ok(records);
  }

  let wal_offset = header.wal_start_page * header.page_size as u64;
  let live = pager.read_range(wal_offset + tail, (head - tail) as usize)?;
  let mut offset = 0;
  while offset < live.len() {
    let salt = header_salt_at(header, tail + offset as u64);
    match parse_wal_record_with_salt(&live, offset, salt) {
      Some(record) => {
        offset = record.record_end;
        records.push(record);
      }
      None => break, // Invalid record
    }
  }

  Ok(records)
}

/// Read the entire WAL area into memory, with one positioned read.
#[cfg(test)]
pub(crate) fn read_wal_area(pager: &mut FilePager, header: &DbHeaderV1) -> Result<Vec<u8>> {
  let page_size = header.page_size as u64;
  pager.read_range(
    header.wal_start_page * page_size,
    (header.wal_page_count * page_size) as usize,
  )
}

/// Extract committed transactions from WAL records in COMMIT-record order.
pub(crate) fn committed_transactions(
  wal_records: &[ParsedWalRecord],
) -> Vec<(TxId, Vec<&ParsedWalRecord>)> {
  extract_committed_transactions_in_order(wal_records)
}

/// Replay a single WAL record into delta and update allocators/schema.
///
/// Returns whether replay applied it: `false` for the vector maintenance
/// record types (`BatchVectors`, `SealFragment`, `CompactFragments`), which
/// no version writes and replay skips. Fails with `InvalidWal` on a record
/// whose payload does not parse: its CRC checked, so it is not torn by a
/// crash but written by a buggy or incompatible version, and skipping it
/// would silently drop part of a committed transaction.
#[allow(clippy::too_many_arguments)]
pub fn replay_wal_record(
  record: &ParsedWalRecord,
  snapshot: Option<&SnapshotData>,
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
) -> Result<bool> {
  match record.record_type {
    WalRecordType::CreateNode => {
      let data = payload(record, parse_create_node_payload(&record.payload))?;
      replay_create_node(snapshot, delta, data.node_id, data.key.as_deref());
      if data.node_id >= *next_node_id {
        *next_node_id = data.node_id.saturating_add(1);
      }
    }
    WalRecordType::CreateNodesBatch => {
      let nodes = payload(record, parse_create_nodes_batch_payload(&record.payload))?;
      for data in nodes {
        replay_create_node(snapshot, delta, data.node_id, data.key.as_deref());
        if data.node_id >= *next_node_id {
          *next_node_id = data.node_id.saturating_add(1);
        }
      }
    }
    WalRecordType::DeleteNode => {
      let data = payload(record, parse_delete_node_payload(&record.payload))?;
      delta.delete_node(data.node_id);
    }
    WalRecordType::AddEdge => {
      let data = payload(record, parse_add_edge_payload(&record.payload))?;
      replay_add_edge(snapshot, delta, data.src, data.etype, data.dst);
    }
    WalRecordType::AddEdgesBatch => {
      let edges = payload(record, parse_add_edges_batch_payload(&record.payload))?;
      for data in edges {
        replay_add_edge(snapshot, delta, data.src, data.etype, data.dst);
      }
    }
    WalRecordType::AddEdgeProps => {
      let data = payload(record, parse_add_edge_props_payload(&record.payload))?;
      if replay_add_edge(snapshot, delta, data.src, data.etype, data.dst) {
        for (key_id, value) in data.props {
          delta.set_edge_prop(data.src, data.etype, data.dst, key_id, value);
        }
      }
    }
    WalRecordType::AddEdgesPropsBatch => {
      let edges = payload(record, parse_add_edges_props_batch_payload(&record.payload))?;
      for data in edges {
        if replay_add_edge(snapshot, delta, data.src, data.etype, data.dst) {
          for (key_id, value) in data.props {
            delta.set_edge_prop(data.src, data.etype, data.dst, key_id, value);
          }
        }
      }
    }
    WalRecordType::DeleteEdge => {
      let data = payload(record, parse_delete_edge_payload(&record.payload))?;
      let in_snapshot = delta.snapshot_edge_over(snapshot, data.src, data.etype, data.dst);
      delta.delete_edge_over(data.src, data.etype, data.dst, in_snapshot);
    }
    // Prop and label writes to a node or edge that does not exist (older
    // versions let them through) are skipped, as `replay_add_edge` skips
    // edges to missing nodes: they would surface if the id came back.
    WalRecordType::SetNodeProp => {
      let data = payload(record, parse_set_node_prop_payload(&record.payload))?;
      if delta.node_exists_over(snapshot, data.node_id) {
        delta.set_node_prop(data.node_id, data.key_id, data.value);
      }
    }
    WalRecordType::DelNodeProp => {
      let data = payload(record, parse_del_node_prop_payload(&record.payload))?;
      if delta.node_exists_over(snapshot, data.node_id) {
        delta.delete_node_prop(data.node_id, data.key_id);
      }
    }
    WalRecordType::DefineLabel => {
      let data = payload(record, parse_define_label_payload(&record.payload))?;
      delta.define_label(data.label_id, &data.name);
      label_names.insert(data.name.clone(), data.label_id);
      label_ids.insert(data.label_id, data.name);
      if data.label_id >= *next_label_id {
        *next_label_id = data.label_id + 1;
      }
    }
    WalRecordType::DefineEtype => {
      let data = payload(record, parse_define_etype_payload(&record.payload))?;
      delta.define_etype(data.label_id, &data.name);
      etype_names.insert(data.name.clone(), data.label_id);
      etype_ids.insert(data.label_id, data.name);
      if data.label_id >= *next_etype_id {
        *next_etype_id = data.label_id + 1;
      }
    }
    WalRecordType::DefinePropkey => {
      let data = payload(record, parse_define_propkey_payload(&record.payload))?;
      delta.define_propkey(data.label_id, &data.name);
      propkey_names.insert(data.name.clone(), data.label_id);
      propkey_ids.insert(data.label_id, data.name);
      if data.label_id >= *next_propkey_id {
        *next_propkey_id = data.label_id + 1;
      }
    }
    WalRecordType::AddNodeLabel => {
      let data = payload(record, parse_add_node_label_payload(&record.payload))?;
      if delta.node_exists_over(snapshot, data.node_id) {
        delta.add_node_label(data.node_id, data.label_id);
      }
    }
    WalRecordType::RemoveNodeLabel => {
      let data = payload(record, parse_remove_node_label_payload(&record.payload))?;
      if delta.node_exists_over(snapshot, data.node_id) {
        delta.remove_node_label(data.node_id, data.label_id);
      }
    }
    WalRecordType::SetEdgeProp => {
      let data = payload(record, parse_set_edge_prop_payload(&record.payload))?;
      if delta.edge_exists_over(snapshot, data.src, data.etype, data.dst) {
        delta.set_edge_prop(data.src, data.etype, data.dst, data.key_id, data.value);
      }
    }
    WalRecordType::SetEdgeProps => {
      let data = payload(record, parse_set_edge_props_payload(&record.payload))?;
      if delta.edge_exists_over(snapshot, data.src, data.etype, data.dst) {
        for (key_id, value) in data.props {
          delta.set_edge_prop(data.src, data.etype, data.dst, key_id, value);
        }
      }
    }
    WalRecordType::DelEdgeProp => {
      let data = payload(record, parse_del_edge_prop_payload(&record.payload))?;
      if delta.edge_exists_over(snapshot, data.src, data.etype, data.dst) {
        delta.delete_edge_prop(data.src, data.etype, data.dst, data.key_id);
      }
    }
    WalRecordType::SetNodeVector => {
      let data = payload(record, parse_set_node_vector_payload(&record.payload))?;
      delta.pending_vectors.insert(
        (data.node_id, data.prop_key_id),
        Some(VectorRef::from(data.vector)),
      );
    }
    WalRecordType::DelNodeVector => {
      let data = payload(record, parse_del_node_vector_payload(&record.payload))?;
      delta
        .pending_vectors
        .insert((data.node_id, data.prop_key_id), None);
    }
    WalRecordType::BatchVectors | WalRecordType::SealFragment | WalRecordType::CompactFragments => {
      return Ok(false);
    }
    // Transaction boundaries: committed_transactions drops them.
    WalRecordType::Begin | WalRecordType::Commit | WalRecordType::Rollback => {}
  }
  Ok(true)
}

/// The parsed payload of `record`, or `InvalidWal` if it did not parse.
fn payload<T>(record: &ParsedWalRecord, parsed: Option<T>) -> Result<T> {
  parsed.ok_or_else(|| {
    KiteError::InvalidWal(format!(
      "{:?} record of transaction {} passed its checksum but its {}-byte payload does not \
       parse: it was written by a buggy or incompatible version, not torn by a crash",
      record.record_type,
      record.txid,
      record.payload.len()
    ))
  })
}

/// Replay an edge add under the write path's rules: both endpoints must exist,
/// and an edge that is already visible is not added again. Older WALs may hold
/// either. Returns whether the edge exists afterwards.
fn replay_add_edge(
  snapshot: Option<&SnapshotData>,
  delta: &mut DeltaState,
  src: NodeId,
  etype: ETypeId,
  dst: NodeId,
) -> bool {
  if !delta.node_exists_over(snapshot, src) || !delta.node_exists_over(snapshot, dst) {
    return false;
  }
  let in_snapshot = delta.snapshot_edge_over(snapshot, src, etype, dst);
  delta.add_edge_over(src, etype, dst, in_snapshot);
  true
}

/// Replay a node create under the write path's rule: the id must not exist.
/// A create of a snapshot node is skipped, unless a replayed delete removed
/// it: then it is a recreate. Older WALs may repeat a create.
fn replay_create_node(
  snapshot: Option<&SnapshotData>,
  delta: &mut DeltaState,
  node_id: NodeId,
  key: Option<&str>,
) {
  if !delta.node_exists_over(snapshot, node_id) {
    delta.create_node(node_id, key);
  }
}

/// After replay, turn vector sets for nodes that no longer exist into deletes.
/// Older WALs logged a node delete without deletes for its vectors.
pub(crate) fn drop_vectors_of_missing_nodes(
  delta: &mut DeltaState,
  snapshot: Option<&SnapshotData>,
) {
  let orphaned: Vec<(NodeId, PropKeyId)> = delta
    .pending_vectors
    .iter()
    .filter(|(&(node_id, _), op)| op.is_some() && !delta.node_exists_over(snapshot, node_id))
    .map(|(&key, _)| key)
    .collect();
  for key in orphaned {
    delta.pending_vectors.insert(key, None);
  }
}

#[cfg(test)]
mod tests {
  // These tests never checkpoint: checkpoint unit tests arm process-wide
  // phase barriers that a concurrent checkpoint here could consume.
  use crate::core::single_file::{
    close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
  };
  use crate::core::wal::record::{
    build_add_edge_payload, build_create_node_payload, build_delete_edge_payload,
    build_delete_node_payload, WalRecord,
  };
  use crate::types::{NodeId, PropValue, WalRecordType};
  use crate::vector::store::vector_store_has;
  use std::path::Path;
  use tempfile::tempdir;

  fn open(path: &Path) -> SingleFileDB {
    open_single_file(path, SingleFileOpenOptions::new().auto_checkpoint(false)).expect("open")
  }

  /// Commit raw WAL records with no in-memory effect, as older versions
  /// logged operations the write path now rejects or skips.
  fn commit_legacy_records(db: &SingleFileDB, records: Vec<(WalRecordType, Vec<u8>)>) {
    db.begin(false).expect("begin");
    let (txid, tx_handle) = db.require_write_tx_handle().expect("write tx");
    for (record_type, payload) in records {
      db.write_wal_tx(&tx_handle, WalRecord::new(record_type, txid, payload))
        .expect("write record");
    }
    db.commit().expect("commit");
  }

  #[test]
  fn replay_drops_edge_to_missing_node() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("dangling.kitedb");
    let missing: NodeId = 999_999;
    let db = open(&path);
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("a");
    let t = db.define_etype("T").expect("etype");
    db.commit().expect("commit");
    commit_legacy_records(
      &db,
      vec![(
        WalRecordType::AddEdge,
        build_add_edge_payload(a, t, missing),
      )],
    );
    close_single_file(db).expect("close");

    let db = open(&path);
    assert!(!db.edge_exists(a, t, missing));
    let delta = db.delta.read();
    assert!(
      delta.out_add.is_empty() && delta.in_add.is_empty(),
      "dangling edge replayed into the delta (would fail the next checkpoint)"
    );
    drop(delta);
    close_single_file(db).expect("close");
  }

  #[test]
  fn replay_skips_delete_of_missing_edge() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("stale_delete.kitedb");
    let db = open(&path);
    db.begin(false).expect("begin");
    let a = db.create_node(Some("a")).expect("a");
    let b = db.create_node(Some("b")).expect("b");
    let t = db.define_etype("T").expect("etype");
    db.commit().expect("commit");
    commit_legacy_records(
      &db,
      vec![(
        WalRecordType::DeleteEdge,
        build_delete_edge_payload(a, t, b),
      )],
    );
    db.begin(false).expect("begin");
    db.add_edge(a, t, b).expect("edge");
    db.commit().expect("commit");
    close_single_file(db).expect("close");

    let db = open(&path);
    assert!(
      db.edge_exists(a, t, b),
      "stale delete swallowed a later add"
    );
    assert_eq!(db.count_edges(), 1);
    close_single_file(db).expect("close");
  }

  #[test]
  fn replay_drops_vector_of_node_deleted_without_vector_delete() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("vector.kitedb");
    let db = open(&path);
    db.begin(false).expect("begin");
    let keep = db.create_node(Some("keep")).expect("keep");
    let node = db.create_node(Some("v")).expect("node");
    let pk = db.define_propkey("embedding").expect("propkey");
    db.set_node_vector(keep, pk, &[0.0, 1.0, 0.0, 0.0])
      .expect("vector");
    db.set_node_vector(node, pk, &[1.0, 0.5, 0.25, 0.125])
      .expect("vector");
    db.commit().expect("commit");
    commit_legacy_records(
      &db,
      vec![(WalRecordType::DeleteNode, build_delete_node_payload(node))],
    );
    close_single_file(db).expect("close");

    let db = open(&path);
    assert!(!db.node_exists(node));
    assert!(db.node_vector(node, pk).is_none());
    assert!(db.has_node_vector(keep, pk));
    let stores = db.vector_stores.read();
    let store = stores.get(&pk).expect("store");
    assert!(
      !vector_store_has(store, node),
      "store keeps the vector, so the next checkpoint would persist it"
    );
    drop(stores);
    close_single_file(db).expect("close");
  }

  #[test]
  fn replay_skips_create_of_existing_node() {
    // Older versions could log a second create for a node (upsert by id did
    // not see a recreated node). Replay must not wipe the node's state.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("repeat_create.kitedb");
    let db = open(&path);
    db.begin(false).expect("begin");
    let node = db.create_node(Some("n")).expect("node");
    let key = db.define_propkey("k").expect("propkey");
    db.set_node_prop(node, key, PropValue::I64(1))
      .expect("prop");
    db.commit().expect("commit");
    commit_legacy_records(
      &db,
      vec![(
        WalRecordType::CreateNode,
        build_create_node_payload(node, None),
      )],
    );
    close_single_file(db).expect("close");

    let db = open(&path);
    assert_eq!(db.node_key(node).as_deref(), Some("n"));
    assert_eq!(db.node_prop(node, key), Some(PropValue::I64(1)));
    close_single_file(db).expect("close");
  }

  #[test]
  fn replay_of_max_node_id_does_not_wrap_allocator() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("max_id.kitedb");
    let db = open(&path);
    db.begin(false).expect("begin");
    let first = db.create_node(None).expect("first");
    db.commit().expect("commit");
    commit_legacy_records(
      &db,
      vec![(
        WalRecordType::CreateNode,
        build_create_node_payload(u64::MAX, None),
      )],
    );
    close_single_file(db).expect("close");

    let db = open(&path);
    assert!(db.node_exists(first));
    db.begin(false).expect("begin");
    assert!(db.create_node(None).is_err(), "allocator must not wrap");
    db.rollback().expect("rollback");
    close_single_file(db).expect("close");
  }
}

/// raydb-b4 `fsync-group` lane: one sync per Full-mode commit group, and the
/// crash images it must survive.
#[cfg(test)]
#[path = "b4_fsync_group_tests.rs"]
pub(crate) mod b4_fsync_group_tests;
