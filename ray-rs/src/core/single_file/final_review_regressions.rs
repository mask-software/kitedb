//! Regression tests from the final review of the background-checkpoint batch
//! (14ed498..6bf4d17), each written to fail before its fix:
//!
//! - WAL heads misread from the header of a cut taken while the WAL was
//!   empty (open failed, or replayed stale bytes of an earlier WAL cycle);
//! - a transaction's view taken between a background install's snapshot swap
//!   and its delta swap (a committed `add_edge` was lost);
//! - the smaller risks the review listed: MVCC commit order, refused vector
//!   commits, and unknown WAL record types.
//!
//! (Those about the dual-region WAL's cuts went with it: stalled-cut
//! cancellation, retained WAL compaction, unresumable and declined cuts.)
//!
//! Included from checkpoint.rs, so its test hooks are in scope. The randomized
//! crash-image stress test runs one short round by default; `SCRATCH_ITERS`,
//! `SCRATCH_MS`, `SCRATCH_CHAOS` (e.g. `bg,cp,opt,vac`), `SCRATCH_MODE`
//! (`ro`/`rw`), `SCRATCH_MVCC` (`0` runs it without MVCC; MVCC is the
//! default), `SCRATCH_GROUP`, `SCRATCH_BLOCKING`, and `SCRATCH_WAL_KIB` tune
//! it. The large-database measurement is `#[ignore]`d (see its docs).
use super::*;
use crate::core::single_file::{open_single_file, SingleFileOpenOptions};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(20);
  while !cond() {
    assert!(Instant::now() < deadline, "timed out waiting for {what}");
    std::thread::sleep(Duration::from_millis(1));
  }
}

fn crash_copy(db_path: &std::path::Path, tag: &str) -> std::path::PathBuf {
  let copy = db_path.with_extension(format!("{tag}.crash.kitedb"));
  std::fs::copy(db_path, &copy).expect("copy");
  copy
}

/// Commit `count` nodes keyed `prefix-i` in one bulk transaction, whose
/// records (kept in memory until its commit) do not fit in a 64 KiB WAL: the
/// commit goes straight to a WAL segment, and leaves the WAL empty.
fn commit_bulk_past_the_wal(db: &SingleFileDB, prefix: &str, count: usize) {
  let keys: Vec<String> = (0..count)
    .map(|index| format!("{prefix}-{index}-{}", "b".repeat(300)))
    .collect();
  let key_refs: Vec<Option<&str>> = keys.iter().map(|key| Some(key.as_str())).collect();
  db.begin_bulk().expect("begin bulk");
  db.create_nodes_batch(&key_refs).expect("create nodes");
  db.commit().expect("commit bulk");
  assert_eq!(
    db.wal_stats().primary_head,
    0,
    "setup: the WAL should be empty"
  );
}

/// A background cut taken while the WAL is empty (right after a blocking
/// checkpoint, optimize, or vacuum, then a commit too big for the WAL, which
/// went to a WAL segment), then one post-cut commit, then a crash before the
/// install.
#[test]
fn scratch_cut_with_empty_wal_then_crash_is_openable() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("scratch-empty-wal-cut.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .auto_checkpoint(false);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  commit_node(&db, "before");
  db.checkpoint().expect("blocking checkpoint");
  commit_bulk_past_the_wal(&db, "bulk", 300);

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
  let cp_db = Arc::clone(&db);
  let cp = std::thread::spawn(move || cp_db.background_checkpoint());
  wait_until("cut", || checkpoint_test_cuts(&db) > 0);
  commit_node(&db, "post-cut");
  {
    let h = db.header.read();
    eprintln!(
      "header after post-cut commit: wal_head={} primary_head={} secondary_head={} region={} marker={}",
      h.wal_head, h.wal_primary_head, h.wal_secondary_head, h.active_wal_region, h.checkpoint_in_progress
    );
  }
  let copy = crash_copy(&db_path, "a");
  parked.wait();
  cp.join().unwrap().expect("checkpoint");

  for read_only in [true, false] {
    let copy2 = crash_copy(&copy, if read_only { "ro" } else { "rw" });
    let opened = open_single_file(&copy2, options.clone().read_only(read_only));
    match opened {
      Ok(crashed) => {
        assert!(crashed.node_by_key("before").is_some());
        assert!(crashed
          .node_by_key(&format!("bulk-299-{}", "b".repeat(300)))
          .is_some());
        assert!(
          crashed.node_by_key("post-cut").is_some(),
          "post-cut lost (read_only={read_only})"
        );
      }
      Err(error) => panic!("crash copy unopenable (read_only={read_only}): {error:?}"),
    }
  }
}

fn set_prop(db: &SingleFileDB, node: NodeId, key: PropKeyId, value: &str) {
  db.begin(false).expect("begin");
  db.set_node_prop(node, key, PropValue::String(value.to_string()))
    .expect("set prop");
  db.commit().expect("commit");
}

/// Same empty-WAL cut, but no post-cut commit: the header names an empty
/// WAL. Records of earlier WAL cycles are still in the WAL's bytes.
#[test]
fn scratch_cut_with_empty_wal_crash_does_not_replay_stale_wal_bytes() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("scratch-empty-wal-stale.kitedb");
  let options = SingleFileOpenOptions::new()
    .wal_size(64 * 1024)
    .auto_checkpoint(false);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint 0");

  // Cycle 1: two same-shape transactions.
  set_prop(&db, node, key, "AAAA");
  set_prop(&db, node, key, "BBBB");
  db.checkpoint().expect("checkpoint 1");
  // Cycle 2: one transaction of the same shape overwrites only the first.
  set_prop(&db, node, key, "CCCC");
  db.checkpoint().expect("checkpoint 2");
  assert_eq!(db.wal_stats().primary_head, 0);
  assert_eq!(
    db.node_prop(node, key),
    Some(PropValue::String("CCCC".into()))
  );
  commit_bulk_past_the_wal(&db, "bulk", 300);

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
  let cp_db = Arc::clone(&db);
  let cp = std::thread::spawn(move || cp_db.background_checkpoint());
  wait_until("cut", || checkpoint_test_cuts(&db) > 0);
  let copy = crash_copy(&db_path, "stale");
  parked.wait();
  cp.join().unwrap().expect("checkpoint");

  for read_only in scratch_modes() {
    let copy2 = crash_copy(&copy, if read_only { "ro" } else { "rw" });
    let crashed = open_single_file(&copy2, options.clone().read_only(read_only))
      .unwrap_or_else(|e| panic!("open (read_only={read_only}): {e:?}"));
    let value = crashed.node_prop(node, key);
    eprintln!("read_only={read_only}: p = {value:?}");
    assert_eq!(
      value,
      Some(PropValue::String("CCCC".into())),
      "stale WAL bytes from an earlier cycle were replayed (read_only={read_only})"
    );
    if !read_only {
      drop(crashed);
      let again = open_single_file(&copy2, options.clone()).expect("reopen");
      eprintln!("after second reopen: p = {:?}", again.node_prop(node, key));
    }
  }
}

fn scratch_modes() -> Vec<bool> {
  match std::env::var("SCRATCH_MODE").as_deref() {
    Ok("rw") => vec![false],
    Ok("ro") => vec![true],
    _ => vec![true, false],
  }
}

/// Randomized: concurrent writers (toggling an edge and a sequence property
/// per thread in each transaction), long transactions carried across cuts
/// (some rolled back), auto background checkpoints on a 64 KiB WAL, and
/// atomic crash images taken under the pager lock. Every image must open,
/// contain every commit acknowledged before it was taken, and nothing rolled
/// back; the edge must agree with the sequence number (no double apply).
#[test]
fn scratch_stress_crash_images_under_concurrent_background_checkpoints() {
  use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
  use std::sync::Mutex as StdMutex;
  let _serial = checkpoint_test_serial();
  let iterations: usize = std::env::var("SCRATCH_ITERS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(1);
  for round in 0..iterations {
    let temp_dir = tempdir().expect("temp dir");
    let db_path = temp_dir
      .path()
      .join(format!("scratch-stress-{round}.kitedb"));
    let wal_kib: usize = std::env::var("SCRATCH_WAL_KIB")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(64);
    let options = SingleFileOpenOptions::new()
      .wal_size(wal_kib * 1024)
      .auto_checkpoint(std::env::var("SCRATCH_NOCP").is_err())
      .checkpoint_threshold(0.5)
      .background_checkpoint(std::env::var("SCRATCH_BLOCKING").is_err())
      .sync_mode(crate::core::single_file::SyncMode::Normal)
      .mvcc(std::env::var("SCRATCH_MVCC").map_or(true, |mode| mode != "0"))
      .mvcc_gc_interval_ms(2)
      .group_commit_enabled(std::env::var("SCRATCH_GROUP").is_ok());
    let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
    const WRITERS: usize = 4;
    db.begin(false).expect("begin");
    let etype = db.define_etype("E").expect("etype");
    let seq_key = db.define_propkey("seq").expect("propkey");
    let mut pairs = Vec::new();
    for t in 0..WRITERS {
      let a = db.create_node(Some(&format!("a-{t}"))).expect("a");
      let b = db.create_node(Some(&format!("b-{t}"))).expect("b");
      pairs.push((a, b));
    }
    db.commit().expect("commit");

    let stop = Arc::new(AtomicBool::new(false));
    let acked: Arc<Vec<AtomicU64>> = Arc::new((0..WRITERS).map(|_| AtomicU64::new(0)).collect());
    let rolled_back: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let long_committed: Arc<StdMutex<Vec<Vec<String>>>> = Arc::new(StdMutex::new(Vec::new()));
    let errors: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let mut handles = Vec::new();
    for t in 0..WRITERS {
      let db = Arc::clone(&db);
      let stop = Arc::clone(&stop);
      let acked = Arc::clone(&acked);
      let rolled_back = Arc::clone(&rolled_back);
      let errors = Arc::clone(&errors);
      let (a, b) = pairs[t];
      handles.push(std::thread::spawn(move || {
        let mut seq = 0u64;
        let mut n = 0u64;
        while !stop.load(AO::Relaxed) {
          n += 1;
          let rollback = n.is_multiple_of(9);
          let next = seq + 1;
          let r = (|| -> Result<()> {
            db.begin(false)?;
            db.set_node_prop(a, seq_key, PropValue::I64(next as i64))?;
            if next % 2 == 1 {
              db.add_edge(a, etype, b)?;
            } else {
              db.delete_edge(a, etype, b)?;
            }
            let key = if rollback {
              format!("rb-{t}-{n}")
            } else {
              format!("t-{t}-{next}")
            };
            db.create_node(Some(&format!("{key}|{}", "p".repeat(300))))?;
            if rollback {
              db.rollback()
            } else {
              db.commit()
            }
          })();
          match r {
            Ok(()) if rollback => rolled_back
              .lock()
              .unwrap()
              .push(format!("rb-{t}-{n}|{}", "p".repeat(300))),
            Ok(()) => {
              seq = next;
              acked[t].store(seq, AO::SeqCst);
            }
            Err(e) => {
              let _ = db.rollback();
              let msg = format!("writer {t} txn {n}: {e:?}");
              if !matches!(e, KiteError::WalBufferFull) {
                errors.lock().unwrap().push(msg);
              } else {
                std::thread::sleep(Duration::from_millis(1));
              }
            }
          }
        }
      }));
    }
    // Long transactions spanning cuts.
    {
      let db = Arc::clone(&db);
      let stop = Arc::clone(&stop);
      let rolled_back = Arc::clone(&rolled_back);
      let long_committed = Arc::clone(&long_committed);
      let errors = Arc::clone(&errors);
      handles.push(std::thread::spawn(move || {
        let mut n = 0;
        while !stop.load(AO::Relaxed) {
          n += 1;
          let keys: Vec<String> = (0..4)
            .map(|j| format!("long-{n}-{j}|{}", "l".repeat(200)))
            .collect();
          let r = (|| -> Result<()> {
            db.begin(false)?;
            for key in &keys {
              db.create_node(Some(key))?;
              std::thread::sleep(Duration::from_millis(3));
            }
            if n % 3 == 0 {
              db.rollback()
            } else {
              db.commit()
            }
          })();
          match r {
            Ok(()) if n % 3 == 0 => rolled_back.lock().unwrap().extend(keys),
            Ok(()) => long_committed.lock().unwrap().push(keys),
            Err(e) => {
              let _ = db.rollback();
              if !matches!(e, KiteError::WalBufferFull) {
                errors.lock().unwrap().push(format!("long {n}: {e:?}"));
              }
            }
          }
        }
      }));
    }

    if let Ok(chaos) = std::env::var("SCRATCH_CHAOS") {
      let db = Arc::clone(&db);
      let stop = Arc::clone(&stop);
      let errors = Arc::clone(&errors);
      handles.push(std::thread::spawn(move || {
        let ops: Vec<&str> = chaos
          .split(',')
          .map(|s| s.trim())
          .collect::<Vec<_>>()
          .into_iter()
          .map(|s| Box::leak(s.to_string().into_boxed_str()) as &str)
          .collect();
        let mut n = 0usize;
        while !stop.load(AO::Relaxed) {
          std::thread::sleep(Duration::from_millis(5));
          let op = ops[n % ops.len()];
          n += 1;
          let r = match op {
            "bg" => db.background_checkpoint(),
            "cp" => db.checkpoint(),
            "opt" => db.optimize_single_file(None),
            "vac" => db.vacuum_single_file(Some(crate::core::single_file::VacuumOptions {
              shrink_wal: false,
              min_wal_size: None,
            })),
            "resize" => {
              let cur = db.header.read().wal_page_count as usize * 4096;
              let next = if cur == wal_kib * 1024 {
                cur * 2
              } else {
                wal_kib * 1024
              };
              db.resize_wal(
                next,
                Some(crate::core::single_file::ResizeWalOptions {
                  allow_shrink: true,
                  checkpoint: true,
                }),
              )
            }
            _ => Ok(()),
          };
          if let Err(e) = r {
            if !matches!(e, KiteError::WalBufferFull) {
              errors.lock().unwrap().push(format!("chaos {op}: {e:?}"));
            }
          }
        }
      }));
    }

    // Crash images.
    let mut images = Vec::new();
    let started = Instant::now();
    let run_ms: u64 = std::env::var("SCRATCH_MS")
      .ok()
      .and_then(|v| v.parse().ok())
      .unwrap_or(1500);
    let mut index = 0;
    while started.elapsed() < Duration::from_millis(run_ms) {
      std::thread::sleep(Duration::from_millis(
        std::env::var("SCRATCH_IMG_MS")
          .ok()
          .and_then(|v| v.parse().ok())
          .unwrap_or(7),
      ));
      let acked_now: Vec<u64> = acked.iter().map(|v| v.load(AO::SeqCst)).collect();
      let rb_now = rolled_back.lock().unwrap().clone();
      let long_now = long_committed.lock().unwrap().clone();
      let copy = temp_dir.path().join(format!("image-{index}.kitedb"));
      {
        let _pager = db.pager.lock();
        std::fs::copy(&db_path, &copy).expect("copy image");
      }
      let acked_after: Vec<u64> = acked.iter().map(|v| v.load(AO::SeqCst)).collect();
      images.push((copy, (acked_now, acked_after), rb_now, long_now));
      index += 1;
    }
    stop.store(true, AO::Relaxed);
    for h in handles {
      h.join().expect("thread");
    }
    let errs = errors.lock().unwrap().clone();
    let final_acked: Vec<u64> = acked.iter().map(|v| v.load(AO::SeqCst)).collect();
    eprintln!(
      "round {round}: images={} acked={final_acked:?} snapshot_gen={} errors={}",
      images.len(),
      db.header.read().active_snapshot_gen,
      errs.len()
    );
    for e in errs.iter().take(5) {
      eprintln!("  error: {e}");
    }

    let check = |what: &str,
                 opened: &SingleFileDB,
                 bounds: &(Vec<u64>, Vec<u64>),
                 rb: &[String],
                 long: &[Vec<String>],
                 exact: bool|
     -> std::result::Result<(), String> {
      let acked_at = &bounds.0;
      for t in 0..WRITERS {
        let (a, b) = pairs[t];
        let v = match opened.node_prop(a, seq_key) {
          Some(PropValue::I64(v)) => v as u64,
          None => 0,
          other => return Err(format!("{what}: bad seq {other:?}")),
        };
        let lo = acked_at[t];
        let hi = if exact { lo } else { bounds.1[t] + 1 };
        if v < lo || v > hi {
          return Err(format!("{what}: writer {t} seq {v}, acked {lo}"));
        }
        let edge = opened.edge_exists(a, etype, b);
        if edge != (v % 2 == 1) {
          let diag = scratch_diag(opened, a, seq_key, v);
          return Err(format!(
            "{what}: writer {t} seq {v} but edge={edge}\n{diag}"
          ));
        }
        if lo > 0
          && opened
            .node_by_key(&format!("t-{t}-{lo}|{}", "p".repeat(300)))
            .is_none()
        {
          return Err(format!("{what}: writer {t} acked key {lo} missing"));
        }
      }
      for key in rb {
        if opened.node_by_key(key).is_some() {
          return Err(format!("{what}: rolled back {key:.16} present"));
        }
      }
      for keys in long {
        for key in keys {
          if opened.node_by_key(key).is_none() {
            return Err(format!("{what}: long committed {key:.16} missing"));
          }
        }
      }
      Ok(())
    };

    let mut failures = Vec::new();
    for (i, (copy, acked_at, rb, long)) in images.iter().enumerate() {
      let ro = open_single_file(copy, options.clone().read_only(true));
      match ro {
        Ok(opened) => {
          if let Err(e) = check(&format!("image {i} ro"), &opened, acked_at, rb, long, false) {
            failures.push(e);
          }
        }
        Err(e) => failures.push(format!("image {i} ro unopenable: {e:?}")),
      }
      match open_single_file(copy, options.clone().auto_checkpoint(false)) {
        Ok(opened) => {
          if let Err(e) = check(&format!("image {i} rw"), &opened, acked_at, rb, long, false) {
            failures.push(e);
          }
          drop(opened);
          match open_single_file(copy, options.clone().auto_checkpoint(false)) {
            Ok(again) => {
              if let Err(e) = check(
                &format!("image {i} rw-again"),
                &again,
                acked_at,
                rb,
                long,
                false,
              ) {
                failures.push(e);
              }
            }
            Err(e) => failures.push(format!("image {i} second open: {e:?}")),
          }
        }
        Err(e) => failures.push(format!("image {i} rw unopenable: {e:?}")),
      }
    }
    let rb_final = rolled_back.lock().unwrap().clone();
    let long_final = long_committed.lock().unwrap().clone();
    if let Err(e) = check(
      "live",
      &db,
      &(final_acked.clone(), final_acked.clone()),
      &rb_final,
      &long_final,
      true,
    ) {
      failures.push(e);
    }
    let db = Arc::try_unwrap(db).ok().expect("sole owner");
    drop(db);
    match open_single_file(&db_path, options.clone()) {
      Ok(reopened) => {
        if let Err(e) = check(
          "reopen",
          &reopened,
          &(final_acked.clone(), final_acked.clone()),
          &rb_final,
          &long_final,
          true,
        ) {
          failures.push(e);
        }
      }
      Err(e) => failures.push(format!("reopen: {e:?}")),
    }
    for f in failures.iter().take(10) {
      eprintln!("FAIL {f}");
    }
    assert!(
      failures.is_empty() && errs.is_empty(),
      "{} failures, {} writer errors",
      failures.len(),
      errs.len()
    );
  }
}

/// No crash: a background checkpoint stops before its install (a panic
/// here). Vacuum rebuilds the WAL buffer from the header
/// (WalBuffer::from_header), then the next background checkpoint runs; no
/// value reverts to one of an earlier WAL cycle.
#[test]
fn scratch_vacuum_after_a_stopped_background_checkpoint_keeps_values() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("scratch-vacuum-abandoned.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint 0");
  set_prop(&db, node, key, "AAAA");
  set_prop(&db, node, key, "BBBB");
  db.checkpoint().expect("checkpoint 1");
  set_prop(&db, node, key, "CCCC");
  db.checkpoint().expect("checkpoint 2");
  // A transaction of the same shape: the only record of the log.
  set_prop(&db, node, key, "DDDD");

  let cp_db = Arc::clone(&db);
  let panicked = std::thread::spawn(move || {
    set_checkpoint_test_panic(Some(CheckpointPhase::SnapshotDurable));
    cp_db.background_checkpoint()
  })
  .join();
  assert!(panicked.is_err());
  assert!(
    !db.is_checkpoint_running(),
    "the panicked run left the checkpoint running"
  );

  db.vacuum_single_file(None).expect("vacuum");
  let stats = db.wal_stats();
  eprintln!(
    "after vacuum: primary_head={} secondary_head={} region={}",
    stats.primary_head, stats.secondary_head, stats.active_region
  );
  let resumed = db.background_checkpoint();
  eprintln!("resume: {resumed:?}");
  let live = db.node_prop(node, key);
  eprintln!("live p = {live:?}");
  drop(db);
  let reopened = open_single_file(&db_path, options).expect("reopen");
  let durable = reopened.node_prop(node, key);
  eprintln!("reopened p = {durable:?}");
  assert_eq!(
    live,
    Some(PropValue::String("DDDD".into())),
    "live value reverted"
  );
  assert_eq!(
    durable,
    Some(PropValue::String("DDDD".into())),
    "durable value reverted"
  );
}

/// A transaction open across a background install must not decide an
/// add_edge from a view taken between the install's snapshot swap and its
/// delta swap: the new snapshot (which holds a pre-cut add) with the old
/// delta (where a post-cut commit removed that add). Regression: it saw the
/// edge, skipped its AddEdge record, and its committed add was lost.
#[test]
fn scratch_open_transaction_add_during_install_swap_is_not_lost() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("scratch-swap-window.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  db.begin(false).expect("begin");
  let a = db.create_node(Some("a")).expect("a");
  let b = db.create_node(Some("b")).expect("b");
  let e = db.define_etype("E").expect("etype");
  db.commit().expect("commit");
  db.checkpoint().expect("checkpoint"); // S0: a, b, no edge
  db.begin(false).expect("begin");
  db.add_edge(a, e, b).expect("pre-cut add"); // C1, pre-cut
  db.commit().expect("commit");
  commit_node(&db, "pad"); // keep the primary non-empty regardless
  let gen0 = db.header.read().active_snapshot_gen;

  let released = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::CutReleased, Arc::clone(&released));
  let cp_db = Arc::clone(&db);
  let cp = std::thread::spawn(move || cp_db.background_checkpoint());
  wait_until("cut", || checkpoint_test_cuts(&db) > 0);

  // C2, post-cut: delete the edge.
  db.begin(false).expect("begin");
  db.delete_edge(a, e, b).expect("post-cut delete");
  db.commit().expect("commit");
  assert!(!db.edge_exists(a, e, b));

  // W opens its transaction before the install.
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let (began_tx, began_rx) = mpsc::channel::<()>();
  let w_db = Arc::clone(&db);
  let writer = std::thread::spawn(move || -> Result<()> {
    w_db.begin(false)?;
    began_tx.send(()).unwrap();
    go_rx.recv().unwrap();
    w_db.add_edge(a, e, b)?;
    w_db.commit()
  });
  began_rx.recv().unwrap();

  // Hold a snapshot read guard so the install parks swapping the snapshot.
  let guard = db.snapshot.read();
  released.wait();
  wait_until("install", || {
    db.header.read().active_snapshot_gen > gen0 && db.wal_stats().active_region == 0
  });
  std::thread::sleep(Duration::from_millis(200)); // run now waits for snapshot.write()
  go_tx.send(()).unwrap(); // W takes delta.read(), queues on snapshot.read()
  std::thread::sleep(Duration::from_millis(200));
  drop(guard);

  writer.join().unwrap().expect("writer");
  cp.join().unwrap().expect("checkpoint");
  let live = db.edge_exists(a, e, b);
  let db = Arc::try_unwrap(db).ok().expect("sole owner");
  drop(db);
  let reopened = open_single_file(&db_path, options).expect("reopen");
  let durable = reopened.edge_exists(a, e, b);
  assert!(
    live && durable,
    "W's committed add_edge lost: live={live} durable={durable}"
  );
}

/// Records of the transactions that set `node`'s `key` to v-1, v, v+1, and
/// where the WAL stands.
fn scratch_diag(db: &SingleFileDB, node: NodeId, key: PropKeyId, v: u64) -> String {
  use crate::core::wal::record::parse_set_node_prop_payload;
  let h = db.header.read().clone();
  let mut out = format!(
    "  header: gen={} wal_head={} tail={} ph={} sh={} region={} marker={} snap_pages={}\n",
    h.active_snapshot_gen,
    h.wal_head,
    h.wal_tail,
    h.wal_primary_head,
    h.wal_secondary_head,
    h.active_wal_region,
    h.checkpoint_in_progress,
    h.snapshot_page_count
  );
  let records = {
    let mut pager = db.pager.lock();
    db.wal_buffer
      .lock()
      .records_for_recovery(&mut pager)
      .unwrap_or_default()
  };
  out += &format!("  wal records: {}\n", records.len());
  for want in [v.saturating_sub(1), v, v + 1] {
    let txids: Vec<TxId> = records
      .iter()
      .filter(|r| r.record_type == WalRecordType::SetNodeProp)
      .filter_map(|r| parse_set_node_prop_payload(&r.payload).map(|d| (r.txid, d)))
      .filter(|(_, d)| {
        d.node_id == node && d.key_id == key && d.value == PropValue::I64(want as i64)
      })
      .map(|(txid, _)| txid)
      .collect();
    for txid in txids {
      let kinds: Vec<String> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.txid == txid)
        .map(|(i, r)| format!("{i}:{:?}", r.record_type))
        .collect();
      out += &format!("  seq {want} tx {txid}: {}\n", kinds.join(" "));
    }
  }
  out
}

/// Measurement: a 1M-node database with the default 4 MB WAL under two
/// sustained writers and auto background checkpoints. Counts installs and
/// writer failures over SCRATCH_SECS.
#[test]
#[ignore]
fn scratch_large_db_background_checkpoints_under_sustained_writes() {
  use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
  let nodes: usize = std::env::var("SCRATCH_NODES")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(1_000_000);
  let secs: u64 = std::env::var("SCRATCH_SECS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(60);
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("scratch-large.kitedb");
  {
    let build = SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .wal_size(256 * 1024 * 1024)
      .sync_mode(crate::core::single_file::SyncMode::Normal);
    let db = open_single_file(&db_path, build).expect("open");
    db.begin(false).expect("begin");
    let e = db.define_etype("E").expect("etype");
    let name = db.define_propkey("name").expect("propkey");
    db.commit().expect("commit");
    let mut prev = None;
    let batch = 20_000;
    for chunk in 0..nodes.div_ceil(batch) {
      db.begin(false).expect("begin");
      for i in 0..batch.min(nodes - chunk * batch) {
        let n = db
          .create_node(Some(&format!("node-{chunk}-{i}")))
          .expect("node");
        db.set_node_prop(n, name, PropValue::String(format!("value-{chunk}-{i}")))
          .expect("prop");
        if let Some(p) = prev {
          db.add_edge(p, e, n).expect("edge");
        }
        prev = Some(n);
      }
      db.commit().expect("commit");
      if chunk % 10 == 9 {
        db.checkpoint().expect("checkpoint");
      }
    }
    db.checkpoint().expect("checkpoint");
    db.resize_wal(
      4 * 1024 * 1024,
      Some(crate::core::single_file::ResizeWalOptions {
        allow_shrink: true,
        checkpoint: true,
      }),
    )
    .expect("resize");
  }
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(true)
    .background_checkpoint(true)
    .checkpoint_threshold(0.5)
    .sync_mode(crate::core::single_file::SyncMode::Normal);
  let db = Arc::new(open_single_file(&db_path, options).expect("reopen"));
  let gen0 = db.header.read().active_snapshot_gen;
  eprintln!(
    "wal pages {}; start gen {gen0}",
    db.header.read().wal_page_count
  );
  let stop = Arc::new(AtomicBool::new(false));
  let ok = Arc::new(AtomicU64::new(0));
  let full = Arc::new(AtomicU64::new(0));
  // (last commit, longest interval without one)
  let gap = Arc::new(std::sync::Mutex::new((Instant::now(), Duration::ZERO)));
  let mut handles = Vec::new();
  for t in 0..2 {
    let (db, stop, ok, full) = (
      Arc::clone(&db),
      Arc::clone(&stop),
      Arc::clone(&ok),
      Arc::clone(&full),
    );
    let gap = Arc::clone(&gap);
    handles.push(std::thread::spawn(move || {
      let mut i = 0u64;
      while !stop.load(AO::Relaxed) {
        i += 1;
        let r = (|| -> Result<()> {
          db.begin(false)?;
          db.create_node(Some(&format!("w{t}-{i}-{}", "x".repeat(200))))?;
          db.commit()
        })();
        match r {
          Ok(()) => {
            ok.fetch_add(1, AO::Relaxed);
            let mut gap = gap.lock().unwrap();
            let now = Instant::now();
            gap.1 = gap.1.max(now - gap.0);
            gap.0 = now;
          }
          Err(_) => {
            let _ = db.rollback();
            full.fetch_add(1, AO::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
          }
        }
      }
    }));
  }
  let start = Instant::now();
  while start.elapsed() < Duration::from_secs(secs) {
    std::thread::sleep(Duration::from_secs(5));
    let s = db.wal_stats();
    eprintln!(
      "t={:>3}s commits={} failed={} installs={} region={} primary_head={} secondary_used={}",
      start.elapsed().as_secs(),
      ok.load(AO::Relaxed),
      full.load(AO::Relaxed),
      db.header.read().active_snapshot_gen - gen0,
      s.active_region,
      s.primary_head,
      s.secondary_head - db.wal_buffer.lock().primary_region_size()
    );
  }
  stop.store(true, AO::Relaxed);
  for h in handles {
    h.join().unwrap();
  }
  let (last, longest) = *gap.lock().unwrap();
  eprintln!(
    "total: commits={} failed={} installs={} longest interval without a commit {:?} (since the last one {:?})",
    ok.load(AO::Relaxed),
    full.load(AO::Relaxed),
    db.header.read().active_snapshot_gen - gen0,
    longest,
    last.elapsed()
  );
}

/// Commit filler nodes until the active WAL region has exactly `left` bytes
/// free (`left` a multiple of 8 below 48, so no filler transaction fits).
fn fill_wal_region_leaving(db: &SingleFileDB, left: u64) {
  // A filler transaction is BEGIN (24 bytes) + CREATE_NODE (36 + key bytes,
  // padded to 8) + COMMIT (24).
  let mut index = 0;
  loop {
    let rest = db.wal_buffer.lock().free() - left;
    if rest == 0 {
      return;
    }
    let key_len = if rest > 2400 {
      1000
    } else {
      rest as usize - 84
    };
    let key = format!("fill{index:06}{}", "f".repeat(key_len - 10));
    commit_node(db, &key);
    index += 1;
  }
}

/// A commit that waits for WAL space (here for a checkpoint to free WAL
/// segments) takes its MVCC commit timestamp only when it writes its COMMIT
/// record. Regression: the timestamp was taken before the wait, so a read
/// transaction that began during the wait had a later snapshot and then saw
/// the commit appear part-way through; commit timestamps could also run
/// against WAL and delta order.
#[test]
fn mvcc_commit_timestamp_is_taken_after_waiting_for_wal_space() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("mvcc-commit-ts.kitedb");
  let options = SingleFileOpenOptions::new().wal_size(64 * 1024).mvcc(true);
  let db = Arc::new(open_single_file(&db_path, options.clone()).expect("open"));
  // Below the checkpoint trigger: only writers waiting for space start one.
  set_wal_segment_test_limit(&db, 64 * 1024);
  db.begin(false).expect("begin");
  let node = db.create_node(Some("n")).expect("node");
  let key = db.define_propkey("p").expect("propkey");
  db.set_node_prop(node, key, PropValue::String("old".into()))
    .expect("prop");
  db.commit().expect("commit");

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
  let cp_db = Arc::clone(&db);
  let cp = std::thread::spawn(move || cp_db.background_checkpoint());
  wait_until("cut", || checkpoint_test_cuts(&db) > 0);

  let (wrote_tx, wrote_rx) = mpsc::channel::<()>();
  let (go_tx, go_rx) = mpsc::channel::<()>();
  let writer_db = Arc::clone(&db);
  let writer = std::thread::spawn(move || -> Result<()> {
    writer_db.begin(false)?;
    writer_db.set_node_prop(node, key, PropValue::String("new".into()))?;
    wrote_tx.send(()).unwrap();
    go_rx.recv().unwrap();
    writer_db.commit()
  });
  wrote_rx.recv().unwrap();
  // Fill the WAL segments to their limit (the fillers spill after the cut,
  // so the install does not free them), then leave less room in the WAL
  // than a COMMIT record (24 bytes).
  let mut filler = 0;
  while wal_segment_test_stats(&db).bytes < 64 * 1024 {
    commit_node(&db, &format!("seg{filler:06}{}", "s".repeat(1000)));
    filler += 1;
  }
  fill_wal_region_leaving(&db, 8);
  go_tx.send(()).unwrap();
  wait_until("the commit to wait for WAL segment space", || {
    writers_waiting_for_segments(&db) > 0
  });

  db.begin(true).expect("read transaction");
  assert_eq!(
    db.node_prop(node, key),
    Some(PropValue::String("old".into()))
  );
  parked.wait();
  cp.join().expect("checkpoint thread").expect("checkpoint");
  writer.join().expect("writer thread").expect("commit");
  assert_eq!(
    db.node_prop(node, key),
    Some(PropValue::String("old".into())),
    "a read transaction saw a commit that finished after it began"
  );
  db.commit().expect("end read transaction");
  assert_eq!(
    db.node_prop(node, key),
    Some(PropValue::String("new".into()))
  );
}

/// A commit refused for a vector dimension conflict is not committed in MVCC
/// either. Regression: the refusal came after MVCC marked it committed, so
/// its writes conflicted with later transactions as if they had happened.
#[test]
fn refused_vector_commit_is_not_committed_in_mvcc() {
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("mvcc-refused-vector.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .mvcc(true);
  let db = Arc::new(open_single_file(&db_path, options).expect("open"));
  db.begin(false).expect("begin");
  let embedding = db.define_propkey("embedding").expect("propkey");
  db.commit().expect("commit");

  // Refused: stages 4 dimensions before another commit fixes the store at 3.
  let (staged_tx, staged_rx) = mpsc::channel::<()>();
  let (refused_go_tx, refused_go_rx) = mpsc::channel::<()>();
  let refused_db = Arc::clone(&db);
  let refused = std::thread::spawn(move || {
    refused_db.begin(false).expect("begin");
    let node = refused_db.create_node(Some("shared")).expect("create");
    refused_db
      .set_node_vector(node, embedding, &[0.5; 4])
      .expect("stage vector");
    staged_tx.send(()).unwrap();
    refused_go_rx.recv().unwrap();
    refused_db.commit()
  });
  staged_rx.recv().unwrap();
  db.begin(false).expect("begin");
  let three = db.create_node(Some("three")).expect("create");
  db.set_node_vector(three, embedding, &[0.5; 3])
    .expect("vector");
  db.commit().expect("commit");

  // Later: began before the refused commit, writes the same key.
  let (began_tx, began_rx) = mpsc::channel::<()>();
  let (later_go_tx, later_go_rx) = mpsc::channel::<()>();
  let later_db = Arc::clone(&db);
  let later = std::thread::spawn(move || {
    later_db.begin(false).expect("begin");
    later_db.create_node(Some("shared")).expect("create");
    began_tx.send(()).unwrap();
    later_go_rx.recv().unwrap();
    later_db.commit()
  });
  began_rx.recv().unwrap();
  refused_go_tx.send(()).unwrap();
  let refused = refused.join().expect("refused thread");
  assert!(
    matches!(refused, Err(KiteError::VectorDimensionMismatch { .. })),
    "{refused:?}"
  );
  later_go_tx.send(()).unwrap();
  let later = later.join().expect("later thread");
  assert!(later.is_ok(), "conflict with a refused commit: {later:?}");
  assert!(db.node_by_key("shared").is_some());
}

/// A WAL record whose CRC checks but whose type this version does not know
/// (a newer version wrote it) is not torn: a writable open refuses rather
/// than trim it away. Regression: the open-time trim dropped it, and every
/// record after it, for good.
#[test]
fn writable_open_refuses_rather_than_trims_records_of_unknown_types() {
  use crate::util::crc::crc32;
  let _serial = checkpoint_test_serial();
  let temp_dir = tempdir().expect("temp dir");
  let db_path = temp_dir.path().join("unknown-record-type.kitedb");
  let options = SingleFileOpenOptions::new().auto_checkpoint(false);
  let db = open_single_file(&db_path, options.clone()).expect("open");
  commit_node(&db, "known");
  {
    let mut pager = db.pager.lock();
    let mut wal = db.wal_buffer.lock();
    let mut header = db.header.write();
    let mut record = crate::core::wal::record::WalRecord::new(
      WalRecordType::Begin,
      header.next_tx_id + 1,
      vec![7; 12],
    )
    .build();
    record[4] = 200; // no such record type
    let crc_end = WAL_RECORD_HEADER_SIZE + 12;
    let crc = crc32(&record[4..crc_end]);
    record[crc_end..crc_end + 4].copy_from_slice(&crc.to_le_bytes());
    wal.write_record_bytes_batch(&record).expect("append");
    wal.flush(&mut pager).expect("flush");
    wal.store_in_header(&mut header);
    db.persist_header(&mut pager, &mut header, true)
      .expect("header");
  }
  drop(db);

  let bytes = std::fs::read(&db_path).expect("read file");
  let writable = open_single_file(&db_path, options.clone());
  assert!(
    writable.is_err(),
    "a writable open accepted an unknown record type"
  );
  drop(writable);
  assert!(
    std::fs::read(&db_path).expect("read file") == bytes,
    "the writable open rewrote the file"
  );
  let read_only = open_single_file(&db_path, options.read_only(true)).expect("read-only open");
  assert!(read_only.node_by_key("known").is_some());
}
