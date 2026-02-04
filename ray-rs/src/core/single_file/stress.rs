//! Stress/soak tests for SingleFileDB

use std::collections::HashSet;

use super::open::{close_single_file, open_single_file, SingleFileOpenOptions, SyncMode};
use super::SingleFileDB;
use crate::error::Result;

fn open_stress_db(path: &std::path::Path) -> Result<SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new().sync_mode(SyncMode::Normal),
  )
}

fn open_soak_db(path: &std::path::Path) -> Result<SingleFileDB> {
  open_single_file(
    path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .wal_size(64 * 1024 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.7)
      .background_checkpoint(false),
  )
}

#[test]
fn test_single_file_stress_checkpoint_reopen() -> Result<()> {
  let temp_dir = tempfile::tempdir()?;
  let db_path = temp_dir.path().join("stress-single-file.kitedb");

  let mut expected_keys: HashSet<String> = HashSet::new();
  let mut next_id = 0u64;

  for cycle in 0..8 {
    let db = open_stress_db(&db_path)?;

    for _batch in 0..4 {
      db.begin(false)?;

      for _ in 0..25 {
        let key = format!("n{next_id}");
        let node_id = db.create_node(Some(&key))?;
        let _ = db.set_node_prop_by_name(
          node_id,
          "name",
          crate::types::PropValue::String(key.clone()),
        );
        expected_keys.insert(key);
        next_id += 1;
      }

      // Create a few edges per batch
      for i in 0..10 {
        let src_key = format!("n{}", next_id.saturating_sub(1 + i));
        let dst_key = format!("n{}", next_id.saturating_sub(1 + ((i + 1) % 10)));
        if let (Some(src), Some(dst)) = (db.node_by_key(&src_key), db.node_by_key(&dst_key)) {
          let _ = db.add_edge_by_name(src, "FOLLOWS", dst);
        }
      }

      db.commit()?;
    }

    if cycle % 3 == 2 {
      db.checkpoint()?;
    }

    close_single_file(db)?;

    // Reopen and validate a few invariants
    let db = open_stress_db(&db_path)?;
    for key in expected_keys.iter().take(20) {
      assert!(db.node_by_key(key).is_some());
    }
    close_single_file(db)?;
  }

  Ok(())
}

#[test]
fn test_single_file_resize_wal_stress() -> Result<()> {
  let temp_dir = tempfile::tempdir()?;
  let db_path = temp_dir.path().join("stress-resize-wal.kitedb");

  let db = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .wal_size(64 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.7)
      .background_checkpoint(false),
  )?;

  for i in 0..200 {
    db.begin(false)?;
    db.create_node(Some(&format!("pre-{i}")))?;
    db.commit()?;
  }

  db.resize_wal(8 * 1024 * 1024, None)?;
  close_single_file(db)?;

  let db = open_single_file(
    &db_path,
    SingleFileOpenOptions::new()
      .sync_mode(SyncMode::Normal)
      .wal_size(8 * 1024 * 1024)
      .auto_checkpoint(true)
      .checkpoint_threshold(0.7)
      .background_checkpoint(false),
  )?;

  for i in 0..400 {
    db.begin(false)?;
    db.create_node(Some(&format!("post-{i}")))?;
    db.commit()?;
  }

  assert!(db.node_by_key("pre-0").is_some());
  assert!(db.node_by_key("post-0").is_some());

  close_single_file(db)?;

  Ok(())
}

#[test]
#[ignore]
fn test_single_file_soak_long_run() -> Result<()> {
  let temp_dir = tempfile::tempdir()?;
  let db_path = temp_dir.path().join("soak-single-file.kitedb");

  let mut expected_keys: HashSet<String> = HashSet::new();
  let mut next_id = 0u64;

  for cycle in 0..50 {
    let db = open_soak_db(&db_path)?;

    for _batch in 0..10 {
      db.begin(false)?;
      for _ in 0..50 {
        let key = format!("n{next_id}");
        let node_id = db.create_node(Some(&key))?;
        let _ = db.set_node_prop_by_name(
          node_id,
          "name",
          crate::types::PropValue::String(key.clone()),
        );
        expected_keys.insert(key);
        next_id += 1;
      }
      db.commit()?;
    }

    if cycle % 5 == 4 {
      db.checkpoint()?;
    }

    close_single_file(db)?;
  }

  let db = open_soak_db(&db_path)?;
  assert_eq!(db.count_nodes() as usize, expected_keys.len());
  close_single_file(db)?;

  Ok(())
}
