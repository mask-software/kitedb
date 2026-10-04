//! raydb-b4 `checkpoint-cost` lane. Included from checkpoint.rs for its test
//! hooks (phase barriers, cut and snapshot-byte counters).
//!
//! Finding 1: a bulk load checkpoints every time the WAL fills, and every
//! checkpoint rebuilds the whole snapshot, so the load writes
//! O(data^2 / WAL) snapshot bytes.
//! Finding 2: a commit waits for a checkpoint: the commit that crosses the
//! auto-checkpoint threshold runs the whole checkpoint before it returns,
//! and a writer that fills the secondary WAL region waits for the install.
//! Finding 3: the file left by load + checkpoint + close depends on the WAL
//! size: the last snapshot can land after the dead pages of earlier ones.
use super::*;
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

/// The smallest WAL a database accepts (16 pages), standing in for the
/// default 4 MiB WAL against gigabytes of data.
const SMALL_WAL: usize = 64 * 1024;

#[derive(Clone, Copy)]
struct Schema {
  person: LabelId,
  name: PropKeyId,
  age: PropKeyId,
  since: PropKeyId,
  knows: ETypeId,
}

fn define_schema(db: &SingleFileDB) -> Schema {
  db.begin(false).expect("begin schema");
  let schema = Schema {
    person: db.define_label("Person").expect("label"),
    name: db.define_propkey("name").expect("name"),
    age: db.define_propkey("age").expect("age"),
    since: db.define_propkey("since").expect("since"),
    knows: db.define_etype("KNOWS").expect("etype"),
  };
  db.commit().expect("commit schema");
  schema
}

/// The benchmark's loader, scaled down: `nodes` keyed nodes with a label and
/// two properties, then `edges` edges with one property, `batch` rows per
/// bulk transaction. Edge endpoints come from a fixed LCG.
fn bulk_load(db: &SingleFileDB, nodes: usize, edges: usize, batch: usize) -> Vec<NodeId> {
  let schema = define_schema(db);
  let mut ids = Vec::with_capacity(nodes);
  for start in (0..nodes).step_by(batch) {
    let keys: Vec<String> = (start..(start + batch).min(nodes))
      .map(|index| format!("p{index}"))
      .collect();
    let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
    db.begin_bulk().expect("begin bulk nodes");
    let created = db.create_nodes_batch(&key_refs).expect("create nodes");
    for (offset, &id) in created.iter().enumerate() {
      let index = start + offset;
      db.add_node_label(id, schema.person).expect("label");
      db.set_node_prop(id, schema.name, PropValue::String(format!("name-{index}")))
        .expect("name");
      db.set_node_prop(id, schema.age, PropValue::I64((index % 90) as i64))
        .expect("age");
    }
    db.commit().expect("commit nodes");
    ids.extend(created);
  }

  let mut state = 0x2545_f491_4f6c_dd1du64;
  let mut next = || {
    state = state
      .wrapping_mul(6_364_136_223_846_793_005)
      .wrapping_add(1_442_695_040_888_963_407);
    (state >> 33) as usize
  };
  for start in (0..edges).step_by(batch) {
    let rows: Vec<EdgeWithProps> = (start..(start + batch).min(edges))
      .map(|index| {
        let src = ids[next() % nodes];
        let dst = ids[next() % nodes];
        (
          src,
          schema.knows,
          dst,
          vec![(schema.since, PropValue::I64(index as i64))],
        )
      })
      .collect();
    db.begin_bulk().expect("begin bulk edges");
    db.add_edges_with_props_batch(rows).expect("add edges");
    db.commit().expect("commit edges");
  }
  ids
}

fn snapshot_bytes(db: &SingleFileDB) -> u64 {
  let header = db.header.read();
  header.snapshot_page_count * header.page_size as u64
}

fn commit_nodes(db: &SingleFileDB, prefix: &str, count: usize) {
  db.begin(false).expect("begin");
  for index in 0..count {
    db.create_node(Some(&format!("{prefix}-{index}")))
      .expect("create node");
  }
  db.commit().expect("commit");
}

/// Finding 1. Loading data writes snapshot pages in proportion to the data,
/// not to the data times the number of times the WAL filled: with the WAL
/// small next to the data (as the default 4 MiB WAL is next to a 10M-edge
/// load), the load must not rewrite the whole database at every fill.
#[test]
fn bulk_load_writes_snapshot_bytes_linear_in_the_data() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("bulk-load-linear.kitedb");
  let db = open_single_file(&path, SingleFileOpenOptions::new().wal_size(SMALL_WAL)).expect("open");
  bulk_load(&db, 2_000, 20_000, 100);
  db.checkpoint().expect("final checkpoint");

  let written = checkpoint_test_snapshot_bytes(&db);
  let final_bytes = snapshot_bytes(&db);
  let cuts = checkpoint_test_cuts(&db);
  close_single_file(db).expect("close");
  assert!(
    written <= 8 * final_bytes,
    "the load wrote {written} snapshot bytes ({:.1}x the final {final_bytes}-byte snapshot) over \
     {cuts} background checkpoints: every WAL fill rewrote the whole database",
    written as f64 / final_bytes as f64
  );
}

/// Finding 2, the committer: the commit that crosses the auto-checkpoint
/// threshold returns without waiting for the checkpoint it starts. The
/// checkpoint is held after its cut; the commit must come back meanwhile.
#[test]
fn commit_that_starts_an_auto_checkpoint_returns_before_the_checkpoint_finishes() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("commit-not-held-by-auto-checkpoint.kitedb");
  let db = Arc::new(
    open_single_file(&path, SingleFileOpenOptions::new().wal_size(SMALL_WAL)).expect("open"),
  );
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&held));

  let (returned, returns) = mpsc::channel();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      let mut commits = 0;
      // Small commits until one starts a checkpoint (it cuts first).
      while checkpoint_test_cuts(&db) == 0 && !db.is_checkpoint_running() {
        commit_nodes(&db, &format!("c{commits}"), 4);
        commits += 1;
      }
      let _ = returned.send(commits);
    })
  };
  let outcome = returns.recv_timeout(Duration::from_secs(2));
  // Let the checkpoint go on, whoever runs it.
  held.wait();
  writer.join().expect("writer");
  while db.is_checkpoint_running() {
    std::thread::yield_now();
  }
  assert!(
    outcome.is_ok(),
    "the commit that crossed the auto-checkpoint threshold did not return while its checkpoint \
     was held after the cut: it runs the whole checkpoint (snapshot build, write, install) \
     before returning"
  );
  let commits = outcome.unwrap();
  for index in 0..commits {
    assert!(db.node_by_key(&format!("c{index}-0")).is_some());
  }
}

/// Finding 2, the other writers: while a background checkpoint builds its
/// snapshot, a writer keeps committing however much it writes. Today the
/// writer waits for the install once its records fill the secondary WAL
/// region (a quarter of the WAL), which at 1M nodes / 10M edges stalls
/// commits for seconds. The checkpoint is held after its cut; the writer
/// commits twice the whole WAL's size meanwhile.
#[test]
fn writer_is_not_held_up_by_a_checkpoint_building_its_snapshot() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("writer-not-held-by-checkpoint.kitedb");
  let db = Arc::new(
    open_single_file(
      &path,
      SingleFileOpenOptions::new()
        .wal_size(SMALL_WAL)
        .auto_checkpoint(false),
    )
    .expect("open"),
  );
  commit_nodes(&db, "pre", 50);
  // A writer never cancels this cut as stalled while the test holds it.
  set_checkpoint_test_stall_timeout(&db, Duration::from_secs(120));
  let held = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&held));
  let checkpoint = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || db.background_checkpoint())
  };
  let deadline = Instant::now() + Duration::from_secs(5);
  while db.checkpoint_state.lock().cut_owner.is_none() {
    assert!(Instant::now() < deadline, "the checkpoint never cut");
    std::thread::yield_now();
  }

  // Each node's records take about 100 bytes, so this is twice the WAL.
  let commits = 2 * db.wal_stats().capacity as usize / (20 * 100);
  let (done, finished) = mpsc::channel();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      for commit in 0..commits {
        commit_nodes(&db, &format!("w{commit}"), 20);
      }
      let _ = done.send(());
    })
  };
  let outcome = finished.recv_timeout(Duration::from_secs(3));
  held.wait();
  checkpoint
    .join()
    .expect("checkpoint thread")
    .expect("checkpoint");
  writer.join().expect("writer");
  assert!(
    outcome.is_ok(),
    "a writer committing twice the WAL's size waited for a background checkpoint held after its \
     cut: once the secondary WAL region fills, every commit waits for the install"
  );
  for index in 0..commits {
    assert!(db.node_by_key(&format!("w{index}-19")).is_some());
  }
}

/// The file after load + checkpoint + close, and how much of it is neither
/// the header pages, the WAL area nor the snapshot.
struct FileLayout {
  store_bytes: u64,
  dead_pages: u64,
  snapshot_pages: u64,
}

fn load_checkpoint_close(path: &std::path::Path, wal_size: usize) -> FileLayout {
  let db = open_single_file(path, SingleFileOpenOptions::new().wal_size(wal_size)).expect("open");
  bulk_load(&db, 1_500, 15_000, 100);
  db.checkpoint().expect("checkpoint");
  let (page_size, wal_pages, snapshot_pages) = {
    let header = db.header.read();
    (
      header.page_size as u64,
      header.wal_page_count,
      header.snapshot_page_count,
    )
  };
  close_single_file(db).expect("close");
  let file_bytes = std::fs::metadata(path).expect("metadata").len();
  let file_pages = file_bytes.div_ceil(page_size);
  FileLayout {
    store_bytes: file_bytes - wal_pages * page_size,
    dead_pages: file_pages - (2 + wal_pages + snapshot_pages),
    snapshot_pages,
  }
}

/// Finding 3. The same data, loaded, checkpointed and closed with different
/// WAL sizes, leaves files that differ only by the WAL area: no dead pages
/// (old snapshots) stay in front of the last one.
#[test]
fn file_size_after_load_checkpoint_close_does_not_depend_on_wal_size() {
  let dir = tempdir().expect("tempdir");
  let layouts: Vec<(usize, FileLayout)> = [SMALL_WAL, 4 * SMALL_WAL, 16 * SMALL_WAL]
    .into_iter()
    .map(|wal_size| {
      let path = dir.path().join(format!("file-size-wal-{wal_size}.kitedb"));
      (wal_size, load_checkpoint_close(&path, wal_size))
    })
    .collect();
  let report: Vec<String> = layouts
    .iter()
    .map(|(wal_size, layout)| {
      format!(
        "WAL {wal_size}: store {} bytes, snapshot {} pages, {} dead pages",
        layout.store_bytes, layout.snapshot_pages, layout.dead_pages
      )
    })
    .collect();
  for (_, layout) in &layouts {
    assert_eq!(
      layout.dead_pages, 0,
      "dead pages left after load + checkpoint + close: {report:?}"
    );
  }
  let snapshot_pages = layouts[0].1.snapshot_pages;
  for (_, layout) in &layouts {
    assert_eq!(
      layout.snapshot_pages, snapshot_pages,
      "same data, different snapshots: {report:?}"
    );
  }
}

/// Finding 3, across processes: dead pages a run left in front of the
/// snapshot (here: auto-checkpoints ending with the snapshot at the end of
/// the file) do not stay in the file for good once a later process
/// checkpoints and closes.
#[test]
fn checkpoint_after_reopen_reclaims_dead_pages() {
  let dir = tempdir().expect("tempdir");
  // Find a load whose last auto-checkpoint leaves dead pages in front of the
  // snapshot; the loads differ only in size.
  for nodes in (600..=1_400).step_by(100) {
    let path = dir.path().join(format!("reopen-reclaim-{nodes}.kitedb"));
    let options = SingleFileOpenOptions::new().wal_size(SMALL_WAL);
    let db = open_single_file(&path, options.clone()).expect("open");
    bulk_load(&db, nodes, 10 * nodes, 100);
    let dead_before = {
      let header = db.header.read();
      header.snapshot_start_page - (header.wal_start_page + header.wal_page_count)
    };
    close_single_file(db).expect("close");
    if dead_before == 0 {
      continue;
    }

    let db = open_single_file(&path, options).expect("reopen");
    commit_nodes(&db, "after-reopen", 1);
    db.checkpoint().expect("checkpoint after reopen");
    let (page_size, live_pages) = {
      let header = db.header.read();
      (
        header.page_size as u64,
        2 + header.wal_page_count + header.snapshot_page_count,
      )
    };
    close_single_file(db).expect("close");
    let file_pages = std::fs::metadata(&path)
      .expect("metadata")
      .len()
      .div_ceil(page_size);
    assert_eq!(
      file_pages,
      live_pages,
      "{} dead pages after a checkpoint in a reopened database ({dead_before} before)",
      file_pages - live_pages
    );
    return;
  }
  panic!("no load left dead pages in front of its snapshot; adjust the sizes");
}

/// Open puts the pages no header slot names on the free list, but holds back
/// those the other slot names: after a crash between an install's two header
/// writes, that slot is the fallback should the newest one be lost, so the
/// next checkpoint must not write over the snapshot it names.
#[test]
fn reopen_after_a_crash_mid_install_keeps_the_fallback_snapshot() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("crash-mid-install.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  // A large snapshot, then a small one: its install fails after the first
  // header slot names it, so the slots name different snapshots.
  db.begin(false).expect("begin");
  let ids: Vec<NodeId> = (0..2_000)
    .map(|index| db.create_node(Some(&format!("n{index}"))).expect("node"))
    .collect();
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint");
  db.begin(false).expect("begin");
  for &id in &ids[10..] {
    db.delete_node(id).expect("delete");
  }
  db.commit().expect("commit");
  set_checkpoint_test_fault(Some(CheckpointPhase::HeaderDurable));
  assert!(db.checkpoint().is_err(), "the injected fault did not fire");
  set_checkpoint_test_fault(None);

  // The crash: the file as it is now.
  let copy = dir.path().join("crash-mid-install-copy.kitedb");
  std::fs::copy(&path, &copy).expect("copy");
  drop(db);
  let db = open_single_file(&copy, options).expect("open the crash copy");
  let page_size = db.header.read().page_size as usize;
  let bytes = std::fs::read(&copy).expect("read");
  let slots: Vec<DbHeaderV1> = (0..2)
    .map(|slot| DbHeaderV1::parse(&bytes[slot * page_size..(slot + 1) * page_size]).expect("slot"))
    .collect();
  let selected = db.header.read().clone();
  let fallback = slots
    .iter()
    .find(|slot| slot.snapshot_start_page != selected.snapshot_start_page)
    .expect("the slots name different snapshots")
    .clone();

  commit_nodes(&db, "after-crash", 1);
  db.checkpoint().expect("checkpoint after the crash");
  let written = db.header.read().clone();
  let fallback_pages =
    fallback.snapshot_start_page..fallback.snapshot_start_page + fallback.snapshot_page_count;
  assert!(
    written.snapshot_start_page + written.snapshot_page_count <= fallback_pages.start
      || written.snapshot_start_page >= fallback_pages.end,
    "the checkpoint wrote its snapshot (pages {}..{}) over the one the fallback header slot \
     names (pages {fallback_pages:?})",
    written.snapshot_start_page,
    written.snapshot_start_page + written.snapshot_page_count
  );
  assert!(db.node_by_key("n0").is_some() && db.node_by_key("n10").is_none());
  assert!(db.node_by_key("after-crash-0").is_some());
}

/// Everything reads can see of the graph: per node, its key, labels and
/// properties, and its out-edges with their properties, sorted.
type GraphImage = Vec<(
  NodeId,
  Option<String>,
  Vec<LabelId>,
  Vec<(PropKeyId, String)>,
  Vec<(ETypeId, NodeId, Vec<(PropKeyId, String)>)>,
)>;

fn graph_image(db: &SingleFileDB) -> GraphImage {
  let sorted_props = |props: Option<HashMap<PropKeyId, PropValue>>| {
    let mut props: Vec<(PropKeyId, String)> = props
      .unwrap_or_default()
      .into_iter()
      .map(|(key, value)| (key, format!("{value:?}")))
      .collect();
    props.sort();
    props
  };
  let mut nodes = db.list_nodes();
  nodes.sort_unstable();
  nodes
    .into_iter()
    .map(|node| {
      let mut labels = db.node_labels(node);
      labels.sort_unstable();
      let mut edges: Vec<_> = db
        .out_edges(node)
        .into_iter()
        .map(|(etype, dst)| (etype, dst, sorted_props(db.edge_props(node, etype, dst))))
        .collect();
      edges.sort();
      (
        node,
        db.node_key(node),
        labels,
        sorted_props(db.node_props(node)),
        edges,
      )
    })
    .collect()
}

/// The checkpoint's graph collection works on edges as columns (one
/// allocation per column rather than a property map per edge): a
/// checkpoint must still keep exactly what reads saw, with snapshot edges
/// whose properties the delta changed, removed or added, deleted edges and
/// nodes, and new edges on old and new nodes; and so must a reopen.
#[test]
fn checkpoint_keeps_every_edge_and_property_as_read_before_it() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("checkpoint-keeps-graph.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = open_single_file(&path, options.clone()).expect("open");
  let ids = bulk_load(&db, 200, 2_000, 100);
  db.begin(false).expect("begin");
  let weight = db.define_propkey("weight").expect("propkey");
  let since = db.propkey_id("since").expect("since");
  let knows = db.etype_id("KNOWS").expect("etype");
  let likes = db.define_etype("LIKES").expect("etype");
  db.commit().expect("commit");
  db.checkpoint().expect("first checkpoint");

  db.begin(false).expect("begin");
  let edges: Vec<(NodeId, NodeId)> = ids
    .iter()
    .take(40)
    .filter_map(|&src| db.out_edges(src).first().map(|&(_, dst)| (src, dst)))
    .collect();
  for (index, &(src, dst)) in edges.iter().enumerate() {
    match index % 5 {
      0 => db
        .set_edge_prop(src, knows, dst, since, PropValue::I64(-1))
        .expect("change prop"),
      1 => db
        .delete_edge_prop(src, knows, dst, since)
        .expect("remove prop"),
      2 => db
        .set_edge_prop(
          src,
          knows,
          dst,
          weight,
          PropValue::String(format!("w{index}")),
        )
        .expect("add prop"),
      3 => db.delete_edge(src, knows, dst).expect("delete edge"),
      _ => db
        .add_edge_with_props(src, likes, dst, vec![(weight, PropValue::F64(0.5))])
        .expect("new edge on old nodes"),
    }
  }
  db.delete_node(ids[150]).expect("delete node");
  let fresh = db.create_node(Some("fresh")).expect("node");
  db.add_edge_with_props(fresh, likes, ids[0], vec![(since, PropValue::I64(7))])
    .expect("edge from a new node");
  db.add_edge(ids[1], likes, fresh)
    .expect("edge to a new node");
  db.commit().expect("commit");

  let before = graph_image(&db);
  db.checkpoint().expect("checkpoint");
  assert!(
    graph_image(&db) == before,
    "the checkpoint changed the graph"
  );
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options).expect("reopen");
  assert!(
    graph_image(&reopened) == before,
    "the reopened graph differs"
  );
}
