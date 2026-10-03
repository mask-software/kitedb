//! raydb-b4 `pipeline` lane: what a small commit costs its committer and the
//! commit group that writes it, in heap allocations.
//!
//! With several writers, a 1-node commit spent much of its time in the system
//! allocator: about 27 allocations per commit, several of them freed by a
//! thread other than the one that allocated them (a commit group's leader
//! freed its members' record buffers and key groups, and handed every
//! member's released key sets to one committer), which macOS's allocator
//! makes slow for every thread. These pin the allocation counts down.
//!
//! This test binary counts allocations with its own global allocator
//! (`support/counting_alloc.rs`).

use std::sync::{Arc, Barrier};

use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};

#[path = "support/counting_alloc.rs"]
mod counting_alloc;

use counting_alloc::counted;

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

fn keys(prefix: &str, count: usize) -> Vec<String> {
  (0..count).map(|i| format!("{prefix}-{i}")).collect()
}

/// A 1-node commit on one thread, after a warm-up, allocates a handful of
/// blocks: its node's key (in the transaction's writes, its pending delta
/// and its key index), its WAL record's payload and its set of written keys.
/// It allocated about 27 before.
#[test]
fn b4_pl_small_commit_allocates_little() {
  const WARM: usize = 300;
  const COMMITS: usize = 2000;
  let dir = tempfile::tempdir().expect("tempdir");
  let db = open_single_file(dir.path().join("allocs.kitedb"), options()).expect("open");
  let commit = |key: &str| {
    db.begin(false).expect("begin");
    db.create_node(Some(key)).expect("create");
    db.commit().expect("commit");
  };
  for key in keys("warm", WARM) {
    commit(&key);
  }
  let keys = keys("measured", COMMITS);
  let ((), allocations, _) = counted(|| {
    for key in &keys {
      commit(key);
    }
  });
  let per_commit = allocations as f64 / COMMITS as f64;
  println!("1-node commit: {per_commit:.2} allocations");
  assert!(
    per_commit <= 8.0,
    "a 1-node commit allocated {per_commit:.2} times (at most 8 expected)"
  );
}

/// Committers on several threads, whose commits are written in groups by
/// one of them: a commit's memory is mostly freed by the thread that
/// allocated it. Each commit's written keys still go back to whichever
/// committer's commit releases them (about two blocks per commit).
#[test]
fn b4_pl_group_commits_free_memory_on_the_allocating_thread() {
  const THREADS: usize = 4;
  const COMMITS: usize = 1500;
  let dir = tempfile::tempdir().expect("tempdir");
  let db = Arc::new(open_single_file(dir.path().join("frees.kitedb"), options()).expect("open"));
  let start = Arc::new(Barrier::new(THREADS));
  let writers: Vec<_> = (0..THREADS)
    .map(|writer| {
      let (db, start) = (Arc::clone(&db), Arc::clone(&start));
      std::thread::spawn(move || {
        let keys = keys(&format!("w{writer}"), COMMITS);
        start.wait();
        let ((), allocations, foreign_frees) = counted(|| {
          for key in &keys {
            db.begin(false).expect("begin");
            db.create_node(Some(key)).expect("create");
            db.commit().expect("commit");
          }
        });
        (allocations, foreign_frees)
      })
    })
    .collect();
  let (mut allocations, mut foreign_frees) = (0, 0);
  for writer in writers {
    let (a, f) = writer.join().expect("writer");
    allocations += a;
    foreign_frees += f;
  }
  let commits = (THREADS * COMMITS) as f64;
  let (allocations, foreign_frees) = (allocations as f64 / commits, foreign_frees as f64 / commits);
  println!(
    "{THREADS} writers: {allocations:.2} allocations and {foreign_frees:.2} frees of another \
     thread's blocks per commit"
  );
  assert!(
    allocations <= 10.0,
    "a 1-node commit beside other writers allocated {allocations:.2} times (at most 10 expected)"
  );
  assert!(
    foreign_frees <= 2.5,
    "a 1-node commit beside other writers freed {foreign_frees:.2} blocks another thread \
     allocated (at most 2.5 expected)"
  );
}
