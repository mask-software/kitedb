//! raydb-b4 `replication-core` lane: epoch fencing under the commit lock
//! (P8), the snapshot copy under concurrent commits (P9), and batched replica
//! apply (P6). Included from replication.rs for the commit hooks and the
//! replica's transaction counter.

use crate::core::single_file::transaction::{BEFORE_NEXT_COMMIT_LOCK, BEFORE_NEXT_COMMIT_MERGE};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileDB, SingleFileOpenOptions, SyncMode,
};
use crate::replication::types::{CommitToken, ReplicationRole};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;
use tempfile::tempdir;

fn primary_options() -> SingleFileOpenOptions {
  SingleFileOpenOptions::new()
    .sync_mode(SyncMode::Full)
    .auto_checkpoint(false)
    .replication_role(ReplicationRole::Primary)
}

fn open_replica(path: &Path, source_db_path: &Path) -> SingleFileDB {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(source_db_path),
  )
  .expect("open replica")
}

fn commit_node(db: &SingleFileDB, key: &str) -> Option<CommitToken> {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit_with_token().expect("commit")
}

/// P8: `ensure_local_commit_allowed` ran before the commit lock, so another
/// instance promoted while a commit waited for that lock (behind a long
/// commit or a checkpoint) did not fence it: the stale primary committed
/// locally, and its sidecar append then failed, leaving a commit that no
/// replica will ever see. The check must run under the commit lock.
///
/// Deterministic: the hook before the commit lock promotes the other
/// instance, after the old check and before the lock.
#[test]
fn b4_p8_promotion_while_a_commit_waits_for_the_commit_lock_fences_it() {
  let dir = tempdir().expect("tempdir");
  let sidecar = dir.path().join("p8-lock.sidecar");
  let primary_a = open_single_file(
    dir.path().join("p8-lock-a.kitedb"),
    primary_options().replication_sidecar_path(&sidecar),
  )
  .expect("open primary a");
  let primary_b = Arc::new(
    open_single_file(
      dir.path().join("p8-lock-b.kitedb"),
      primary_options().replication_sidecar_path(&sidecar),
    )
    .expect("open primary b"),
  );

  let token = commit_node(&primary_a, "a0").expect("a0 token");
  assert_eq!(token.epoch, 1, "setup: a writes epoch 1");

  let promoter = Arc::clone(&primary_b);
  BEFORE_NEXT_COMMIT_LOCK.with(|hook| {
    *hook.borrow_mut() = Some(Box::new(move || {
      let promoted = std::thread::spawn(move || promoter.primary_promote_to_next_epoch())
        .join()
        .expect("promoter thread");
      assert_eq!(promoted.expect("promote b"), 2, "setup: b promoted");
    }));
  });

  primary_a.begin(false).expect("begin late");
  primary_a.create_node(Some("late")).expect("create late");
  let result = primary_a.commit_with_token();
  BEFORE_NEXT_COMMIT_LOCK.with(|hook| hook.borrow_mut().take());
  let committed_locally = primary_a.node_by_key("late").is_some();
  assert!(
    matches!(&result, Err(error) if error.to_string().contains("stale primary"))
      && !committed_locally,
    "a primary superseded while its commit waited for the commit lock must reject the commit: \
     commit={result:?}, committed_locally={committed_locally}"
  );

  close_single_file(primary_a).expect("close a");
  let primary_b = Arc::into_inner(primary_b).expect("b unique");
  close_single_file(primary_b).expect("close b");
}

/// P9: the snapshot export read the database file with no lock, so it could
/// copy a commit that is durable in the file before the commit appended its
/// replication frame. The snapshot then holds commit N under head N-1, and a
/// replica that resumes after the head applies commit N twice. The copy must
/// run under the commit lock (and the checkpoint gate), as backups do.
///
/// Deterministic: the merge hook runs inside the commit, after its header is
/// durable and before its frame is appended, and gives a concurrent export
/// 300 ms to finish there. Locked, the export waits for the commit instead.
#[test]
fn b4_p9_snapshot_export_does_not_copy_a_commit_before_its_frame() {
  let dir = tempdir().expect("tempdir");
  let primary = Arc::new(
    open_single_file(dir.path().join("p9-copy.kitedb"), primary_options()).expect("open primary"),
  );
  for i in 0..2 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let (exporter_tx, exporter_rx) = mpsc::channel();
  let exporter_db = Arc::clone(&primary);
  BEFORE_NEXT_COMMIT_MERGE.with(|hook| {
    *hook.borrow_mut() = Some(Box::new(move || {
      let (done_tx, done_rx) = mpsc::channel();
      let exporter = std::thread::spawn(move || {
        let json = exporter_db
          .primary_export_snapshot_transport_json(true)
          .expect("export snapshot");
        let _ = done_tx.send(());
        json
      });
      let _ = done_rx.recv_timeout(Duration::from_millis(300));
      exporter_tx.send(exporter).expect("hand over exporter");
    }));
  });
  let token = commit_node(&primary, "n2").expect("n2 token");
  assert_eq!(token.log_index, 3, "setup: third frame");

  let json = exporter_rx
    .recv()
    .expect("exporter")
    .join()
    .expect("exporter thread");
  let snapshot: serde_json::Value = serde_json::from_str(&json).expect("parse snapshot json");
  let head = snapshot["head_log_index"].as_u64().expect("head_log_index");
  let data = BASE64_STANDARD
    .decode(snapshot["data_base64"].as_str().expect("data_base64"))
    .expect("decode data");
  let copy_path = dir.path().join("p9-copy-snapshot.kitedb");
  std::fs::write(&copy_path, &data).expect("write snapshot copy");
  let copy = open_single_file(
    &copy_path,
    SingleFileOpenOptions::new().auto_checkpoint(false),
  )
  .expect("open snapshot copy");
  let copied_nodes = copy.count_nodes() as u64;
  close_single_file(copy).expect("close copy");
  assert_eq!(
    copied_nodes, head,
    "every commit creates one node and one frame, so a consistent snapshot holds head_log_index \
     ({head}) nodes; it holds {copied_nodes}"
  );

  let primary = Arc::into_inner(primary).expect("primary unique");
  close_single_file(primary).expect("close primary");
}

/// P6: catch-up applied every frame in its own transaction (two syncs per
/// frame in Full mode). A contiguous run of frames must apply in one
/// transaction.
#[test]
fn b4_p6_catch_up_applies_a_run_of_frames_in_one_transaction() {
  let dir = tempdir().expect("tempdir");
  let primary_path = dir.path().join("p6-batch-primary.kitedb");
  let primary = open_single_file(&primary_path, primary_options()).expect("open primary");
  let replica = open_replica(&dir.path().join("p6-batch-replica.kitedb"), &primary_path);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");
  for i in 0..32 {
    commit_node(&primary, &format!("n{i}")).expect("token");
  }

  let before = replica.next_tx_id.load(Ordering::SeqCst);
  let applied = replica.replica_catch_up_once(64).expect("catch up");
  let transactions = replica.next_tx_id.load(Ordering::SeqCst) - before;
  assert_eq!(applied, 32, "setup: one pull applies every frame");
  assert!(
    (0..32).all(|i| replica.node_by_key(&format!("n{i}")).is_some()),
    "every frame applied"
  );
  assert_eq!(
    transactions, 1,
    "32 contiguous frames must apply in one replica transaction; catch-up used {transactions}"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

/// P4: a bootstrap commits about every 10k writes, not the whole graph at
/// once.
#[test]
fn b4_p4_bootstrap_commits_in_bounded_batches() {
  const NODES: usize = 25_000;
  let dir = tempdir().expect("tempdir");
  let primary_path = dir.path().join("p4-batches-primary.kitedb");
  let primary =
    open_single_file(&primary_path, primary_options().wal_size(16 << 20)).expect("open primary");
  primary.begin(false).expect("begin");
  for i in 0..NODES {
    primary
      .create_node(Some(&format!("n{i}")))
      .expect("create node");
  }
  primary.commit().expect("commit");

  let replica = open_replica(&dir.path().join("p4-batches-replica.kitedb"), &primary_path);
  let before = replica.next_tx_id.load(Ordering::SeqCst);
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap");
  let transactions = replica.next_tx_id.load(Ordering::SeqCst) - before;
  assert_eq!(replica.count_nodes(), NODES);
  assert!(
    (3..=4).contains(&transactions),
    "{NODES} creates commit in batches of 10k: {transactions} transactions"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

/// P4: once a batch commits, the replica holds part of a copy; until the
/// bootstrap sets its cursor, catch-up must refuse to run over it.
#[test]
fn b4_p4_a_committed_batch_marks_the_replica_incomplete_until_the_cursor_is_set() {
  let dir = tempdir().expect("tempdir");
  let primary_path = dir.path().join("p4-incomplete-primary.kitedb");
  let primary = open_single_file(&primary_path, primary_options()).expect("open primary");
  let replica = open_replica(
    &dir.path().join("p4-incomplete-replica.kitedb"),
    &primary_path,
  );
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap empty primary");
  commit_node(&primary, "n0").expect("token");

  // An attempt that commits a batch and stops before its cursor.
  {
    let runtime = replica
      .replica_replication
      .as_ref()
      .expect("replica runtime");
    let mut batch = super::BootstrapBatch::new(&replica, runtime);
    batch
      .write(|db| db.create_node_with_id(500, Some("partial")))
      .expect("write");
    batch.finish().expect("commit batch");
  }

  let error = replica
    .replica_catch_up_once(64)
    .expect_err("catch-up over a partial copy");
  assert!(error.to_string().contains("interrupted"), "{error}");
  assert!(
    replica
      .replica_replication_status()
      .expect("status")
      .needs_reseed
  );

  replica.replica_reseed_from_snapshot().expect("reseed");
  assert!(replica.node_by_key("partial").is_none());
  assert!(replica.node_by_key("n0").is_some());
  commit_node(&primary, "n1").expect("token");
  assert_eq!(replica.replica_catch_up_once(64).expect("catch up"), 1);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

/// The sidecar syncs as the database does: in Full mode each commit's frame
/// (and the manifest naming it) with plain fsync, or `F_FULLFSYNC` with the
/// `full_fsync` opt-in; in Normal and Off modes nothing per commit, and the
/// buffered frames at a checkpoint. `File::sync_all`, which the sidecar used
/// everywhere, is `F_FULLFSYNC` on macOS.
#[test]
fn b4_sidecar_syncs_follow_the_database_sync_policy() {
  use crate::replication::durability::sidecar_syncs_during;

  let full_sync = if cfg!(target_os = "macos") {
    "F_FULLFSYNC"
  } else {
    "sync_all"
  };
  let plain_sync = if cfg!(target_os = "macos") {
    "fsync"
  } else {
    "sync_all"
  };
  let dir = tempdir().expect("tempdir");
  for (name, mode, full_fsync) in [
    ("full", SyncMode::Full, false),
    ("full-fullfsync", SyncMode::Full, true),
    ("normal", SyncMode::Normal, true),
    ("off", SyncMode::Off, false),
  ] {
    let db = open_single_file(
      dir.path().join(format!("sync-{name}.kitedb")),
      primary_options().sync_mode(mode).full_fsync(full_fsync),
    )
    .expect("open primary");
    let (token, commit_syncs) = sidecar_syncs_during(|| commit_node(&db, "n0"));
    assert!(token.is_some(), "{name}: commit appended its frame");
    let (checkpointed, checkpoint_syncs) = sidecar_syncs_during(|| db.checkpoint());
    checkpointed.expect("checkpoint");

    match mode {
      SyncMode::Full => {
        let expected = if full_fsync { full_sync } else { plain_sync };
        assert!(
          commit_syncs.len() >= 2 && commit_syncs.iter().all(|sync| *sync == expected),
          "{name}: the frame and the manifest are synced before the commit returns, with \
           {expected}: {commit_syncs:?}"
        );
      }
      SyncMode::Normal | SyncMode::Off => {
        assert!(
          commit_syncs.is_empty(),
          "{name}: no sidecar sync per commit: {commit_syncs:?}"
        );
        assert!(
          !checkpoint_syncs.is_empty() && checkpoint_syncs.iter().all(|sync| *sync == plain_sync),
          "{name}: a checkpoint syncs the buffered frames with {plain_sync}: {checkpoint_syncs:?}"
        );
      }
    }
    close_single_file(db).expect("close");
  }
}
