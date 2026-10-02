//! raydb-b4 `sigbus` lane: a replica's bootstrap reads its primary's live
//! database file without holding its lock (`open_replication_source`), so the
//! primary can checkpoint, reuse, and truncate that file meanwhile. Included
//! from open.rs.
//!
//! The scenario runs in a child process: before the fix it is killed by
//! SIGBUS, which would take down every other test in this binary.

use crate::core::single_file::open::open_replication_source;
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions,
};
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

const CHILD_ENV: &str = "KITEDB_B4_SIGBUS_CHILD";
const CHILD_DONE: &str = "b4-sigbus-child-done";

/// Run `test_name` (a test of this module) in a child process with
/// `CHILD_ENV` set, and fail unless it ran to the end.
fn run_in_child(test_name: &str) {
  let module = module_path!()
    .split_once("::")
    .map_or(module_path!(), |(_, rest)| rest);
  let output = Command::new(std::env::current_exe().expect("current test binary"))
    .args([
      "--exact",
      &format!("{module}::{test_name}"),
      "--nocapture",
      "--test-threads=1",
    ])
    .env(CHILD_ENV, "1")
    .output()
    .expect("spawn child test process");
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);
  #[cfg(unix)]
  if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&output.status) {
    panic!(
      "child process was killed by signal {signal} (SIGBUS is 7 on Linux, 10 on macOS)\n\
       stdout:\n{stdout}\nstderr:\n{stderr}"
    );
  }
  assert!(
    output.status.success() && stdout.contains(CHILD_DONE),
    "child process failed ({})\nstdout:\n{stdout}\nstderr:\n{stderr}",
    output.status
  );
}

/// Uncompressed sections stay in the mapping after the parse (compressed
/// ones are decoded into memory), so reads after the truncation touch it.
fn writer_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .background_checkpoint(false)
    .disable_checkpoint_compression()
}

fn create_keyed_nodes(db: &SingleFileDB, keys: impl Iterator<Item = String>) -> Vec<u64> {
  db.begin(false).expect("begin");
  let ids = keys
    .map(|key| db.create_node(Some(&key)).expect("create node"))
    .collect();
  db.commit().expect("commit");
  ids
}

fn file_len(path: &Path) -> u64 {
  std::fs::metadata(path).expect("db metadata").len()
}

/// A replica bootstrapping from a live primary opens the primary's file
/// without its lock and mapped the installed snapshot. A primary checkpoint
/// that writes its new snapshot into an earlier free range retires the old
/// snapshot and truncates it from the file's tail. The replica's next read
/// of the old mapping touched pages past the end of the file, and the OS
/// killed the process with SIGBUS (`replication_phase_d`'s
/// `bootstrap_handles_concurrent_primary_writes_safely`, about one run in
/// five).
///
/// Deterministic: the primary's snapshot is placed at the file's tail with
/// a larger free range before it, the source opens, and the primary
/// checkpoints once more, which moves the snapshot into that range and
/// truncates the tail the source had mapped. (The flaky crash hit the same
/// mapping during the source's open, while the parse read it.)
#[test]
fn b4_sigbus_replication_source_survives_primary_truncating_its_snapshot() {
  if std::env::var_os(CHILD_ENV).is_none() {
    run_in_child("b4_sigbus_replication_source_survives_primary_truncating_its_snapshot");
    return;
  }

  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("sigbus-primary.kitedb");
  let primary = open_single_file(&path, writer_options()).expect("open primary");

  // S1: a large snapshot.
  let ids = create_keyed_nodes(&primary, (0..4_000).map(|i| format!("node-{i:05}")));
  primary.checkpoint().expect("checkpoint S1");
  let s1 = primary.header.read().clone();

  // S2: a small snapshot, appended after S1 (installed pages are never
  // reused); S1's pages are freed once S2 is installed in both header slots.
  primary.begin(false).expect("begin delete");
  for &id in &ids[500..] {
    primary.delete_node(id).expect("delete node");
  }
  primary.commit().expect("commit delete");
  primary.checkpoint().expect("checkpoint S2");
  let s2 = primary.header.read().clone();
  assert!(
    s2.snapshot_start_page >= s1.snapshot_start_page + s1.snapshot_page_count,
    "setup: S2 must follow S1 (S1 {}+{}, S2 {}+{})",
    s1.snapshot_start_page,
    s1.snapshot_page_count,
    s2.snapshot_start_page,
    s2.snapshot_page_count
  );
  let s2_start = s2.snapshot_start_page * u64::from(s2.page_size);

  // The replica's view: the primary's live file, opened without its lock.
  let source = open_replication_source(&path).expect("open replication source");

  // S3 fits S1's free range; S2, now retired at the tail, is truncated.
  create_keyed_nodes(&primary, std::iter::once("after-source-open".to_string()));
  primary.checkpoint().expect("checkpoint S3");
  let s3 = primary.header.read().clone();
  assert!(
    s3.snapshot_start_page < s2.snapshot_start_page,
    "setup: S3 must reuse S1's range (S2 at page {}, S3 at page {})",
    s2.snapshot_start_page,
    s3.snapshot_start_page
  );
  assert!(
    file_len(&path) <= s2_start,
    "setup: the primary must truncate all of S2 (file {} bytes, S2 at byte {s2_start})",
    file_len(&path)
  );

  // Read the source's snapshot: before the fix these reads touch the mapped
  // S2 pages past the end of the file.
  for (i, &id) in ids[..500].iter().enumerate() {
    let key = format!("node-{i:05}");
    assert_eq!(source.node_by_key(&key), Some(id), "{key}");
    assert_eq!(source.node_key(id).as_deref(), Some(key.as_str()));
  }
  assert_eq!(source.node_by_key("node-00500"), None);
  assert_eq!(source.node_by_key("after-source-open"), None);

  close_single_file(source).expect("close source");
  close_single_file(primary).expect("close primary");
  println!("{CHILD_DONE}");
}
