//! raydb-b4 `write-costs` lane: what MVCC adds to a small write transaction
//! committed with no other transaction open, in heap allocations.
//!
//! A single writer's small MVCC commits ran 15-18% slower than the same
//! commits without MVCC. Much of the difference was the allocator: each
//! transaction allocated fresh sets for the keys it read and wrote, grew them
//! one rehash at a time, and freed them (and the list MVCC released them in)
//! after its commit. These pin down what MVCC may add.
//!
//! This test binary counts allocations with its own global allocator
//! (`support/counting_alloc.rs`).

use kitedb::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode};
use kitedb::types::{ETypeId, NodeId, PropKeyId, PropValue};

#[path = "support/counting_alloc.rs"]
mod counting_alloc;

use counting_alloc::counted;

const WARM: usize = 300;
const COMMITS: usize = 2000;

struct Seeded {
  db: SingleFileDB,
  nodes: Vec<NodeId>,
  prop: PropKeyId,
  etype: ETypeId,
  _dir: tempfile::TempDir,
}

fn seeded(mvcc: bool) -> Seeded {
  let dir = tempfile::tempdir().expect("tempdir");
  let options = SingleFileOpenOptions::new()
    .mvcc(mvcc)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false);
  let db = open_single_file(dir.path().join("costs.kitedb"), options).expect("open");
  db.begin(false).expect("begin");
  let prop = db.define_propkey("p").expect("propkey");
  let etype = db.define_etype("LINK").expect("etype");
  let nodes = (0..64)
    .map(|i| {
      let node = db.create_node(Some(&format!("n{i}"))).expect("node");
      db.set_node_prop(node, prop, PropValue::I64(i))
        .expect("prop");
      node
    })
    .collect();
  db.commit().expect("commit");
  Seeded {
    db,
    nodes,
    prop,
    etype,
    _dir: dir,
  }
}

/// Allocations per commit of `commit(seeded, i)`, after a warm-up.
fn allocations_per_commit(mvcc: bool, commit: impl Fn(&Seeded, usize)) -> f64 {
  let seeded = seeded(mvcc);
  for i in 0..WARM {
    commit(&seeded, i);
  }
  let ((), allocations, _) = counted(|| {
    for i in WARM..WARM + COMMITS {
      commit(&seeded, i);
    }
  });
  allocations as f64 / COMMITS as f64
}

/// Set one prop on an existing node.
fn update_prop(seeded: &Seeded, i: usize) {
  let db = &seeded.db;
  db.begin(false).expect("begin");
  let node = seeded.nodes[i % seeded.nodes.len()];
  db.set_node_prop(node, seeded.prop, PropValue::I64(i as i64))
    .expect("update prop");
  db.commit().expect("commit");
}

/// Create a keyed node, set one prop on it, and link it to an existing node.
fn insert(seeded: &Seeded, i: usize) {
  let db = &seeded.db;
  db.begin(false).expect("begin");
  let node = db.create_node(Some(&format!("w{i}"))).expect("insert node");
  db.set_node_prop(node, seeded.prop, PropValue::I64(i as i64))
    .expect("insert prop");
  let dst = seeded.nodes[i % seeded.nodes.len()];
  db.add_edge(node, seeded.etype, dst).expect("insert edge");
  db.commit().expect("commit");
}

/// An MVCC commit of one prop update, with no other transaction open,
/// allocates no more than the same commit without MVCC: the sets of keys it
/// read and wrote are reused from the thread's last transaction.
#[test]
fn b4_wc_small_mvcc_commit_allocates_as_much_as_without_mvcc() {
  let off = allocations_per_commit(false, update_prop);
  let on = allocations_per_commit(true, update_prop);
  println!("update_prop commit: {off:.2} allocations without MVCC, {on:.2} with");
  assert!(
    on <= off + 0.05,
    "an MVCC update_prop commit allocated {on:.2} times, {:.2} more than without MVCC ({off:.2})",
    on - off
  );
}

/// An MVCC commit that creates a keyed node, sets a prop on it and adds an
/// edge allocates no more than without MVCC either: its key sets, grown to
/// about ten keys, are reused.
#[test]
fn b4_wc_mvcc_insert_commit_allocates_as_much_as_without_mvcc() {
  let off = allocations_per_commit(false, insert);
  let on = allocations_per_commit(true, insert);
  println!("insert commit: {off:.2} allocations without MVCC, {on:.2} with");
  assert!(
    on <= off + 0.05,
    "an MVCC insert commit allocated {on:.2} times, {:.2} more than without MVCC ({off:.2})",
    on - off
  );
}
