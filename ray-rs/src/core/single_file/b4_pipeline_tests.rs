//! raydb-b4 `pipeline` lane: how the locks every read takes lie in memory.
//! Included from mod.rs, for the database's fields.

use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use parking_lot::RwLock;
use tempfile::tempdir;

/// Apple silicon's cache line (two of x86-64's).
const LINE: usize = 128;

/// Where a lock lies, and where the data it guards starts.
fn lock_and_data<T>(lock: &RwLock<T>) -> (usize, usize) {
  let guard = lock.read();
  (
    lock as *const RwLock<T> as usize,
    &*guard as *const T as usize,
  )
}

/// Every read takes the delta's and the snapshot's read locks, and a read
/// lock counts its readers in the lock word: each reader writes that word.
/// The data a lock guards (which readers read next), and the database's
/// other fields, must lie on other cache lines, or each reader's lock makes
/// every other reader miss them. With them on the lock's line, 8 reader
/// threads read 1.6-1.9 times slower (`mvcc_overhead_bench`, reads), and by
/// how much depended on where the allocator placed the database.
#[test]
fn b4_pl_reader_lock_words_have_cache_lines_of_their_own() {
  let dir = tempdir().expect("tempdir");
  let db = open_single_file(
    dir.path().join("lines.kitedb"),
    SingleFileOpenOptions::new(),
  )
  .expect("open");
  let delta: &RwLock<_> = &db.delta;
  let snapshot: &RwLock<_> = &db.snapshot;
  for (name, (lock, data)) in [
    ("delta", lock_and_data(delta)),
    ("snapshot", lock_and_data(snapshot)),
  ] {
    assert_eq!(
      lock % LINE,
      0,
      "the {name} lock does not start a cache line: another field shares its line"
    );
    assert!(
      data >= lock + LINE,
      "the {name} lock's data starts {} bytes after its lock word, on its line",
      data - lock
    );
  }
}
