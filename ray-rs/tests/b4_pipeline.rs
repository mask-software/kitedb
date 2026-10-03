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
//! This test binary counts allocations with its own global allocator: each
//! block carries the id of the thread that allocated it, so a free on
//! another thread can be told apart.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};

/// Blocks start this many bytes into their allocation; the word before the
/// block holds the allocating thread's id.
const HEADER: usize = 16;

static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);

thread_local! {
  static THREAD: u64 = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
  /// Allocations (and reallocations) this thread made while counting.
  static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
  /// Frees this thread made of blocks another thread allocated, while counting.
  static FOREIGN_FREES: Cell<u64> = const { Cell::new(0) };
  static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn thread_id() -> u64 {
  THREAD.try_with(|id| *id).unwrap_or(0)
}

fn counting() -> bool {
  COUNTING.try_with(Cell::get).unwrap_or(false)
}

fn bump(counter: &'static std::thread::LocalKey<Cell<u64>>) {
  let _ = counter.try_with(|count| count.set(count.get() + 1));
}

struct Counting;

impl Counting {
  fn padded(layout: Layout) -> (Layout, usize) {
    let offset = HEADER.max(layout.align());
    // SAFETY: the alignment is a power of two and the size cannot overflow
    // for any layout the program allocates.
    let padded = unsafe {
      Layout::from_size_align_unchecked(layout.size() + offset, layout.align().max(HEADER))
    };
    (padded, offset)
  }
}

// SAFETY: forwards to `System` with a header in front of each block.
unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    if counting() {
      bump(&ALLOCATIONS);
    }
    let (padded, offset) = Self::padded(layout);
    let base = System.alloc(padded);
    if base.is_null() {
      return base;
    }
    *(base.add(offset - 8) as *mut u64) = thread_id();
    base.add(offset)
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    let (padded, offset) = Self::padded(layout);
    if counting() && *(ptr.sub(8) as *const u64) != thread_id() {
      bump(&FOREIGN_FREES);
    }
    System.dealloc(ptr.sub(offset), padded);
  }

  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    if counting() {
      bump(&ALLOCATIONS);
    }
    let (padded, offset) = Self::padded(layout);
    let owner = *(ptr.sub(8) as *const u64);
    let base = System.realloc(ptr.sub(offset), padded, new_size + offset);
    if base.is_null() {
      return base;
    }
    *(base.add(offset - 8) as *mut u64) = owner;
    base.add(offset)
  }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// What `f` allocated on this thread, and the frees it made there of blocks
/// other threads allocated.
fn counted<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
  let before = (ALLOCATIONS.with(Cell::get), FOREIGN_FREES.with(Cell::get));
  COUNTING.with(|on| on.set(true));
  let result = f();
  COUNTING.with(|on| on.set(false));
  (
    result,
    ALLOCATIONS.with(Cell::get) - before.0,
    FOREIGN_FREES.with(Cell::get) - before.1,
  )
}

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
