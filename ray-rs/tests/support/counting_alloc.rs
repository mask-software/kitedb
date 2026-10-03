//! A global allocator for test binaries that counts heap allocations per
//! thread, and frees of blocks another thread allocated. Include it with
//! `#[path = "support/counting_alloc.rs"] mod counting_alloc;`: it installs
//! itself as the binary's global allocator.
//!
//! Each block carries the id of the thread that allocated it, so a free on
//! another thread can be told apart.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

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
pub fn counted<R>(f: impl FnOnce() -> R) -> (R, u64, u64) {
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
