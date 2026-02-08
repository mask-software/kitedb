use kitedb::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use kitedb::replication::primary::default_replication_sidecar_path;
use kitedb::replication::types::ReplicationRole;

fn open_primary(path: &std::path::Path) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new().replication_role(ReplicationRole::Primary),
  )
}

fn open_replica(
  path: &std::path::Path,
  primary_path: &std::path::Path,
) -> kitedb::Result<kitedb::core::single_file::SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .replication_role(ReplicationRole::Replica)
      .replication_source_db_path(primary_path),
  )
}

#[test]
fn replica_bootstrap_from_snapshot_reaches_primary_state() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-bootstrap.kitedb");
  let replica_path = dir.path().join("replica-bootstrap.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");

  primary.begin(false).expect("begin");
  let n1 = primary.create_node(Some("n1")).expect("n1");
  let n2 = primary.create_node(Some("n2")).expect("n2");
  primary.add_edge(n1, 1, n2).expect("edge");
  primary.commit_with_token().expect("commit").expect("token");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  assert_eq!(replica.count_nodes(), primary.count_nodes());
  assert_eq!(replica.count_edges(), primary.count_edges());
  for node_id in primary.list_nodes() {
    assert!(replica.node_exists(node_id));
    assert_eq!(replica.node_key(node_id), primary.node_key(node_id));
  }

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn incremental_catch_up_applies_frames_in_order() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-catch-up.kitedb");
  let replica_path = dir.path().join("replica-catch-up.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");

  primary.begin(false).expect("begin");
  primary.create_node(Some("base")).expect("create base");
  let base_token = primary
    .commit_with_token()
    .expect("commit")
    .expect("base token");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");
  let status = replica.replica_replication_status().expect("status");
  assert_eq!(status.applied_log_index, base_token.log_index);

  primary.begin(false).expect("begin c1");
  primary.create_node(Some("c1")).expect("create c1");
  let token1 = primary
    .commit_with_token()
    .expect("commit c1")
    .expect("token c1");

  primary.begin(false).expect("begin c2");
  primary.create_node(Some("c2")).expect("create c2");
  let token2 = primary
    .commit_with_token()
    .expect("commit c2")
    .expect("token c2");

  let pulled = replica.replica_catch_up_once(1).expect("pull one");
  assert_eq!(pulled, 1);
  let status = replica
    .replica_replication_status()
    .expect("status after one");
  assert_eq!(status.applied_log_index, token1.log_index);

  let pulled = replica.replica_catch_up_once(8).expect("pull remaining");
  assert_eq!(pulled, 1);
  let status = replica
    .replica_replication_status()
    .expect("status after remaining");
  assert_eq!(status.applied_log_index, token2.log_index);

  assert_eq!(replica.count_nodes(), primary.count_nodes());

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn duplicate_chunk_delivery_is_idempotent() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-duplicate.kitedb");
  let replica_path = dir.path().join("replica-duplicate.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");
  primary.begin(false).expect("begin");
  primary.create_node(Some("a")).expect("create a");
  primary
    .commit_with_token()
    .expect("commit a")
    .expect("token a");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  primary.begin(false).expect("begin b");
  primary.create_node(Some("b")).expect("create b");
  primary
    .commit_with_token()
    .expect("commit b")
    .expect("token b");

  replica.replica_catch_up_once(8).expect("initial catch up");
  let node_count_before = replica.count_nodes();
  let status_before = replica.replica_replication_status().expect("status before");

  let replayed = replica
    .replica_catch_up_once_replaying_last_for_testing(1)
    .expect("replay last chunk");
  assert_eq!(replayed, 0, "duplicate frame should be ignored");

  let status_after = replica.replica_replication_status().expect("status after");
  assert_eq!(
    status_after.applied_log_index,
    status_before.applied_log_index
  );
  assert_eq!(replica.count_nodes(), node_count_before);

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn replica_restart_resumes_from_durable_cursor() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-resume.kitedb");
  let replica_path = dir.path().join("replica-resume.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");
  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  primary.begin(false).expect("begin c1");
  primary.create_node(Some("c1")).expect("create c1");
  let t1 = primary
    .commit_with_token()
    .expect("commit c1")
    .expect("token c1");

  primary.begin(false).expect("begin c2");
  primary.create_node(Some("c2")).expect("create c2");
  let t2 = primary
    .commit_with_token()
    .expect("commit c2")
    .expect("token c2");

  let pulled = replica
    .replica_catch_up_once(1)
    .expect("pull one before restart");
  assert_eq!(pulled, 1);
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status")
      .applied_log_index,
    t1.log_index
  );

  close_single_file(replica).expect("close replica");

  let replica = open_replica(&replica_path, &primary_path).expect("reopen replica");
  let status = replica
    .replica_replication_status()
    .expect("status after reopen");
  assert_eq!(status.applied_log_index, t1.log_index);

  let pulled = replica.replica_catch_up_once(8).expect("pull after reopen");
  assert_eq!(pulled, 1);
  assert_eq!(
    replica
      .replica_replication_status()
      .expect("status final")
      .applied_log_index,
    t2.log_index
  );
  assert_eq!(replica.count_nodes(), primary.count_nodes());

  close_single_file(replica).expect("close replica final");
  close_single_file(primary).expect("close primary");
}

#[test]
fn wait_for_token_times_out_then_succeeds_after_catch_up() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-wait.kitedb");
  let replica_path = dir.path().join("replica-wait.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");
  let _primary_sidecar = default_replication_sidecar_path(&primary_path);

  primary.begin(false).expect("begin base");
  primary.create_node(Some("base")).expect("create base");
  primary
    .commit_with_token()
    .expect("commit base")
    .expect("token base");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");

  primary.begin(false).expect("begin next");
  primary.create_node(Some("next")).expect("create next");
  let token = primary
    .commit_with_token()
    .expect("commit next")
    .expect("token next");

  let timed_out = replica.wait_for_token(token, 20).expect("wait timeout");
  assert!(!timed_out, "token should not be visible before catch-up");

  replica.replica_catch_up_once(8).expect("catch up");

  let reached = replica.wait_for_token(token, 1_000).expect("wait success");
  assert!(reached, "token should be visible after catch-up");

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}

#[test]
fn vector_property_mutations_replicate_and_delete() {
  let dir = tempfile::tempdir().expect("tempdir");
  let primary_path = dir.path().join("primary-vector-repl.kitedb");
  let replica_path = dir.path().join("replica-vector-repl.kitedb");

  let primary = open_primary(&primary_path).expect("open primary");
  primary.begin(false).expect("begin base");
  let node = primary
    .create_node(Some("vec-node"))
    .expect("create vec node");
  let embedding_key = primary.define_propkey("embedding").expect("define propkey");
  primary
    .set_node_vector(node, embedding_key, &[0.1, 0.2, 0.3])
    .expect("set vector");
  let token_set = primary
    .commit_with_token()
    .expect("commit set")
    .expect("token set");

  let replica = open_replica(&replica_path, &primary_path).expect("open replica");
  replica
    .replica_bootstrap_from_snapshot()
    .expect("bootstrap snapshot");
  let status = replica.replica_replication_status().expect("status");
  assert_eq!(status.applied_log_index, token_set.log_index);
  let primary_vector = primary
    .node_vector(node, embedding_key)
    .expect("primary vector after commit");
  let replica_vector = replica
    .node_vector(node, embedding_key)
    .expect("replica vector after bootstrap");
  assert_eq!(replica_vector.len(), primary_vector.len());
  for (replica_value, primary_value) in replica_vector.iter().zip(primary_vector.iter()) {
    assert!(
      (replica_value - primary_value).abs() <= 1e-6,
      "vector mismatch: replica={replica_value}, primary={primary_value}"
    );
  }

  primary.begin(false).expect("begin delete");
  primary
    .delete_node_vector(node, embedding_key)
    .expect("delete vector");
  let token_delete = primary
    .commit_with_token()
    .expect("commit delete")
    .expect("token delete");

  let pulled = replica.replica_catch_up_once(8).expect("catch up delete");
  assert_eq!(pulled, 1);
  let status = replica
    .replica_replication_status()
    .expect("status after delete");
  assert_eq!(status.applied_log_index, token_delete.log_index);
  assert!(
    replica.node_vector(node, embedding_key).is_none(),
    "vector delete should replicate"
  );

  close_single_file(replica).expect("close replica");
  close_single_file(primary).expect("close primary");
}
