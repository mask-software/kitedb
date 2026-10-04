//! raydb-b4 `engine-concurrency` lane: checkpoint-gate fairness (F3),
//! deterministic form. Included from checkpoint.rs for its phase hooks; the
//! stress form is `f3_background_checkpoint_loop_does_not_starve_blocking_checkpoints`
//! in `tests/b4_engine_concurrency.rs`.
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use crate::core::single_file::{ResizeWalOptions, VacuumOptions};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

type ExclusiveOp = fn(&SingleFileDB) -> Result<()>;

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(10);
  while !cond() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

/// A blocking checkpoint (or optimize, vacuum, WAL resize) waiting for a
/// running background checkpoint must get the gate once that run ends: a
/// background checkpoint started meanwhile declines instead of claiming the
/// checkpoint status ahead of it. Today the new run claims the status before
/// taking the gate, so the waiter finds a run in progress every time it gets
/// the gate and waits again; a zero-pause background loop starves it.
///
/// Steps: background run B1 parks after its cut; the exclusive operation E
/// takes the gate once (B1 running, so E waits), then parks at its next gate
/// acquisition; B1 finishes; a new background checkpoint B2 starts while E is
/// still waiting. B2 must decline.
#[test]
fn f3_background_checkpoint_declines_while_an_exclusive_operation_waits() {
  let _serial = checkpoint_test_serial();
  let operations: [(&str, ExclusiveOp); 4] = [
    ("checkpoint", |db| db.checkpoint()),
    ("optimize", |db| db.optimize_single_file(None)),
    ("vacuum", |db| {
      db.vacuum_single_file(Some(VacuumOptions {
        shrink_wal: false,
        min_wal_size: None,
      }))
    }),
    ("resize_wal", |db| {
      db.resize_wal(
        2 * 1024 * 1024,
        Some(ResizeWalOptions {
          allow_shrink: true,
          checkpoint: true,
        }),
      )
    }),
  ];
  for (name, operation) in operations {
    let dir = tempdir().expect("tempdir");
    let db = Arc::new(
      open_single_file(
        dir.path().join("f3-gate.kitedb"),
        SingleFileOpenOptions::new()
          .auto_checkpoint(false)
          .wal_size(1024 * 1024),
      )
      .expect("open"),
    );
    commit_node(&db, "seed");

    // B1 parks after its cut, holding the checkpoint status.
    let b1_parked = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&b1_parked));
    let b1 = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || db.background_checkpoint())
    };
    wait_until("B1's cut", || checkpoint_test_cuts(&db) > 0);
    commit_node(&db, "post-cut");

    // E takes the gate, sees B1 running, and goes to wait.
    let e_first = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::GateAcquired, Arc::clone(&e_first));
    let e = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || operation(&db))
    };
    e_first.wait();
    // E's next gate acquisition can only follow B1's end.
    let e_second = Arc::new(Barrier::new(2));
    set_checkpoint_test_barrier(&db, CheckpointPhase::GateAcquired, Arc::clone(&e_second));

    b1_parked.wait();
    b1.join()
      .expect("B1 thread")
      .expect("B1 background checkpoint");

    // B2 starts while E still waits for the gate.
    let b2 = {
      let db = Arc::clone(&db);
      std::thread::spawn(move || db.background_checkpoint())
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    while !b2.is_finished() && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(1));
    }
    e_second.wait();
    let b2_result = b2.join().expect("B2 thread");
    e.join()
      .expect("exclusive thread")
      .unwrap_or_else(|error| panic!("{name} failed: {error}"));

    assert!(
      matches!(b2_result, Err(KiteError::CheckpointDeclined(_))),
      "a background checkpoint started while a blocking {name} waited for the gate, so the \
       {name} had to wait for it as well: {b2_result:?}"
    );
    for key in ["seed", "post-cut"] {
      assert!(db.node_by_key(key).is_some(), "{name}: {key} missing");
    }
    // Background checkpoints start again once nothing waits.
    db.background_checkpoint()
      .unwrap_or_else(|error| panic!("{name}: background checkpoint after the wait: {error}"));
  }
}
