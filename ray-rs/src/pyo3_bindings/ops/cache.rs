//! Cache operations for Python bindings

use crate::core::single_file::SingleFileDB as RustSingleFileDB;
use crate::types::{ETypeId, NodeId};

use crate::pyo3_bindings::stats::CacheStats;

/// Trait for cache operations
pub trait CacheOps {
  /// Check if caching is enabled
  fn cache_is_enabled_impl(&self) -> bool;
  /// Invalidate all caches for a node
  fn cache_invalidate_node_impl(&self, node_id: i64);
  /// Invalidate caches for a specific edge
  fn cache_invalidate_edge_impl(&self, src: i64, etype: u32, dst: i64);
  /// Invalidate a cached key lookup
  fn cache_invalidate_key_impl(&self, key: &str);
  /// Clear all caches
  fn cache_clear_impl(&self);
  /// Clear only the query cache
  fn cache_clear_query_impl(&self);
  /// Clear only the key cache
  fn cache_clear_key_impl(&self);
  /// Clear only the property cache
  fn cache_clear_property_impl(&self);
  /// Clear only the traversal cache
  fn cache_clear_traversal_impl(&self);
  /// Get cache statistics
  fn cache_stats_impl(&self) -> Option<CacheStats>;
  /// Reset cache statistics
  fn cache_reset_stats_impl(&self);
}

// ============================================================================
// Single-file database operations
// ============================================================================

pub fn cache_is_enabled(db: &RustSingleFileDB) -> bool {
  db.cache_is_enabled()
}

pub fn cache_invalidate_node(db: &RustSingleFileDB, node_id: NodeId) {
  db.cache_invalidate_node(node_id);
}

pub fn cache_invalidate_edge(db: &RustSingleFileDB, src: NodeId, etype: ETypeId, dst: NodeId) {
  db.cache_invalidate_edge(src, etype, dst);
}

pub fn cache_invalidate_key(db: &RustSingleFileDB, key: &str) {
  db.cache_invalidate_key(key);
}

pub fn cache_clear(db: &RustSingleFileDB) {
  db.cache_clear();
}

pub fn cache_clear_query(db: &RustSingleFileDB) {
  db.cache_clear_query();
}

pub fn cache_clear_key(db: &RustSingleFileDB) {
  db.cache_clear_key();
}

pub fn cache_clear_property(db: &RustSingleFileDB) {
  db.cache_clear_property();
}

pub fn cache_clear_traversal(db: &RustSingleFileDB) {
  db.cache_clear_traversal();
}

pub fn cache_stats(db: &RustSingleFileDB) -> Option<CacheStats> {
  db.cache_stats().map(|s| CacheStats {
    property_cache_hits: s.property_cache_hits as i64,
    property_cache_misses: s.property_cache_misses as i64,
    property_cache_size: s.property_cache_size as i64,
    traversal_cache_hits: s.traversal_cache_hits as i64,
    traversal_cache_misses: s.traversal_cache_misses as i64,
    traversal_cache_size: s.traversal_cache_size as i64,
    query_cache_hits: s.query_cache_hits as i64,
    query_cache_misses: s.query_cache_misses as i64,
    query_cache_size: s.query_cache_size as i64,
  })
}

pub fn cache_reset_stats(db: &RustSingleFileDB) {
  db.cache_reset_stats();
}
