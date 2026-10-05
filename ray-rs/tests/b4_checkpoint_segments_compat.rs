//! raydb-b4 `checkpoint-segments`: compatibility of WAL segments (design:
//! `raydb-b4/_SEGMENTS_DESIGN.md`) with files of format version 2.
//!
//! The fixtures `tests/fixtures/v2_*.kitedb` were written by the v2 writer
//! (`generate_v2_fixtures`, which went with that writer; see commit 6ff2a82,
//! `b4_checkpoint_segments_tests.rs`; 64 KiB WAL), in each WAL state it
//! leaves: records in the WAL (with an uncommitted
//! transaction's among them), a background checkpoint's cut in progress
//! with commits in the secondary region, a retired primary region, and a cut
//! too big to merge back into the primary region. A new binary must open,
//! read, write, spill, checkpoint and reopen each.
//!
//! A file whose header names WAL segments must need a reader of version 3
//! (older binaries then refuse it with `VersionMismatch` instead of missing
//! the segments' commits), and go back to version 2 once a checkpoint
//! covers every segment.

use std::path::{Path, PathBuf};

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::types::DbHeaderV1;

fn pad(prefix: &str, index: usize) -> String {
  format!("{prefix}-{index}-{}", "p".repeat(100))
}

fn wide(prefix: &str, index: usize, fill: char) -> String {
  format!("{prefix}-{index}-{}", fill.to_string().repeat(1000))
}

struct Fixture {
  name: &'static str,
  present: Vec<String>,
  absent: Vec<String>,
}

fn fixtures() -> Vec<Fixture> {
  let keys = |prefix: &str, count: usize| -> Vec<String> {
    (0..count).map(|index| pad(prefix, index)).collect()
  };
  vec![
    Fixture {
      name: "v2_wal_records.kitedb",
      present: [keys("snap", 50), keys("wal", 30), keys("late", 1)].concat(),
      absent: (0..20).map(|index| wide("open", index, 'o')).collect(),
    },
    Fixture {
      name: "v2_cut_in_progress.kitedb",
      present: [keys("pre", 20), keys("post", 20)].concat(),
      absent: Vec::new(),
    },
    Fixture {
      name: "v2_primary_retired.kitedb",
      present: [keys("pre", 20), keys("post", 20)].concat(),
      absent: Vec::new(),
    },
    Fixture {
      name: "v2_cut_too_big.kitedb",
      present: (0..42)
        .map(|index| wide("fill", index, 'f'))
        .chain((0..6).map(|index| wide("post", index, 'p')))
        .collect(),
      absent: Vec::new(),
    },
  ]
}

fn copy_fixture(name: &str, dir: &Path) -> PathBuf {
  let source = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures")
    .join(name);
  let path = dir.join(name);
  std::fs::copy(source, &path).expect("copy fixture");
  path
}

fn check_keys(db: &kitedb::core::single_file::SingleFileDB, fixture: &Fixture, context: &str) {
  for key in &fixture.present {
    assert!(
      db.node_by_key(key).is_some(),
      "{} {context}: {:.24} missing",
      fixture.name,
      key
    );
  }
  for key in &fixture.absent {
    assert!(
      db.node_by_key(key).is_none(),
      "{} {context}: uncommitted {:.24} present",
      fixture.name,
      key
    );
  }
}

/// The newest valid header slot of the file at `path`, and the number of WAL
/// segments its page names (v3 field at byte 184).
fn newest_header(path: &Path) -> (DbHeaderV1, u32) {
  let bytes = std::fs::read(path).expect("read the file");
  let page_size = 4096;
  (0..2)
    .filter_map(|slot| {
      let page = &bytes[slot * page_size..(slot + 1) * page_size];
      DbHeaderV1::parse(page).ok().map(|header| {
        (
          header,
          u32::from_le_bytes(page[184..188].try_into().unwrap()),
        )
      })
    })
    .max_by_key(|(header, _)| header.change_counter)
    .expect("a valid header slot")
}

/// Commit `count` nodes of about 300 bytes of WAL each (four times the
/// fixtures' 48 KiB primary region at 640).
fn write_nodes(db: &kitedb::core::single_file::SingleFileDB, prefix: &str, count: usize) {
  for index in 0..count {
    db.begin(false).expect("begin");
    db.create_node(Some(&format!("{prefix}-{index}-{}", "n".repeat(200))))
      .expect("node");
    db.commit().expect("commit");
  }
}

/// C1-C4. Each v2 fixture opens read-only (in place) and writable, then takes
/// writes that spill its WAL into segments, a checkpoint, and reopens,
/// keeping every commit.
#[test]
fn v2_files_open_write_spill_and_checkpoint() {
  for fixture in fixtures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = copy_fixture(fixture.name, dir.path());
    let read_only = open_single_file(&path, SingleFileOpenOptions::new().read_only(true))
      .unwrap_or_else(|error| panic!("{}: read-only open: {error}", fixture.name));
    check_keys(&read_only, &fixture, "read-only");
    drop(read_only);

    // No checkpoint while writing: the WAL spills into segments.
    let db = open_single_file(&path, SingleFileOpenOptions::new().auto_checkpoint(false))
      .unwrap_or_else(|error| panic!("{}: writable open: {error}", fixture.name));
    check_keys(&db, &fixture, "writable");
    write_nodes(&db, "new", 640);
    // Dropped, not closed: a clean close checkpoints the segments away.
    drop(db);
    let (header, segments) = newest_header(&path);
    assert!(
      segments > 0 && header.version == 3 && header.min_reader_version == 3,
      "{}: after writes that filled the WAL four times over, the file names {segments} WAL \
       segments (version {}, min reader {})",
      fixture.name,
      header.version,
      header.min_reader_version
    );

    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen");
    check_keys(&db, &fixture, "reopened with segments");
    assert!(db
      .node_by_key(&format!("new-639-{}", "n".repeat(200)))
      .is_some());
    db.checkpoint().expect("checkpoint");
    close_single_file(db).expect("close");
    let (header, segments) = newest_header(&path);
    assert_eq!(
      (segments, header.version, header.min_reader_version),
      (0, 2, 2),
      "{}: a checkpoint left WAL segments, or kept the file at version 3",
      fixture.name
    );
    let db = open_single_file(&path, SingleFileOpenOptions::new()).expect("reopen");
    check_keys(&db, &fixture, "after the checkpoint");
    assert!(db
      .node_by_key(&format!("new-0-{}", "n".repeat(200)))
      .is_some());
  }
}

/// C5. A file whose header names WAL segments needs a reader of version 3:
/// an older binary (version 2) refuses it at open (`check_supported`:
/// `min_reader_version` above its own) instead of opening it without the
/// segments' commits. Once a checkpoint covers every segment the file is a
/// version 2 file again.
#[test]
fn a_file_naming_segments_needs_a_v3_reader() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("needs-v3.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  write_nodes(&db, "key", 640);
  // Dropped, not closed: a clean close checkpoints the segments away (and
  // the file is version 2 again); a process that ends without closing
  // leaves them.
  drop(db);

  let (header, segments) = newest_header(&path);
  let old_reader_refuses = header.min_reader_version > 2;
  assert!(
    segments > 0 && header.version == 3 && old_reader_refuses,
    "the file names {segments} segments with version {} and min reader {}: a version 2 \
     binary would open it and miss their commits",
    header.version,
    header.min_reader_version
  );
  header
    .check_supported(true)
    .expect("this build reads and writes it");

  let db = open_single_file(&path, options.clone()).expect("reopen");
  db.checkpoint().expect("checkpoint");
  close_single_file(db).expect("close");
  let (header, segments) = newest_header(&path);
  assert_eq!(
    (segments, header.version, header.min_reader_version),
    (0, 2, 2)
  );
  let db = open_single_file(&path, options).expect("reopen");
  assert!(db
    .node_by_key(&format!("key-639-{}", "n".repeat(200)))
    .is_some());
}
