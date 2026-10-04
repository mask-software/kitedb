//! raydb-b4 `checkpoint-cost` lane: the heap a checkpoint needs.
//!
//! A bulk load of 1M nodes / 10M edges peaked at 11.6 GB (8 GB with a 64 MB
//! WAL) for a database of a few hundred MB: each checkpoint copied every
//! node and edge of the graph into its own heap objects (a hash map of
//! properties per edge, another per node), then built the new snapshot from
//! those copies, so a checkpoint's peak heap was several times the
//! snapshot's size.
//!
//! This binary has a global allocator that tracks live and peak heap bytes
//! across all threads, so it holds a single test: nothing may allocate
//! beside the checkpoint it measures.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::types::{EdgeWithProps, NodeId, PropValue};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Tracking;

fn grew(bytes: usize) {
  let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
  PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: forwards every call to `System`.
unsafe impl GlobalAlloc for Tracking {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    let ptr = System.alloc(layout);
    if !ptr.is_null() {
      grew(layout.size());
    }
    ptr
  }

  unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
    let ptr = System.alloc_zeroed(layout);
    if !ptr.is_null() {
      grew(layout.size());
    }
    ptr
  }

  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    System.dealloc(ptr, layout);
    LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
  }

  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    let moved = System.realloc(ptr, layout, new_size);
    if !moved.is_null() {
      if new_size >= layout.size() {
        grew(new_size - layout.size());
      } else {
        LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
      }
    }
    moved
  }
}

#[global_allocator]
static GLOBAL: Tracking = Tracking;

/// Heap bytes `f` needed at its peak, beyond what was live before it.
fn peak_heap_of(f: impl FnOnce()) -> usize {
  let before = LIVE.load(Ordering::Relaxed);
  PEAK.store(before, Ordering::Relaxed);
  f();
  PEAK.load(Ordering::Relaxed).saturating_sub(before)
}

const NODES: usize = 2_000;
const EDGES: usize = 50_000;
/// What a checkpoint may need at its peak, per edge, for this graph: its
/// uncompressed snapshot is about 60 bytes an edge (CSR in both directions,
/// one property), and a checkpoint holds the new snapshot's sections, its
/// file image, and the inflated copy it installs.
const PEAK_BYTES_PER_EDGE: usize = 200;

/// A checkpoint's peak heap grows with the snapshot it writes, at a few
/// times its size: from the delta (the first checkpoint after a bulk load)
/// and from the previous snapshot (every later one).
#[test]
fn checkpoint_peak_heap_is_a_few_times_the_snapshot() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-memory.kitedb");
  let db =
    open_single_file(&path, SingleFileOpenOptions::new().auto_checkpoint(false)).expect("open");

  db.begin(false).expect("begin schema");
  let person = db.define_label("Person").expect("label");
  let name = db.define_propkey("name").expect("name");
  let age = db.define_propkey("age").expect("age");
  let since = db.define_propkey("since").expect("since");
  let knows = db.define_etype("KNOWS").expect("etype");
  db.commit().expect("commit schema");

  let mut nodes: Vec<NodeId> = Vec::with_capacity(NODES);
  for start in (0..NODES).step_by(1_000) {
    let keys: Vec<String> = (start..start + 1_000).map(|i| format!("p{i}")).collect();
    let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
    db.begin_bulk().expect("begin bulk");
    let created = db.create_nodes_batch(&key_refs).expect("nodes");
    for (offset, &id) in created.iter().enumerate() {
      let index = start + offset;
      db.add_node_label(id, person).expect("label");
      db.set_node_prop(id, name, PropValue::String(format!("name-{index}")))
        .expect("name");
      db.set_node_prop(id, age, PropValue::I64((index % 90) as i64))
        .expect("age");
    }
    db.commit().expect("commit nodes");
    nodes.extend(created);
  }
  let mut state = 0x9e37_79b9_7f4a_7c15u64;
  for start in (0..EDGES).step_by(5_000) {
    let rows: Vec<EdgeWithProps> = (start..start + 5_000)
      .map(|index| {
        state = state
          .wrapping_mul(6_364_136_223_846_793_005)
          .wrapping_add(1_442_695_040_888_963_407);
        let src = nodes[(state >> 33) as usize % NODES];
        let dst = nodes[(state >> 13) as usize % NODES];
        (src, knows, dst, vec![(since, PropValue::I64(index as i64))])
      })
      .collect();
    db.begin_bulk().expect("begin bulk");
    db.add_edges_with_props_batch(rows).expect("edges");
    db.commit().expect("commit edges");
  }

  let from_delta = peak_heap_of(|| db.checkpoint().expect("checkpoint from the delta"));
  db.begin(false).expect("begin");
  db.create_node(Some("one-more")).expect("node");
  db.commit().expect("commit");
  let from_snapshot = peak_heap_of(|| db.checkpoint().expect("checkpoint from the snapshot"));
  close_single_file(db).expect("close");

  let bound = PEAK_BYTES_PER_EDGE * EDGES;
  assert!(
    from_delta <= bound && from_snapshot <= bound,
    "checkpoint peak heap: {from_delta} bytes from the delta ({} per edge), {from_snapshot} from \
     the snapshot ({} per edge); at most {PEAK_BYTES_PER_EDGE} per edge expected",
    from_delta / EDGES,
    from_snapshot / EDGES
  );
}
