//! b4 cache lane: key lookups never consult the key cache.
//!
//! `CacheStats` (the public `cache_stats()`) has no key-cache counters, so this
//! crate-private test reads them from the `CacheManager` itself. Every key-cache
//! lookup counts a hit or a miss, so zero lookups after repeated `node_by_key`
//! calls means the read path never consulted it.

use crate::core::single_file::{close_single_file, open_single_file, SingleFileOpenOptions};
use crate::types::NodeId;

#[test]
fn enabled_key_cache_serves_repeated_key_lookups() {
  let dir = tempfile::tempdir().expect("tempdir");
  let path = dir.path().join("key-cache.kitedb");
  let db = open_single_file(
    &path,
    SingleFileOpenOptions::new()
      .auto_checkpoint(false)
      .enable_cache(),
  )
  .expect("open");

  db.begin(false).expect("begin");
  let nodes: Vec<NodeId> = (0..16)
    .map(|i| db.create_node(Some(&format!("k{i}"))).expect("node"))
    .collect();
  db.commit().expect("commit");

  for round in 0..100 {
    if round == 50 {
      db.checkpoint().expect("checkpoint");
    }
    for (i, &node) in nodes.iter().enumerate() {
      assert_eq!(db.node_by_key(&format!("k{i}")), Some(node));
    }
    assert_eq!(db.node_by_key("missing"), None);
  }

  let stats = db
    .cache
    .read()
    .as_ref()
    .map(|cache| cache.manager_stats())
    .expect("enabled cache");
  close_single_file(db).expect("close");

  let lookups = stats.key_cache_hits + stats.key_cache_misses;
  assert!(
    lookups > 0 && stats.key_cache_hits > 0 && stats.key_cache_size > 0,
    "cache enabled, but 1700 key lookups never consulted the key cache: \
     lookups={lookups} hits={} entries={}\n  manager_stats: {stats:?}",
    stats.key_cache_hits,
    stats.key_cache_size,
  );
}
