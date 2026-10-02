use std::str::FromStr;

use kitedb::replication::log_store::{ReplicationFrame, SegmentLogStore};
use kitedb::replication::manifest::{ManifestStore, ReplicationManifest, SegmentMeta};
use kitedb::replication::types::{CommitToken, ReplicationCursor};

#[test]
fn commit_token_invalid_strings_rejected() {
  let invalid = [
    "", "1", "1:", "1:2:3", "x:1", "1:y", "-1:2", "1:-2", " 1:2", "1:2 ",
  ];

  for raw in invalid {
    assert!(
      CommitToken::from_str(raw).is_err(),
      "token should fail: {raw}"
    );
  }
}

#[test]
fn replication_cursor_invalid_strings_rejected() {
  let invalid = [
    "",
    "1:2:3",
    "1:2:3:4:5",
    "x:2:3:4",
    "1:y:3:4",
    "1:2:z:4",
    "1:2:3:w",
    "-1:2:3:4",
    "1:2:-3:4",
    "1:2:3:-4",
    "1:2:3:4 ",
  ];

  for raw in invalid {
    assert!(
      ReplicationCursor::from_str(raw).is_err(),
      "cursor should fail: {raw}"
    );
  }
}

#[test]
fn token_cursor_ordering_epoch_aware_and_monotonic() {
  let t1 = CommitToken::new(1, 41);
  let t2 = CommitToken::new(1, 42);
  let t3 = CommitToken::new(2, 1);
  assert!(t1 < t2);
  assert!(t2 < t3);

  let c1 = ReplicationCursor::new(1, 1, 100, 10);
  let c2 = ReplicationCursor::new(1, 1, 101, 10);
  let c3 = ReplicationCursor::new(1, 2, 0, 11);
  let c4 = ReplicationCursor::new(2, 0, 0, 0);
  assert!(c1 < c2);
  assert!(c2 < c3);
  assert!(c3 < c4);
}

#[test]
fn token_cursor_roundtrip_property() {
  for epoch in [0_u64, 1, 7, 1024, u16::MAX as u64] {
    for log_index in [0_u64, 1, 2, 99, 65_535] {
      let token = CommitToken::new(epoch, log_index);
      let parsed = CommitToken::from_str(&token.to_string()).expect("parse token");
      assert_eq!(parsed, token);

      let cursor = ReplicationCursor::new(epoch, epoch + 1, log_index + 2, log_index);
      let parsed = ReplicationCursor::from_str(&cursor.to_string()).expect("parse cursor");
      assert_eq!(parsed, cursor);
    }
  }
}

#[test]
fn manifest_interrupted_write_never_yields_partial_valid_state() {
  let dir = tempfile::tempdir().expect("tempdir");
  let manifest_path = dir.path().join("replication-manifest.json");
  let store = ManifestStore::new(&manifest_path);

  let baseline = ReplicationManifest {
    version: 1,
    epoch: 3,
    head_log_index: 41,
    retained_floor: 7,
    active_segment_id: 9,
    segments: vec![SegmentMeta {
      id: 9,
      start_log_index: 1,
      end_log_index: 41,
      size_bytes: 2048,
    }],
    generation: 0,
  };
  store.write(&baseline).expect("write baseline");

  let interrupted_tmp_path = manifest_path.with_extension("json.tmp");
  std::fs::write(&interrupted_tmp_path, b"{\"version\":1,\"epoch\":99")
    .expect("write interrupted temp");

  let loaded = store.read().expect("load manifest");
  assert_eq!(loaded, baseline);
}

#[test]
fn manifest_reload_after_rewrite_is_deterministic() {
  let dir = tempfile::tempdir().expect("tempdir");
  let manifest_path = dir.path().join("replication-manifest.json");
  let store = ManifestStore::new(&manifest_path);

  let first = ReplicationManifest {
    version: 1,
    epoch: 2,
    head_log_index: 10,
    retained_floor: 1,
    active_segment_id: 1,
    segments: vec![SegmentMeta {
      id: 1,
      start_log_index: 1,
      end_log_index: 10,
      size_bytes: 123,
    }],
    generation: 0,
  };
  let second = ReplicationManifest {
    version: 1,
    epoch: 2,
    head_log_index: 11,
    retained_floor: 1,
    active_segment_id: 2,
    segments: vec![
      SegmentMeta {
        id: 1,
        start_log_index: 1,
        end_log_index: 10,
        size_bytes: 123,
      },
      SegmentMeta {
        id: 2,
        start_log_index: 11,
        end_log_index: 11,
        size_bytes: 64,
      },
    ],
    generation: 0,
  };

  store.write(&first).expect("write first");
  assert_eq!(store.read().expect("read first"), first);

  store.write(&second).expect("write second");
  assert_eq!(store.read().expect("read second"), second);

  let reopened = ManifestStore::new(&manifest_path);
  assert_eq!(reopened.read().expect("read reopened"), second);
}

#[test]
fn segment_append_read_roundtrip_preserves_boundaries_indices() {
  let dir = tempfile::tempdir().expect("tempdir");
  let segment_path = dir.path().join("segment-0001.rlog");

  let mut writer = SegmentLogStore::create(&segment_path).expect("create segment");
  writer
    .append(&ReplicationFrame::new(1, 1, b"alpha".to_vec()))
    .expect("append 1");
  writer
    .append(&ReplicationFrame::new(1, 2, vec![0, 1, 2, 3]))
    .expect("append 2");
  writer
    .append(&ReplicationFrame::new(1, 3, b"omega".to_vec()))
    .expect("append 3");
  writer.sync().expect("sync");

  let reader = SegmentLogStore::open(&segment_path).expect("open reader");
  let frames = reader.read_all().expect("read all");

  assert_eq!(frames.len(), 3);
  assert_eq!(frames[0].epoch, 1);
  assert_eq!(frames[0].log_index, 1);
  assert_eq!(frames[0].payload, b"alpha");
  assert_eq!(frames[1].log_index, 2);
  assert_eq!(frames[1].payload, vec![0, 1, 2, 3]);
  assert_eq!(frames[2].log_index, 3);
  assert_eq!(frames[2].payload, b"omega");
}

#[test]
fn corrupt_segment_frame_checksum_fails_scan() {
  let dir = tempfile::tempdir().expect("tempdir");
  let segment_path = dir.path().join("segment-0002.rlog");

  let mut writer = SegmentLogStore::create(&segment_path).expect("create segment");
  writer
    .append(&ReplicationFrame::new(4, 99, b"payload".to_vec()))
    .expect("append");
  writer.sync().expect("sync");

  let mut bytes = std::fs::read(&segment_path).expect("read bytes");
  let last = bytes.len() - 1;
  bytes[last] ^= 0xFF;
  std::fs::write(&segment_path, &bytes).expect("corrupt bytes");

  let reader = SegmentLogStore::open(&segment_path).expect("open reader");
  assert!(reader.read_all().is_err(), "checksum mismatch must error");
}
