//! raydb-b4 `mvcc` lane, finding 5: a transaction that begins while another
//! commit is being published must see that commit entirely or not at all, and
//! keep seeing the same state for its whole snapshot.
//!
//! The committer pauses at one of two hooks in `publish_commit`:
//! - before the merge (durable, no MVCC timestamp yet): a reader that begins
//!   there must not see the commit, also not after it merges;
//! - after the MVCC timestamp (inside the publish): a reader must not begin
//!   until the commit is merged, and then sees it.
//!
//! Each case runs for a node property change, a node creation and an edge
//! addition, with and without a long-lived reader open (with no other reader,
//! the commit records no version history).
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tempfile::tempdir;

/// How long the after-timestamp hook waits for a reader that should be
/// blocked from beginning; the reader proceeds once the publish ends.
const BLOCKED_READER_WAIT: Duration = Duration::from_millis(300);
const DEADLINE: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug)]
enum Change {
  NodeProp,
  NodeCreate,
  EdgeAdd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pause {
  BeforeMerge,
  AfterTimestamp,
}

struct Graph {
  a: NodeId,
  b: NodeId,
  prop: PropKeyId,
  etype: ETypeId,
}

/// What a reader sees of the state `Change` touches.
#[derive(Debug, Clone, PartialEq)]
struct View {
  prop: Option<PropValue>,
  new_by_key: Option<NodeId>,
  node_count: usize,
  out_edges: Vec<(ETypeId, NodeId)>,
}

fn view(db: &SingleFileDB, graph: &Graph) -> View {
  View {
    prop: db.node_prop(graph.a, graph.prop),
    new_by_key: db.node_by_key("new"),
    node_count: db.list_nodes().len(),
    out_edges: db.out_edges(graph.a),
  }
}

fn seed(db: &SingleFileDB) -> Graph {
  db.begin(false).expect("begin seed");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  let prop = db.define_propkey("p").expect("propkey");
  let etype = db.define_etype("T").expect("etype");
  db.set_node_prop(a, prop, PropValue::I64(0)).expect("prop");
  db.commit().expect("commit seed");
  Graph { a, b, prop, etype }
}

fn apply(db: &SingleFileDB, graph: &Graph, change: Change) {
  match change {
    Change::NodeProp => db
      .set_node_prop(graph.a, graph.prop, PropValue::I64(1))
      .expect("set prop"),
    Change::NodeCreate => {
      db.create_node(Some("new")).expect("create node");
    }
    Change::EdgeAdd => db
      .add_edge(graph.a, graph.etype, graph.b)
      .expect("add edge"),
  }
}

/// Holds a read transaction open on its own thread until dropped.
struct OpenReader {
  release: Option<mpsc::Sender<()>>,
  handle: Option<std::thread::JoinHandle<()>>,
}

impl OpenReader {
  fn open(db: &Arc<SingleFileDB>) -> Self {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let db = Arc::clone(db);
    let handle = std::thread::spawn(move || {
      db.begin(true).expect("begin long-lived reader");
      ready_tx.send(()).expect("reader ready");
      let _ = release_rx.recv();
      db.rollback().expect("end long-lived reader");
    });
    ready_rx.recv().expect("long-lived reader began");
    Self {
      release: Some(release_tx),
      handle: Some(handle),
    }
  }
}

impl Drop for OpenReader {
  fn drop(&mut self) {
    drop(self.release.take());
    if let Some(handle) = self.handle.take() {
      let _ = handle.join();
    }
  }
}

/// Runs one case and returns what the reader saw before and after the
/// commit finished, plus the views before and after the commit.
fn run(change: Change, pause: Pause, long_lived_reader: bool) -> [View; 4] {
  let dir = tempdir().expect("temp dir");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .mvcc(true);
  let db = Arc::new(open_single_file(dir.path().join("f5.kitedb"), options).expect("open"));
  let graph = Arc::new(seed(&db));
  let old = view(&db, &graph);
  let _long_lived = long_lived_reader.then(|| OpenReader::open(&db));

  // The reader begins when the committer's hook says so, reads, then reads
  // again once the commit has finished.
  let (begin_tx, begin_rx) = mpsc::channel::<()>();
  let (first_tx, first_rx) = mpsc::channel::<View>();
  let (again_tx, again_rx) = mpsc::channel::<()>();
  let reader = {
    let db = Arc::clone(&db);
    let graph = Arc::clone(&graph);
    std::thread::spawn(move || {
      begin_rx.recv().expect("begin signal");
      db.begin(true).expect("reader begin");
      first_tx.send(view(&db, &graph)).expect("send first view");
      again_rx.recv().expect("again signal");
      let second = view(&db, &graph);
      db.rollback().expect("reader end");
      second
    })
  };

  let (first_seen_tx, first_seen_rx) = mpsc::channel::<View>();
  let committer = {
    let db = Arc::clone(&db);
    let graph = Arc::clone(&graph);
    std::thread::spawn(move || {
      db.begin(false).expect("committer begin");
      apply(&db, &graph, change);
      let hook: Box<dyn FnOnce()> = Box::new(move || {
        begin_tx.send(()).expect("signal reader");
        let wait = match pause {
          Pause::BeforeMerge => DEADLINE,
          Pause::AfterTimestamp => BLOCKED_READER_WAIT,
        };
        if let Ok(first) = first_rx.recv_timeout(wait) {
          first_seen_tx.send(first).expect("forward first view");
        } else {
          // Blocked until the publish ends: forward the view once it comes.
          std::thread::spawn(move || {
            if let Ok(first) = first_rx.recv_timeout(DEADLINE) {
              let _ = first_seen_tx.send(first);
            }
          });
        }
      });
      match pause {
        Pause::BeforeMerge => BEFORE_NEXT_COMMIT_MERGE.with(|h| *h.borrow_mut() = Some(hook)),
        Pause::AfterTimestamp => AFTER_NEXT_COMMIT_TIMESTAMP.with(|h| *h.borrow_mut() = Some(hook)),
      }
      db.commit()
    })
  };
  committer
    .join()
    .expect("committer thread")
    .expect("committer commit");
  let first = first_seen_rx
    .recv_timeout(DEADLINE)
    .expect("reader never read");
  again_tx.send(()).expect("signal again");
  let second = reader.join().expect("reader thread");
  let new = view(&db, &graph);
  [old, first, second, new]
}

fn check(change: Change, pause: Pause, long_lived_reader: bool) {
  let [old, first, second, new] = run(change, pause, long_lived_reader);
  assert_ne!(old, new, "{change:?}: the commit changed nothing visible");
  let expected = match pause {
    Pause::BeforeMerge => &old,
    Pause::AfterTimestamp => &new,
  };
  assert_eq!(
    first, *expected,
    "{change:?} {pause:?} long_lived_reader={long_lived_reader}: first read of the reader \
     that began during the commit"
  );
  assert_eq!(
    second, first,
    "{change:?} {pause:?} long_lived_reader={long_lived_reader}: the reader's snapshot \
     changed when the commit finished"
  );
}

macro_rules! f5_cases {
  ($($name:ident: $change:ident, $pause:ident, $reader:expr;)+) => {
    $(
      #[test]
      fn $name() {
        check(Change::$change, Pause::$pause, $reader);
      }
    )+
  };
}

f5_cases! {
  f5_prop_before_merge: NodeProp, BeforeMerge, false;
  f5_prop_before_merge_with_reader: NodeProp, BeforeMerge, true;
  f5_prop_after_timestamp: NodeProp, AfterTimestamp, false;
  f5_prop_after_timestamp_with_reader: NodeProp, AfterTimestamp, true;
  f5_create_before_merge: NodeCreate, BeforeMerge, false;
  f5_create_before_merge_with_reader: NodeCreate, BeforeMerge, true;
  f5_create_after_timestamp: NodeCreate, AfterTimestamp, false;
  f5_create_after_timestamp_with_reader: NodeCreate, AfterTimestamp, true;
  f5_edge_before_merge: EdgeAdd, BeforeMerge, false;
  f5_edge_before_merge_with_reader: EdgeAdd, BeforeMerge, true;
  f5_edge_after_timestamp: EdgeAdd, AfterTimestamp, false;
  f5_edge_after_timestamp_with_reader: EdgeAdd, AfterTimestamp, true;
}
