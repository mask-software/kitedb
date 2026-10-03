//! raydb-b4 `core-misc` lane: `SingleFileDB::check` and the snapshot checks.
//! Included from check.rs.
use super::*;
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use crate::core::snapshot::reader::SnapshotData;
use crate::core::snapshot::writer::{
  build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
};
use crate::types::{PropValue, SectionId, SECTION_ENTRY_SIZE, SNAPSHOT_HEADER_SIZE};
use crate::util::binary::{read_u64, write_u32, write_u64};
use crate::util::crc::crc32;
use std::collections::HashMap;
use std::io::Write;
use tempfile::{tempdir, NamedTempFile};

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

/// A snapshot of nodes "a" -> "b" whose first key-index entry has the wrong
/// hash. Loading accepts it (nothing on the load path rehashes keys), but
/// `lookup_by_key` no longer finds that key; `check_snapshot` reports it.
fn snapshot_with_a_wrong_key_hash() -> SnapshotData {
  let node = |node_id, key: &str| NodeData {
    node_id,
    key: Some(key.to_string()),
    labels: Vec::new(),
    props: HashMap::new(),
  };
  let mut buffer = build_snapshot_to_memory(SnapshotBuildInput {
    generation: 1,
    nodes: vec![node(1, "a"), node(2, "b")],
    edges: vec![EdgeData {
      src: 1,
      etype: 1,
      dst: 2,
      props: HashMap::new(),
    }],
    labels: HashMap::new(),
    etypes: HashMap::from([(1, "knows".to_string())]),
    propkeys: HashMap::new(),
    vector_stores: None,
    compression: None,
  })
  .expect("build snapshot");

  let entry = SNAPSHOT_HEADER_SIZE + SectionId::KeyEntries as usize * SECTION_ENTRY_SIZE;
  let key_entries = read_u64(&buffer, entry) as usize;
  let hash = read_u64(&buffer, key_entries);
  write_u64(&mut buffer, key_entries, hash ^ 1);
  let crc_offset = buffer.len() - 4;
  let crc = crc32(&buffer[..crc_offset]);
  write_u32(&mut buffer, crc_offset, crc);

  let mut file = NamedTempFile::new().expect("temp file");
  file.write_all(&buffer).expect("write snapshot");
  SnapshotData::load(file.path()).expect("load accepts the corrupt key index")
}

/// `check()` validated only what the graph API reports (edge endpoints,
/// edge_exists, counts) and never ran `check_snapshot`, so a snapshot whose
/// key index, CSR order or reciprocity was corrupt checked valid.
#[test]
fn check_reports_a_corrupt_snapshot() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("corrupt.kitedb"), options()).expect("open");
  **db.snapshot.write() = Some(snapshot_with_a_wrong_key_hash());

  let report = db.check();
  // Leave the database as it was opened before closing it.
  db.snapshot.write().take();
  close_single_file(db).expect("close");

  assert!(
    !report.valid
      && report
        .errors
        .iter()
        .any(|error| error.starts_with("snapshot: ") && error.contains("key entry 0")),
    "check() must report the snapshot's wrong key hash; got {report:?}"
  );
}

/// The low-level API accepts label, edge type and property key IDs that were
/// never defined, and a snapshot stores them as they are. A database built
/// that way is valid: `check()` must say so, and warn about the IDs.
#[test]
fn check_warns_about_undefined_schema_ids_without_failing() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("undefined-ids.kitedb");
  let db = open_single_file(&path, options()).expect("open");
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("node a");
  let b = db.create_node(Some("b")).expect("node b");
  db.add_node_label(a, 7).expect("undefined label");
  db.add_edge(a, 9, b).expect("undefined edge type");
  db.set_node_prop(a, 11, PropValue::I64(1))
    .expect("undefined prop key");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");

  let schema_warnings = |report: &CheckResult| {
    ["NodeLabelIds", "OutEtype", "NodePropKeys"]
      .into_iter()
      .filter(|section| {
        !report
          .warnings
          .iter()
          .any(|warning| warning.starts_with("snapshot: ") && warning.contains(section))
      })
      .collect::<Vec<_>>()
  };

  let report = db.check();
  close_single_file(db).expect("close");
  assert!(
    report.valid,
    "undefined schema IDs are valid; got errors {:?}",
    report.errors
  );
  let missing = schema_warnings(&report);
  assert!(
    missing.is_empty(),
    "check() must warn about the undefined IDs in {missing:?}; warnings: {:?}",
    report.warnings
  );

  let reopened = open_single_file(&path, options()).expect("reopen");
  let report = reopened.check();
  close_single_file(reopened).expect("close");
  assert!(
    report.valid && schema_warnings(&report).is_empty(),
    "after reopen: {report:?}"
  );
}
