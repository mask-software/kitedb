//! raydb-b4 `core-misc` lane: vector-store compaction at checkpoint (B12).
//! Included from checkpoint.rs, so its test hooks are in scope.
use super::*;
use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use crate::vector::store::vector_store_vector_id;
use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

const DIMENSIONS: usize = 4;
/// Vectors per fragment. The default (100_000) would take that many vectors
/// to seal a single fragment.
const FRAGMENT_SIZE: usize = 32;
/// Enough vectors to seal 20 fragments.
const NODES: usize = 20 * FRAGMENT_SIZE;

/// The operations that rewrite the snapshot from the live stores.
#[derive(Clone, Copy, Debug)]
enum Rewrite {
  Blocking,
  Background,
  Optimize,
}

fn options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new().auto_checkpoint(false)
}

fn vector_for(index: usize) -> Vec<f32> {
  vec![1.0 + index as f32, 2.0, 0.5 + (index % 7) as f32, -1.0]
}

/// A vector per node, then 3 of every 4 deleted, so every sealed fragment is
/// about 75% tombstones. Returns the property key and the nodes, `v{index}`.
fn populate(db: &SingleFileDB) -> (PropKeyId, Vec<NodeId>) {
  db.begin(false).expect("begin");
  let embedding = db.define_propkey("embedding").expect("propkey");
  // The store the commit fills (stores keep their config across snapshots).
  db.vector_stores.write().insert(
    embedding,
    create_vector_store(
      VectorStoreConfig::new(DIMENSIONS)
        .with_fragment_target_size(FRAGMENT_SIZE)
        .with_row_group_size(8),
    ),
  );
  let nodes: Vec<NodeId> = (0..NODES)
    .map(|index| {
      let node = db
        .create_node(Some(&format!("v{index}")))
        .expect("create node");
      db.set_node_vector(node, embedding, &vector_for(index))
        .expect("set vector");
      node
    })
    .collect();
  db.commit().expect("commit vectors");

  db.begin(false).expect("begin");
  for (index, &node) in nodes.iter().enumerate() {
    if index % 4 != 0 {
      db.delete_node_vector(node, embedding)
        .expect("delete vector");
    }
  }
  db.commit().expect("commit deletes");
  (embedding, nodes)
}

/// Each node holding a vector, with its vector id and its vector as
/// `node_vector` reads it.
fn live_vectors(
  db: &SingleFileDB,
  embedding: PropKeyId,
  nodes: &[NodeId],
) -> HashMap<NodeId, (u64, Vec<f32>)> {
  db.materialize_all_vector_stores()
    .expect("materialize stores");
  let ids: Vec<(NodeId, u64)> = {
    let stores = db.vector_stores.read();
    let store = stores.get(&embedding).expect("vector store");
    nodes
      .iter()
      .filter_map(|&node| Some((node, vector_store_vector_id(store, node)?)))
      .collect()
  };
  ids
    .into_iter()
    .map(|(node, id)| {
      let vector = db
        .node_vector(node, embedding)
        .expect("vector of a node with an id");
      (node, (id, vector.to_vec()))
    })
    .collect()
}

/// `(total_vectors, total_deleted)` of the store.
fn store_totals(db: &SingleFileDB, embedding: PropKeyId) -> (usize, usize) {
  db.materialize_all_vector_stores()
    .expect("materialize stores");
  let stores = db.vector_stores.read();
  let store = stores.get(&embedding).expect("vector store");
  (store.total_vectors, store.total_deleted)
}

/// The store holds exactly the `expected` vectors, under the same vector ids
/// where one is given, and only a few tombstones: at most those of the active
/// fragment and of one sealed fragment too few to compact on its own.
fn assert_compacted(
  db: &SingleFileDB,
  embedding: PropKeyId,
  nodes: &[NodeId],
  expected: &HashMap<NodeId, (Option<u64>, Vec<f32>)>,
  stage: &str,
) {
  let actual = live_vectors(db, embedding, nodes);
  let mut actual_nodes: Vec<NodeId> = actual.keys().copied().collect();
  let mut expected_nodes: Vec<NodeId> = expected.keys().copied().collect();
  actual_nodes.sort_unstable();
  expected_nodes.sort_unstable();
  assert_eq!(actual_nodes, expected_nodes, "{stage}: nodes with a vector");
  for (node, (id, vector)) in expected {
    let (actual_id, actual_vector) = &actual[node];
    if let Some(id) = id {
      assert_eq!(actual_id, id, "{stage}: vector id of node {node}");
    }
    assert_eq!(actual_vector, vector, "{stage}: vector of node {node}");
  }
  let distinct_ids: HashSet<u64> = actual.values().map(|(id, _)| *id).collect();
  assert_eq!(distinct_ids.len(), actual.len(), "{stage}: vector ids");

  let (total, deleted) = store_totals(db, embedding);
  assert_eq!(total - deleted, expected.len(), "{stage}: live count");
  assert!(
    total <= expected.len() + 2 * FRAGMENT_SIZE,
    "{stage}: the store still holds {total} vectors ({deleted} deleted) for {} live ones: \
     the rewrite did not compact its fragments",
    expected.len()
  );
}

fn rewrite_compacts_deleted_vectors(rewrite: Rewrite) {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join(format!("b12-{rewrite:?}.kitedb"));
  let db = Arc::new(open_single_file(&path, options()).expect("open"));
  let (embedding, nodes) = populate(&db);

  let mut expected: HashMap<NodeId, (Option<u64>, Vec<f32>)> = live_vectors(&db, embedding, &nodes)
    .into_iter()
    .map(|(node, (id, vector))| (node, (Some(id), vector)))
    .collect();
  assert_eq!(expected.len(), NODES / 4, "setup: live vectors");
  assert_eq!(
    store_totals(&db, embedding),
    (NODES, NODES - NODES / 4),
    "setup: deletes leave tombstones"
  );

  match rewrite {
    Rewrite::Blocking => db.checkpoint().expect("checkpoint"),
    Rewrite::Optimize => db.optimize_single_file(None).expect("optimize"),
    Rewrite::Background => {
      // Vector changes committed after the cut reach the new stores through
      // the install's replay, keyed by node and property.
      let parked = Arc::new(Barrier::new(2));
      set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
      let checkpointer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || db.background_checkpoint())
      };
      parked.wait();

      db.begin(false).expect("begin");
      for (index, vector) in [(1, vector_for(NODES + 1)), (4, vector_for(NODES + 4))] {
        // Node 1's vector was deleted; node 4's is replaced.
        db.set_node_vector(nodes[index], embedding, &vector)
          .expect("set vector");
        let stored = db.node_vector(nodes[index], embedding).expect("pending");
        expected.insert(nodes[index], (None, stored.to_vec()));
      }
      db.delete_node_vector(nodes[8], embedding)
        .expect("delete vector");
      expected.remove(&nodes[8]);
      db.commit().expect("commit after the cut");

      checkpointer
        .join()
        .expect("checkpoint thread")
        .expect("background checkpoint");
    }
  }
  assert_compacted(
    &db,
    embedding,
    &nodes,
    &expected,
    &format!("after {rewrite:?}"),
  );

  let db = Arc::into_inner(db).expect("sole owner");
  close_single_file(db).expect("close");
  let reopened = open_single_file(&path, options()).expect("reopen");
  assert_compacted(
    &reopened,
    embedding,
    &nodes,
    &expected,
    &format!("after {rewrite:?} and reopen"),
  );

  // The compacted store keeps taking vectors, under fresh ids.
  reopened.begin(false).expect("begin");
  let node = reopened.create_node(Some("new")).expect("create node");
  reopened
    .set_node_vector(node, embedding, &vector_for(0))
    .expect("set vector");
  reopened.commit().expect("commit");
  let stored = reopened.node_vector(node, embedding).expect("new vector");
  expected.insert(node, (None, stored.to_vec()));
  let mut all_nodes = nodes.clone();
  all_nodes.push(node);
  reopened.checkpoint().expect("next checkpoint");
  assert_compacted(
    &reopened,
    embedding,
    &all_nodes,
    &expected,
    &format!("after {rewrite:?}, reopen and an insert"),
  );
  close_single_file(reopened).expect("close");
}

#[test]
fn b12_blocking_checkpoint_compacts_deleted_vectors() {
  rewrite_compacts_deleted_vectors(Rewrite::Blocking);
}

#[test]
fn b12_background_checkpoint_compacts_deleted_vectors() {
  rewrite_compacts_deleted_vectors(Rewrite::Background);
}

#[test]
fn b12_optimize_compacts_deleted_vectors() {
  rewrite_compacts_deleted_vectors(Rewrite::Optimize);
}
