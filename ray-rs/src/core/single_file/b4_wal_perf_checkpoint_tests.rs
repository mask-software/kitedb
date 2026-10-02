//! raydb-b4 `wal-perf` lane, finding 5: a background checkpoint's install
//! moves the post-cut records back to the start of the primary region under
//! the commit lock. It now reuses the records its replay read and checked
//! before taking the lock, and writes their copy with one positioned write.
//! The writes must stay crash safe: a crash anywhere in the install keeps
//! every acknowledged commit. Included from checkpoint.rs for its phase
//! hooks.
use super::*;
use crate::core::pager::io_hooks::{self, IoEvent};
use crate::core::single_file::{
  close_single_file, open_single_file, SingleFileOpenOptions, SyncMode,
};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn commit_node(db: &SingleFileDB, key: &str) {
  db.begin(false).expect("begin");
  db.create_node(Some(key)).expect("create node");
  db.commit().expect("commit");
}

fn apply(image: &mut Vec<u8>, offset: u64, data: &[u8]) {
  let (start, end) = (offset as usize, offset as usize + data.len());
  if image.len() < end {
    image.resize(end, 0);
  }
  image[start..end].copy_from_slice(data);
}

/// The disk after a crash at each point of `events`, from `base` (the file
/// when they started): every write so far landed, in order; and, as an OS
/// may reorder write-back between syncs, the writes before the last
/// successful sync landed but after it only the header pages' (below
/// `header_end`), the worst case for a header naming bytes not yet written.
fn crash_images(base: &[u8], events: &[IoEvent], header_end: u64) -> Vec<(String, Vec<u8>)> {
  let mut images = Vec::new();
  for cut in 0..=events.len() {
    let prefix = &events[..cut];
    let last_sync = prefix
      .iter()
      .rposition(|event| matches!(event, IoEvent::Sync { ok: true }));
    let mut in_order = base.to_vec();
    let mut reordered = base.to_vec();
    for (index, event) in prefix.iter().enumerate() {
      let IoEvent::Write { offset, data } = event else {
        continue;
      };
      apply(&mut in_order, *offset, data);
      if last_sync.is_some_and(|sync| index < sync) || *offset < header_end {
        apply(&mut reordered, *offset, data);
      }
    }
    images.push((format!("crash after {cut} events, in order"), in_order));
    images.push((
      format!("crash after {cut} events, headers ahead of data"),
      reordered,
    ));
  }
  images
}

/// Post-cut commits spanning several WAL pages; the checkpoint thread's
/// writes and syncs from the moment they are acknowledged (the install,
/// its move-back, and their headers) are applied to a copy of the file
/// taken then.
#[test]
fn f5_crash_during_install_and_move_back_keeps_every_commit() {
  let _serial = checkpoint_test_serial();
  let dir = tempdir().expect("tempdir");
  let path = dir.path().join("f5-install.kitedb");
  let options = SingleFileOpenOptions::new()
    .auto_checkpoint(false)
    .sync_mode(SyncMode::Normal);
  let db = Arc::new(open_single_file(&path, options.clone()).expect("open"));
  let header_end = 2 * db.header.read().page_size as u64;
  let pre: Vec<String> = (0..20).map(|index| format!("pre-{index}")).collect();
  for key in &pre {
    commit_node(&db, key);
  }

  let parked = Arc::new(Barrier::new(2));
  set_checkpoint_test_barrier(&db, CheckpointPhase::SnapshotDurable, Arc::clone(&parked));
  let checkpointer = {
    let db = Arc::clone(&db);
    std::thread::spawn(move || io_hooks::record_io_during(|| db.background_checkpoint()))
  };
  let deadline = Instant::now() + Duration::from_secs(10);
  while db.header.read().checkpoint_in_progress == 0 {
    assert!(Instant::now() < deadline, "the cut never happened");
    std::thread::yield_now();
  }
  let post: Vec<String> = (0..100)
    .map(|index| format!("post-{index}-{}", "p".repeat(200)))
    .collect();
  for key in &post {
    commit_node(&db, key);
  }
  let base = std::fs::read(&path).expect("base image");
  io_hooks::restart_io_log_of(checkpointer.thread().id());
  parked.wait();
  let (checkpointed, events) = checkpointer.join().expect("checkpoint thread");
  checkpointed.expect("background checkpoint");

  let stats = db.wal_stats();
  assert_eq!(
    (stats.active_region, stats.tail),
    (0, 0),
    "the post-cut records were not moved back to the primary region"
  );
  assert!(stats.primary_head > 0);
  assert!(
    events
      .iter()
      .any(|event| matches!(event, IoEvent::Write { offset, .. } if *offset >= header_end)),
    "the install wrote no WAL bytes: {events:?}"
  );
  drop(db);

  let image_path = dir.path().join("f5-crash-image.kitedb");
  for (what, image) in crash_images(&base, &events, header_end) {
    std::fs::write(&image_path, &image).expect("write image");
    let crashed = open_single_file(&image_path, options.clone())
      .unwrap_or_else(|error| panic!("{what}: unopenable: {error:?}"));
    let lost: Vec<&String> = pre
      .iter()
      .chain(&post)
      .filter(|key| crashed.node_by_key(key).is_none())
      .collect();
    close_single_file(crashed).expect("close");
    assert!(
      lost.is_empty(),
      "{what}: lost {} acknowledged commits, the first {:?}",
      lost.len(),
      lost.first()
    );
    std::fs::remove_file(&image_path).expect("remove image");
  }
}
