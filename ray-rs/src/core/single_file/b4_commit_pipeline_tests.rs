//! raydb-b4 `commit-pipeline` lane: concurrent MVCC commits (finding 1).
//! Included from transaction.rs for private access.
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;

use crate::core::single_file::{open_single_file, SingleFileOpenOptions, SyncMode};
use crate::types::{PropValue, TxKey};

/// How long writes get while the test holds a lock they must not need. They
/// take milliseconds; only writes that wait for the lock run out of it.
const DEADLINE: Duration = Duration::from_secs(20);

fn mvcc_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .mvcc(true)
    .mvcc_gc_interval_ms(10)
    .sync_mode(SyncMode::Normal)
    .auto_checkpoint(false)
}

/// With MVCC, every write notes what it wrote (and read) for the
/// transaction's conflict check at commit. The notes stay with the
/// transaction until then: a write must not take the transaction manager's
/// lock, which every begin and commit takes. (Each write took it, and a batch
/// write held it while inserting every key it wrote, so concurrent writers
/// stalled each other's commits, and commits stalled behind batch writes.)
#[test]
fn mvcc_writes_take_no_transaction_manager_lock() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("writes-no-mvcc-lock.kitedb");
  let db = Arc::new(open_single_file(&path, mvcc_options()).expect("open"));
  db.begin(false).expect("begin");
  let etype = db.define_etype("knows").expect("etype");
  let prop = db.define_propkey("weight").expect("propkey");
  let label = db.define_label("Person").expect("label");
  let base = db.create_node(Some("base")).expect("base");
  let other = db.create_node(Some("other")).expect("other");
  db.add_edge(base, etype, other).expect("edge");
  db.commit().expect("commit");

  let (begun_tx, begun_rx) = mpsc::channel();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (written_tx, written_rx) = mpsc::channel();
  let (commit_tx, commit_rx) = mpsc::channel::<()>();
  let writer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || {
      db.begin(false).expect("begin");
      begun_tx.send(()).expect("begun");
      go_rx.recv().expect("go");
      let nodes = db
        .create_nodes_batch(&[Some("a"), Some("b"), None])
        .expect("create batch");
      let single = db.create_node(Some("c")).expect("create");
      db.add_edges_batch(&[(nodes[0], etype, nodes[1])])
        .expect("add edges");
      db.add_edges_with_props_batch(vec![(
        nodes[1],
        etype,
        nodes[2],
        vec![(prop, PropValue::I64(1))],
      )])
      .expect("add edges with props");
      db.add_edge_with_props(nodes[2], etype, single, vec![(prop, PropValue::I64(2))])
        .expect("add edge with props");
      db.add_edge(single, etype, nodes[0]).expect("add edge");
      db.set_node_prop(base, prop, PropValue::I64(3))
        .expect("set node prop");
      db.delete_node_prop(nodes[0], prop)
        .expect("delete node prop");
      db.add_node_label(base, label).expect("add label");
      db.remove_node_label(nodes[1], label).expect("remove label");
      db.set_edge_prop(base, etype, other, prop, PropValue::I64(4))
        .expect("set edge prop");
      db.set_edge_props(base, etype, other, vec![(prop, PropValue::I64(5))])
        .expect("set edge props");
      db.delete_edge_prop(nodes[0], etype, nodes[1], prop)
        .expect("delete edge prop");
      db.delete_edge(single, etype, nodes[0])
        .expect("delete edge");
      db.delete_node(nodes[2]).expect("delete node");
      let _ = written_tx.send(());
      commit_rx.recv().expect("commit");
      db.commit()
    })
  };
  begun_rx.recv().expect("writer began");

  let mvcc = db.mvcc.as_ref().expect("mvcc");
  let held = mvcc.tx_manager.lock();
  go_tx.send(()).expect("go");
  let finished = written_rx.recv_timeout(DEADLINE).is_ok();
  drop(held);
  if !finished {
    // Let the writer finish, so the failure ends instead of hanging.
    let _ = written_rx.recv();
  }
  commit_tx.send(()).expect("commit");
  writer.join().expect("writer").expect("writer commit");

  assert!(
    finished,
    "writes did not finish within {DEADLINE:?} while the transaction manager's lock was held"
  );
  assert!(db.node_by_key("a").is_some() && db.node_by_key("c").is_some());
  assert_eq!(
    db.node_prop(base, prop),
    Some(PropValue::I64(3)),
    "the writes committed"
  );
}

/// The keys a write transaction notes still reach its conflict check: a
/// concurrent commit of what it wrote, or of what a write read, conflicts.
#[test]
fn mvcc_conflicts_on_noted_writes_and_reads_guard() {
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("noted-keys-conflict.kitedb");
  let db = Arc::new(open_single_file(&path, mvcc_options()).expect("open"));
  db.begin(false).expect("begin");
  let prop = db.define_propkey("count").expect("propkey");
  let node = db.create_node(Some("counter")).expect("node");
  let doomed = db.create_node(Some("doomed")).expect("doomed");
  db.commit().expect("commit");

  // Write-write: both set the same prop.
  // Read-write: a prop write reads its node, which the other deletes.
  for (first_key, second_target) in [
    (
      TxKey::NodeProp {
        node_id: node,
        key_id: prop,
      },
      node,
    ),
    (TxKey::Node(doomed), doomed),
  ] {
    let (staged_tx, staged_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let second = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || {
        db.begin(false).expect("begin");
        db.set_node_prop(second_target, prop, PropValue::I64(2))
          .expect("set");
        staged_tx.send(()).expect("staged");
        go_rx.recv().expect("go");
        db.commit()
      })
    };
    staged_rx.recv().expect("second staged");
    db.begin(false).expect("begin");
    match &first_key {
      TxKey::NodeProp { node_id, key_id } => db
        .set_node_prop(*node_id, *key_id, PropValue::I64(1))
        .expect("set"),
      TxKey::Node(node_id) => db.delete_node(*node_id).expect("delete"),
      _ => unreachable!(),
    }
    db.commit().expect("first commit");
    go_tx.send(()).expect("go");
    let result = second.join().expect("second");
    assert!(
      matches!(result, Err(crate::error::KiteError::Conflict { .. })),
      "the second commit must conflict on {first_key}: {result:?}"
    );
  }
}

// ============================================================================
// Guards for the reworked commit merge and conflict index
// ============================================================================

mod merge_model {
  use std::collections::{BTreeMap, BTreeSet};

  use rand::rngs::StdRng;
  use rand::{Rng, SeedableRng};

  use crate::types::{DeltaState, NodeId, PropValue};

  /// The merge as it was: every change applied one at a time.
  fn reference_merge(target: &mut DeltaState, mut pending: DeltaState) {
    target.new_labels.extend(pending.new_labels.drain());
    target.new_etypes.extend(pending.new_etypes.drain());
    target.new_propkeys.extend(pending.new_propkeys.drain());
    let removed: BTreeSet<NodeId> = pending
      .deleted_nodes
      .iter()
      .copied()
      .filter(|&node_id| pending.is_node_removed(node_id))
      .collect();
    for node_id in pending.deleted_nodes.drain() {
      target.delete_node(node_id);
    }
    for (node_id, mut node_delta) in pending.created_nodes.drain() {
      target.create_node(node_id, node_delta.key.as_deref());
      for label_id in node_delta.labels.take().into_iter().flatten() {
        target.add_node_label(node_id, label_id);
      }
      for label_id in node_delta.labels_deleted.take().into_iter().flatten() {
        target.remove_node_label(node_id, label_id);
      }
      for (key_id, value) in node_delta.props.take().into_iter().flatten() {
        match value {
          Some(value) => target.set_node_prop_ref(node_id, key_id, value),
          None => target.delete_node_prop(node_id, key_id),
        }
      }
    }
    for (node_id, mut node_delta) in pending.modified_nodes.drain() {
      for label_id in node_delta.labels.take().into_iter().flatten() {
        target.add_node_label(node_id, label_id);
      }
      for label_id in node_delta.labels_deleted.take().into_iter().flatten() {
        target.remove_node_label(node_id, label_id);
      }
      for (key_id, value) in node_delta.props.take().into_iter().flatten() {
        match value {
          Some(value) => target.set_node_prop_ref(node_id, key_id, value),
          None => target.delete_node_prop(node_id, key_id),
        }
      }
    }
    for (src, patches) in pending.out_add.drain() {
      for patch in patches {
        target.add_edge(src, patch.etype, patch.other);
      }
    }
    for (src, patches) in pending.out_del.drain() {
      for patch in patches {
        target.delete_edge(src, patch.etype, patch.other);
      }
    }
    for ((src, etype, dst), props) in pending.edge_props.drain() {
      if removed.contains(&src) || removed.contains(&dst) {
        continue;
      }
      for (key_id, value) in props {
        match value {
          Some(value) => target.set_edge_prop_ref(src, etype, dst, key_id, value),
          None => target.delete_edge_prop(src, etype, dst, key_id),
        }
      }
    }
    target.key_index.extend(pending.key_index.drain());
  }

  /// Every field of `delta`, ordered, with `None` and empty collections
  /// kept apart.
  fn canonical(delta: &DeltaState) -> String {
    fn node(delta: &crate::types::NodeDelta) -> String {
      let labels = |set: &Option<std::collections::HashSet<u32>>| {
        set
          .as_ref()
          .map(|set| set.iter().copied().collect::<BTreeSet<_>>())
      };
      let props = delta.props.as_ref().map(|props| {
        props
          .iter()
          .map(|(&key, value)| (key, format!("{value:?}")))
          .collect::<BTreeMap<_, _>>()
      });
      format!(
        "key={:?} labels={:?} deleted={:?} props={props:?}",
        delta.key,
        labels(&delta.labels),
        labels(&delta.labels_deleted)
      )
    }
    let nodes = |map: &mut dyn Iterator<Item = (&NodeId, &crate::types::NodeDelta)>| {
      map
        .map(|(&id, delta)| (id, node(delta)))
        .collect::<BTreeMap<_, _>>()
    };
    let patches = |map: &mut dyn Iterator<Item = (&NodeId, &BTreeSet<crate::types::EdgePatch>)>| {
      map
        .map(|(&id, set)| (id, set.clone()))
        .collect::<BTreeMap<_, _>>()
    };
    let edge_props = delta
      .edge_props
      .iter()
      .map(|(&edge, props)| {
        let props = props
          .iter()
          .map(|(&key, value)| (key, format!("{value:?}")))
          .collect::<BTreeMap<_, _>>();
        (edge, props)
      })
      .collect::<BTreeMap<_, _>>();
    format!(
      "created={:?}\ndeleted={:?}\nmodified={:?}\nout_add={:?}\nout_del={:?}\nin_add={:?}\n\
       in_del={:?}\nedge_props={edge_props:?}\nlabels={:?}\netypes={:?}\npropkeys={:?}\nkeys={:?}",
      nodes(&mut delta.created_nodes.iter()),
      delta.deleted_nodes.iter().collect::<BTreeSet<_>>(),
      nodes(&mut delta.modified_nodes.iter()),
      patches(&mut delta.out_add.iter()),
      patches(&mut delta.out_del.iter()),
      patches(&mut delta.in_add.iter()),
      patches(&mut delta.in_del.iter()),
      delta.new_labels.iter().collect::<BTreeMap<_, _>>(),
      delta.new_etypes.iter().collect::<BTreeMap<_, _>>(),
      delta.new_propkeys.iter().collect::<BTreeMap<_, _>>(),
      delta.key_index.iter().collect::<BTreeMap<_, _>>(),
    )
  }

  /// Random writes to `delta`, the way the write paths make them, over a
  /// few node ids, keys, types, props and labels (so they collide).
  fn random_writes(rng: &mut StdRng, delta: &mut DeltaState, ops: usize) {
    for _ in 0..ops {
      let node: NodeId = rng.gen_range(1..12);
      let other: NodeId = rng.gen_range(1..12);
      let etype = rng.gen_range(1..3);
      let key_id = rng.gen_range(1..4);
      match rng.gen_range(0..14) {
        0 | 1 => {
          let key = format!("k{}", rng.gen_range(0..6));
          let key = rng.gen_bool(0.7).then_some(key.as_str());
          if key.is_none_or(|key| delta.key_index.get(key).is_none()) {
            delta.create_node(node, key);
          }
        }
        2 => delta.delete_node(node),
        3 | 4 => delta.add_edge_over(node, etype, other, rng.gen_bool(0.3)),
        5 => delta.delete_edge_over(node, etype, other, rng.gen_bool(0.5)),
        6 => delta.add_edge(node, etype, other),
        7 => delta.delete_edge(node, etype, other),
        8 => delta.set_node_prop(node, key_id, PropValue::I64(rng.gen_range(0..100))),
        9 => delta.delete_node_prop(node, key_id),
        10 => delta.add_node_label(node, key_id),
        11 => delta.remove_node_label(node, key_id),
        12 => delta.set_edge_prop(node, etype, other, key_id, PropValue::I64(7)),
        _ => {
          delta.delete_edge_prop(node, etype, other, key_id);
          delta.define_label(key_id, &format!("label{key_id}"));
        }
      }
    }
  }

  /// `DeltaState::merge_from`, which moves whole entries where the
  /// committed delta holds nothing for them, leaves exactly what applying
  /// every change one at a time left, for random committed and pending
  /// deltas.
  #[test]
  fn merge_from_matches_the_change_by_change_merge_guard() {
    for seed in 0..4000u64 {
      let mut rng = StdRng::seed_from_u64(seed);
      let mut committed = DeltaState::new();
      let ops = rng.gen_range(0..40);
      random_writes(&mut rng, &mut committed, ops);
      let mut pending = DeltaState::new();
      let ops = rng.gen_range(0..40);
      random_writes(&mut rng, &mut pending, ops);

      let mut expected = committed.clone();
      reference_merge(&mut expected, pending.clone());
      let mut merged = committed;
      let mut drained = pending;
      merged.merge_from(&mut drained);
      assert_eq!(
        canonical(&merged),
        canonical(&expected),
        "seed {seed}: merge_from differs from the change-by-change merge"
      );
    }
  }
}

mod conflict_model {
  use rand::rngs::StdRng;
  use rand::{Rng, SeedableRng};

  use crate::mvcc::{ConflictDetector, TxKeyGroups, TxManager};
  use crate::types::{Timestamp, TxId, TxKey, TxKeySet};

  /// An open transaction as the model sees it.
  struct Open {
    txid: TxId,
    start_ts: Timestamp,
    reads: TxKeySet,
    writes: TxKeySet,
  }

  fn random_key(rng: &mut StdRng) -> TxKey {
    let node = rng.gen_range(0..40u64);
    match rng.gen_range(0..6) {
      0 => TxKey::Node(node),
      1 => TxKey::NodeProp {
        node_id: node,
        key_id: rng.gen_range(0..3),
      },
      2 => TxKey::Edge {
        src: node,
        etype: 1,
        dst: rng.gen_range(0..40),
      },
      3 => TxKey::NeighborsOut {
        node_id: node,
        etype: None,
      },
      4 => TxKey::NodeLabels(node),
      _ => TxKey::Key(format!("key{}", rng.gen_range(0..20)).into()),
    }
  }

  /// The transaction manager reports a conflict exactly when a commit since
  /// the transaction began wrote a key it read or wrote (the model keeps
  /// every commit's writes), with recent commits kept whole, folded into the
  /// index under a long-lived transaction, and pruned. Random interleavings
  /// of begins, reads, writes, commits and aborts, some with a transaction
  /// open across thousands of commits.
  #[test]
  fn conflict_checks_match_a_model_that_keeps_every_commit_guard() {
    for seed in 0..12u64 {
      let mut rng = StdRng::seed_from_u64(seed);
      let mut tx_mgr = TxManager::new();
      // Small enough to fold, prune and compact often.
      tx_mgr.set_keep_limits_for_test(8, 64, 16, 40);
      let detector = ConflictDetector::new();
      let mut commits: Vec<(Timestamp, TxKeySet)> = Vec::new();
      let mut open: Vec<Open> = Vec::new();
      let long_lived = seed % 2 == 0;
      if long_lived {
        let (txid, start_ts) = tx_mgr.begin_tx();
        open.push(Open {
          txid,
          start_ts,
          reads: TxKeySet::new(),
          writes: TxKeySet::new(),
        });
      }
      let mut folded = false;
      for step in 0..20_000 {
        folded |= tx_mgr.committed_writes_log_len() > 0;
        let action = rng.gen_range(0..10);
        if open.len() < 2 || action < 2 {
          let (txid, start_ts) = tx_mgr.begin_tx();
          open.push(Open {
            txid,
            start_ts,
            reads: TxKeySet::new(),
            writes: TxKeySet::new(),
          });
          continue;
        }
        // The long-lived one (index 0) stays open until the end.
        let first = usize::from(long_lived);
        let index = rng.gen_range(first..open.len().max(first + 1));
        let Some(tx) = open.get_mut(index) else {
          continue;
        };
        match action {
          2 | 3 => {
            let key = random_key(&mut rng);
            tx.reads.insert(key);
          }
          4..=6 => {
            for _ in 0..rng.gen_range(1..6) {
              let key = random_key(&mut rng);
              tx.writes.insert(key);
            }
          }
          7 => {
            let tx = open.swap_remove(index);
            tx_mgr.abort_tx(tx.txid);
          }
          _ => {
            let tx = open.swap_remove(index);
            let groups = rng
              .gen_bool(0.5)
              .then(|| TxKeyGroups::of(&tx.reads, &tx.writes));
            tx_mgr.record_reads_and_writes(tx.txid, tx.reads.clone(), tx.writes.clone(), groups);
            let expected = commits
              .iter()
              .filter(|(commit_ts, _)| *commit_ts >= tx.start_ts)
              .any(|(_, writes)| {
                writes
                  .iter()
                  .any(|key| tx.reads.contains(key) || tx.writes.contains(key))
              });
            let conflicted = detector.validate_commit(&tx_mgr, tx.txid).is_err();
            assert_eq!(
              conflicted, expected,
              "seed {seed} step {step}: conflict check disagrees with the model"
            );
            if conflicted {
              tx_mgr.abort_tx(tx.txid);
            } else {
              let commit_ts = tx_mgr.commit_tx(tx.txid).expect("commit");
              if !tx.writes.is_empty() {
                commits.push((commit_ts, tx.writes));
              }
            }
          }
        }
        // Spot-check the newest commit of a random key, for a snapshot an
        // open transaction can still have.
        if let Some(tx) = open.first() {
          let key = random_key(&mut rng);
          let expected = commits
            .iter()
            .rev()
            .find(|(commit_ts, writes)| *commit_ts >= tx.start_ts && writes.contains(&key))
            .map(|(commit_ts, _)| *commit_ts);
          assert_eq!(
            tx_mgr.committed_write_ts(&key, tx.start_ts),
            expected,
            "seed {seed} step {step}: newest commit of {key}"
          );
        }
      }
      if long_lived {
        assert!(folded, "seed {seed}: no commit was folded into the index");
      }
      assert!(tx_mgr.prune_work > 0, "seed {seed}: nothing was pruned");
    }
  }
}

// ============================================================================
// Finding 2: savepoints
// ============================================================================

mod savepoints {
  use std::sync::Arc;

  use tempfile::tempdir;

  use super::mvcc_options;
  use crate::core::single_file::{open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode};
  use crate::error::KiteError;
  use crate::types::PropValue;

  fn options() -> SingleFileOpenOptions {
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .auto_checkpoint(false)
  }

  fn open(dir: &tempfile::TempDir, options: SingleFileOpenOptions) -> SingleFileDB {
    open_single_file(dir.path().join("savepoints.kitedb"), options).expect("open")
  }

  /// A crash image of `db`'s file, opened: recovery replays its WAL.
  fn crash_image(db: &SingleFileDB, options: SingleFileOpenOptions) -> SingleFileDB {
    let image = db.path().with_extension("image.kitedb");
    std::fs::copy(db.path(), &image).expect("copy");
    open_single_file(&image, options).expect("open image")
  }

  /// Rolling back to a savepoint undoes every kind of write made since (in
  /// the transaction's own reads too) and keeps the earlier ones, which
  /// commit with the later ones.
  #[test]
  fn rollback_to_undoes_the_writes_since_and_keeps_the_rest() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    db.begin(false).expect("begin");
    let etype = db.define_etype("knows").expect("etype");
    let prop = db.define_propkey("age").expect("propkey");
    let label = db.define_label("Person").expect("label");
    let a = db.create_node(Some("a")).expect("a");
    db.set_node_prop(a, prop, PropValue::I64(1)).expect("prop");

    let savepoint = db.savepoint().expect("savepoint");
    let b = db.create_node(Some("b")).expect("b");
    db.set_node_prop(a, prop, PropValue::I64(2)).expect("prop");
    db.add_edge(a, etype, b).expect("edge");
    db.add_node_label(a, label).expect("label");
    db.delete_node_prop(a, prop).expect("delete prop");
    db.rollback_to(&savepoint).expect("rollback to");

    assert!(db.node_by_key("b").is_none(), "b is rolled back");
    assert_eq!(db.node_prop(a, prop), Some(PropValue::I64(1)));
    assert!(db.node_labels(a).is_empty());
    let c = db.create_node(Some("c")).expect("c");
    db.release_savepoint(savepoint).expect("release");
    db.commit().expect("commit");

    assert!(db.node_by_key("a").is_some() && db.node_by_key("c").is_some());
    assert!(db.node_by_key("b").is_none());
    assert_eq!(db.node_prop(a, prop), Some(PropValue::I64(1)));
    assert!(!db.edge_exists(a, etype, c));
    assert_eq!(db.count_edges(), 0);
  }

  /// A savepoint stays live after a rollback to it; rolling back to or
  /// releasing one ends those taken after it; a savepoint works only in the
  /// transaction that took it, and only in a write transaction.
  #[test]
  fn savepoints_nest_and_belong_to_their_transaction() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    assert!(matches!(db.savepoint(), Err(KiteError::NoTransaction)));
    db.begin(true).expect("read-only begin");
    assert!(matches!(db.savepoint(), Err(KiteError::ReadOnly)));
    db.rollback().expect("end");

    db.begin(false).expect("begin");
    let outer = db.savepoint().expect("outer");
    db.create_node(Some("one")).expect("one");
    let inner = db.savepoint().expect("inner");
    db.create_node(Some("two")).expect("two");
    db.rollback_to(&outer).expect("rollback to outer");
    assert!(matches!(
      db.rollback_to(&inner),
      Err(KiteError::InvalidSavepoint(_))
    ));
    assert!(matches!(
      db.release_savepoint(inner),
      Err(KiteError::InvalidSavepoint(_))
    ));
    db.create_node(Some("three")).expect("three");
    db.rollback_to(&outer).expect("again");
    assert!(db.node_by_key("one").is_none() && db.node_by_key("three").is_none());

    // Releasing an outer savepoint ends the inner ones.
    let inner = db.savepoint().expect("inner");
    db.release_savepoint(outer).expect("release outer");
    assert!(matches!(
      db.rollback_to(&inner),
      Err(KiteError::InvalidSavepoint(_))
    ));
    db.commit().expect("commit");

    db.begin(false).expect("next transaction");
    assert!(matches!(
      db.rollback_to(&inner),
      Err(KiteError::InvalidSavepoint(_))
    ));
    db.rollback().expect("rollback");
  }

  /// Rolling back past schema definitions unstages them and releases this
  /// transaction's claims on their names; defining them again works, and a
  /// name only rolled back never reaches the WAL.
  #[test]
  fn rollback_to_after_a_schema_define() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    db.begin(false).expect("begin");
    let node = db.create_node(Some("n")).expect("node");
    let savepoint = db.savepoint().expect("savepoint");
    let label = db.define_label("Gone").expect("label");
    db.define_etype("GoneType").expect("etype");
    db.define_propkey("gone_prop").expect("propkey");
    db.add_node_label(node, label).expect("use label");
    db.rollback_to(&savepoint).expect("rollback to");

    assert_eq!(db.label_id("Gone"), None);
    assert_eq!(db.etype_id("GoneType"), None);
    assert_eq!(db.propkey_id("gone_prop"), None);
    {
      let reservations = db.schema_reservations.lock();
      assert!(
        reservations.labels.is_empty()
          && reservations.etypes.is_empty()
          && reservations.propkeys.is_empty(),
        "the rolled-back definitions still hold their names: {reservations:?}"
      );
    }
    let kept = db.define_label("Kept").expect("define after the rollback");
    db.add_node_label(node, kept).expect("use it");
    db.commit().expect("commit");
    assert_eq!(db.label_id("Gone"), None);
    assert_eq!(db.label_id("Kept"), Some(kept));

    let reopened = crash_image(&db, options());
    assert_eq!(reopened.label_id("Gone"), None);
    assert_eq!(reopened.etype_id("GoneType"), None);
    assert_eq!(reopened.label_id("Kept"), Some(kept));
  }

  /// With MVCC, writes rolled back to a savepoint are not the transaction's
  /// any more: a concurrent commit of the same keys does not make it
  /// conflict. Writes kept past a released savepoint still do.
  #[test]
  fn rolled_back_writes_cause_no_mvcc_conflict() {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(open(&dir, mvcc_options()));
    db.begin(false).expect("begin");
    let prop = db.define_propkey("count").expect("propkey");
    let node = db.create_node(Some("counter")).expect("node");
    db.commit().expect("commit");
    let concurrent_write = |value: i64| {
      let db = Arc::clone(&db);
      std::thread::spawn(move || {
        db.begin(false)?;
        db.set_node_prop(node, prop, PropValue::I64(value))?;
        db.commit()
      })
      .join()
      .expect("writer")
    };

    db.begin(false).expect("begin");
    db.create_node(Some("other")).expect("other");
    let savepoint = db.savepoint().expect("savepoint");
    db.set_node_prop(node, prop, PropValue::I64(1))
      .expect("set");
    db.rollback_to(&savepoint).expect("rollback to");
    concurrent_write(2).expect("concurrent commit");
    db.commit()
      .expect("the rolled-back write must not conflict");
    assert_eq!(db.node_prop(node, prop), Some(PropValue::I64(2)));

    db.begin(false).expect("begin");
    let savepoint = db.savepoint().expect("savepoint");
    db.set_node_prop(node, prop, PropValue::I64(3))
      .expect("set");
    db.release_savepoint(savepoint).expect("release");
    concurrent_write(4).expect("concurrent commit");
    assert!(
      matches!(db.commit(), Err(KiteError::Conflict { .. })),
      "a kept write still conflicts"
    );
  }

  /// While a savepoint is live, the transaction's records stay out of the
  /// WAL; once the last one is released they follow the transaction's
  /// policy (a small one keeps them back until its commit). After the
  /// commit, neither a reopen nor a crash image replays a rolled-back
  /// record.
  #[test]
  fn wal_replay_after_commit_contains_no_rolled_back_records() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    db.begin(false).expect("begin");
    db.create_node(Some("before")).expect("before");
    let head = db.wal_stats().head;
    let savepoint = db.savepoint().expect("savepoint");
    db.create_node(Some("rolled-back")).expect("rolled back");
    assert_eq!(
      db.wal_stats().head,
      head,
      "a live savepoint keeps records back"
    );
    db.rollback_to(&savepoint).expect("rollback to");
    db.create_node(Some("kept")).expect("kept");
    db.release_savepoint(savepoint).expect("release");
    db.create_node(Some("after")).expect("after");
    db.commit().expect("commit");
    assert!(db.wal_stats().head > head, "the commit writes them");

    let crashed = crash_image(&db, options());
    let path = db.path().to_path_buf();
    drop(db);
    let reopened = open_single_file(&path, options()).expect("reopen");
    for opened in [&crashed, &reopened] {
      for key in ["before", "kept", "after"] {
        assert!(opened.node_by_key(key).is_some(), "{key} is missing");
      }
      assert!(opened.node_by_key("rolled-back").is_none());
    }
  }

  /// A transaction committed with its savepoint still live writes the
  /// records it kept back with its COMMIT record; one rolled back writes
  /// none of them.
  #[test]
  fn records_kept_back_are_written_by_the_commit() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    db.begin(false).expect("begin");
    let _savepoint = db.savepoint().expect("savepoint");
    db.create_node(Some("committed")).expect("node");
    db.commit().expect("commit");

    db.begin(false).expect("begin");
    let _savepoint = db.savepoint().expect("savepoint");
    db.create_node(Some("rolled-back")).expect("node");
    db.rollback().expect("rollback");

    let crashed = crash_image(&db, options());
    assert!(crashed.node_by_key("committed").is_some());
    assert!(crashed.node_by_key("rolled-back").is_none());
  }

  /// In a bulk load, which keeps every record until its commit, rolling back
  /// to a savepoint drops the records since.
  #[test]
  fn bulk_load_rolls_back_to_a_savepoint() {
    let dir = tempdir().expect("tempdir");
    let db = open(&dir, options());
    db.begin_bulk().expect("begin bulk");
    db.create_nodes_batch(&[Some("a")]).expect("a");
    let savepoint = db.savepoint().expect("savepoint");
    db.create_nodes_batch(&[Some("b")]).expect("b");
    db.rollback_to(&savepoint).expect("rollback to");
    db.commit().expect("commit");
    let crashed = crash_image(&db, options());
    for opened in [&db, &crashed] {
      assert!(opened.node_by_key("a").is_some());
      assert!(opened.node_by_key("b").is_none());
    }
  }

  /// Records kept back by a savepoint across a background checkpoint (the
  /// cut cannot copy them: they are not in the WAL) are written after it, and
  /// the commit is whole live, after a reopen and in a crash image.
  #[test]
  fn records_kept_back_across_a_background_checkpoint() {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(open(&dir, options()));
    for index in 0..20 {
      db.begin(false).expect("begin");
      db.create_node(Some(&format!("pre-{index}"))).expect("node");
      db.commit().expect("commit");
    }
    db.begin(false).expect("begin");
    db.create_node(Some("open-before-cut")).expect("node");
    let savepoint = db.savepoint().expect("savepoint");
    db.create_node(Some("kept-back")).expect("node");
    {
      let db = Arc::clone(&db);
      std::thread::spawn(move || db.background_checkpoint())
        .join()
        .expect("checkpointer")
        .expect("background checkpoint");
    }
    db.release_savepoint(savepoint).expect("release");
    db.create_node(Some("after-cut")).expect("node");
    db.commit().expect("commit");

    let crashed = crash_image(&db, options());
    let path = db.path().to_path_buf();
    drop(db);
    let reopened = open_single_file(&path, options()).expect("reopen");
    for opened in [&crashed, &reopened] {
      for key in ["pre-0", "open-before-cut", "kept-back", "after-cut"] {
        assert!(opened.node_by_key(key).is_some(), "{key} is missing");
      }
    }
  }
}
