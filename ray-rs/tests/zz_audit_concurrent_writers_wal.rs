//! Auto-checkpoints must keep the WAL from filling up when several threads
//! commit concurrently, in both blocking and background mode, with MVCC (the
//! default: the writers' transactions run together) and without it
//! (deprecated: they run one at a time).
//!
//! Regression: a background checkpoint only starts when no transaction is
//! open. With writers in tight loops some transaction is almost always open,
//! so checkpoints never start and writes fail with "WAL buffer full".

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use std::sync::Arc;
use std::thread;

const WAL_SIZE: usize = 64 * 1024;
const WRITERS: usize = 4;
const COMMITS_PER_WRITER: usize = 1000;

fn options(background: bool, mvcc: bool) -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .wal_size(WAL_SIZE)
    .auto_checkpoint(true)
    .checkpoint_threshold(0.5)
    .background_checkpoint(background)
}

fn assert_all_nodes_present(db: &kitedb::core::single_file::SingleFileDB) {
  for writer in 0..WRITERS {
    for index in [0, COMMITS_PER_WRITER / 2, COMMITS_PER_WRITER - 1] {
      let key = format!("w{writer}:{index}");
      assert!(db.node_by_key(&key).is_some(), "missing {key}");
    }
  }
}

fn concurrent_writers(background: bool, mvcc: bool) {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("concurrent-writers.kitedb");
  let db = Arc::new(open_single_file(&path, options(background, mvcc)).expect("open"));

  let writers: Vec<_> = (0..WRITERS)
    .map(|writer| {
      let db = Arc::clone(&db);
      thread::spawn(move || {
        for index in 0..COMMITS_PER_WRITER {
          db.begin(false)
            .unwrap_or_else(|error| panic!("writer {writer} begin #{index} failed: {error}"));
          db.create_node(Some(&format!("w{writer}:{index}")))
            .unwrap_or_else(|error| panic!("writer {writer} create_node #{index} failed: {error}"));
          db.commit()
            .unwrap_or_else(|error| panic!("writer {writer} commit #{index} failed: {error}"));
        }
      })
    })
    .collect();
  for writer in writers {
    writer.join().expect("writer thread panicked");
  }

  assert_all_nodes_present(&db);
  let db = Arc::try_unwrap(db)
    .ok()
    .expect("sole owner of the database");
  close_single_file(db).expect("close");

  let reopened = open_single_file(&path, options(background, mvcc)).expect("reopen");
  assert_all_nodes_present(&reopened);
  close_single_file(reopened).expect("close reopened");
}

#[test]
fn concurrent_writers_with_background_checkpoints_keep_the_wal_from_filling() {
  concurrent_writers(true, true);
}

#[test]
fn concurrent_writers_with_blocking_checkpoints_keep_the_wal_from_filling() {
  concurrent_writers(false, true);
}

#[test]
fn non_mvcc_writers_with_background_checkpoints_keep_the_wal_from_filling() {
  concurrent_writers(true, false);
}

#[test]
fn non_mvcc_writers_with_blocking_checkpoints_keep_the_wal_from_filling() {
  concurrent_writers(false, false);
}
