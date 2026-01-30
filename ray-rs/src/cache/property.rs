//! Property Cache
//!
//! Caches node and edge property lookups to avoid repeated delta/snapshot reads.
//! Uses targeted invalidation via reverse index for O(1) node/edge invalidation.
//!
//! Ported from src/cache/property-cache.ts

use super::lru::LruCache;
use crate::types::{ETypeId, NodeId, PropKeyId, PropValue, PropertyCacheConfig};
use std::collections::{HashMap, HashSet};

// ============================================================================
// Cache Key Types
// ============================================================================

/// Node property cache key: (NodeId, PropKeyId)
type NodePropKey = (NodeId, PropKeyId);

/// Edge property cache key: (src, etype, dst, propKeyId)
type EdgePropKey = (NodeId, ETypeId, NodeId, PropKeyId);

/// Edge index key for reverse lookup: (src, etype, dst)
type EdgeIndexKey = (NodeId, ETypeId, NodeId);

// ============================================================================
// Property Cache Statistics
// ============================================================================

/// Property cache statistics
#[derive(Debug, Clone, Default)]
pub struct PropertyCacheStats {
  pub hits: u64,
  pub misses: u64,
  pub node_cache_size: usize,
  pub edge_cache_size: usize,
  pub max_node_cache_size: usize,
  pub max_edge_cache_size: usize,
}

impl PropertyCacheStats {
  /// Calculate hit rate
  pub fn hit_rate(&self) -> f64 {
    let total = self.hits + self.misses;
    if total > 0 {
      self.hits as f64 / total as f64
    } else {
      0.0
    }
  }

  /// Total size across both caches
  pub fn total_size(&self) -> usize {
    self.node_cache_size + self.edge_cache_size
  }
}

// ============================================================================
// Property Cache
// ============================================================================

/// Property cache for node and edge properties
///
/// Uses targeted invalidation via reverse index mapping:
/// - `node_key_index`: Maps NodeId -> Set<NodePropKey> for O(1) node invalidation
/// - `edge_key_index`: Maps (src, etype, dst) -> Set<EdgePropKey> for O(1) edge invalidation
///
/// # Example
/// ```
/// use raydb::cache::property::PropertyCache;
/// use raydb::types::{PropertyCacheConfig, PropValue};
///
/// let config = PropertyCacheConfig {
///     max_node_props: 1000,
///     max_edge_props: 1000,
/// };
/// let mut cache = PropertyCache::new(config);
///
/// // Cache a node property
/// cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
/// assert_eq!(cache.get_node_prop(1, 10), Some(&Some(PropValue::I64(42))));
///
/// // Cache miss returns None (undefined in TS)
/// assert_eq!(cache.get_node_prop(999, 10), None);
///
/// // Invalidate all props for a node
/// cache.invalidate_node(1);
/// assert_eq!(cache.get_node_prop(1, 10), None);
/// ```
pub struct PropertyCache {
  /// Node property cache: (NodeId, PropKeyId) -> PropValue or None (deleted)
  node_cache: LruCache<NodePropKey, Option<PropValue>>,

  /// Edge property cache: (src, etype, dst, propKeyId) -> PropValue or None
  edge_cache: LruCache<EdgePropKey, Option<PropValue>>,

  /// Reverse index: NodeId -> Set of cached property keys for that node
  node_key_index: HashMap<NodeId, HashSet<NodePropKey>>,

  /// Reverse index: EdgeIndexKey -> Set of cached property keys for that edge
  edge_key_index: HashMap<EdgeIndexKey, HashSet<EdgePropKey>>,

  /// Cache statistics
  hits: u64,
  misses: u64,
}

impl PropertyCache {
  /// Create a new property cache with the given configuration
  pub fn new(config: PropertyCacheConfig) -> Self {
    Self {
      node_cache: LruCache::new(config.max_node_props),
      edge_cache: LruCache::new(config.max_edge_props),
      node_key_index: HashMap::new(),
      edge_key_index: HashMap::new(),
      hits: 0,
      misses: 0,
    }
  }

  // ========================================================================
  // Node Property API
  // ========================================================================

  /// Get a node property from cache
  ///
  /// Returns:
  /// - `Some(&Some(value))` - Property exists with value
  /// - `Some(&None)` - Property was explicitly deleted/null
  /// - `None` - Not in cache (cache miss)
  pub fn get_node_prop(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    let key = (node_id, prop_key_id);
    let result = self.node_cache.get(&key);

    if result.is_some() {
      self.hits += 1;
    } else {
      self.misses += 1;
    }

    result
  }

  /// Peek at a node property without affecting LRU order
  pub fn peek_node_prop(
    &self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    let key = (node_id, prop_key_id);
    self.node_cache.peek(&key)
  }

  /// Set a node property in cache
  ///
  /// Pass `None` to cache a "property does not exist" result.
  pub fn set_node_prop(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
  ) {
    let key = (node_id, prop_key_id);
    self.node_cache.set(key, value);

    // Track which keys belong to this node for targeted invalidation
    self.node_key_index.entry(node_id).or_default().insert(key);
  }

  /// Invalidate all cached properties for a node
  ///
  /// Complexity: O(k) where k = number of cached props for this node
  pub fn invalidate_node(&mut self, node_id: NodeId) {
    if let Some(keys) = self.node_key_index.remove(&node_id) {
      for key in keys {
        self.node_cache.delete(&key);
      }
    }
  }

  // ========================================================================
  // Edge Property API
  // ========================================================================

  /// Get an edge property from cache
  ///
  /// Returns:
  /// - `Some(&Some(value))` - Property exists with value
  /// - `Some(&None)` - Property was explicitly deleted/null
  /// - `None` - Not in cache (cache miss)
  pub fn get_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    let key = (src, etype, dst, prop_key_id);
    let result = self.edge_cache.get(&key);

    if result.is_some() {
      self.hits += 1;
    } else {
      self.misses += 1;
    }

    result
  }

  /// Peek at an edge property without affecting LRU order
  pub fn peek_edge_prop(
    &self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    let key = (src, etype, dst, prop_key_id);
    self.edge_cache.peek(&key)
  }

  /// Set an edge property in cache
  ///
  /// Pass `None` to cache a "property does not exist" result.
  pub fn set_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
  ) {
    let key = (src, etype, dst, prop_key_id);
    self.edge_cache.set(key, value);

    // Track which keys belong to this edge for targeted invalidation
    let edge_index_key = (src, etype, dst);
    self
      .edge_key_index
      .entry(edge_index_key)
      .or_default()
      .insert(key);
  }

  /// Invalidate all cached properties for an edge
  ///
  /// Complexity: O(k) where k = number of cached props for this edge
  pub fn invalidate_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    let edge_index_key = (src, etype, dst);
    if let Some(keys) = self.edge_key_index.remove(&edge_index_key) {
      for key in keys {
        self.edge_cache.delete(&key);
      }
    }
  }

  // ========================================================================
  // Utility API
  // ========================================================================

  /// Clear all cached properties
  pub fn clear(&mut self) {
    self.node_cache.clear();
    self.edge_cache.clear();
    self.node_key_index.clear();
    self.edge_key_index.clear();
    self.hits = 0;
    self.misses = 0;
  }

  /// Get cache statistics
  pub fn stats(&self) -> PropertyCacheStats {
    PropertyCacheStats {
      hits: self.hits,
      misses: self.misses,
      node_cache_size: self.node_cache.len(),
      edge_cache_size: self.edge_cache.len(),
      max_node_cache_size: self.node_cache.max_size(),
      max_edge_cache_size: self.edge_cache.max_size(),
    }
  }

  /// Get current node cache size
  pub fn node_cache_size(&self) -> usize {
    self.node_cache.len()
  }

  /// Get current edge cache size
  pub fn edge_cache_size(&self) -> usize {
    self.edge_cache.len()
  }

  /// Check if node cache is empty
  pub fn is_node_cache_empty(&self) -> bool {
    self.node_cache.is_empty()
  }

  /// Check if edge cache is empty
  pub fn is_edge_cache_empty(&self) -> bool {
    self.edge_cache.is_empty()
  }

  /// Reset statistics counters
  pub fn reset_stats(&mut self) {
    self.hits = 0;
    self.misses = 0;
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn make_cache() -> PropertyCache {
    PropertyCache::new(PropertyCacheConfig {
      max_node_props: 100,
      max_edge_props: 100,
    })
  }

  #[test]
  fn test_new_cache() {
    let cache = make_cache();
    assert!(cache.is_node_cache_empty());
    assert!(cache.is_edge_cache_empty());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_node_prop_cache_miss() {
    let mut cache = make_cache();
    assert_eq!(cache.get_node_prop(1, 10), None);
    assert_eq!(cache.stats().misses, 1);
  }

  #[test]
  fn test_node_prop_cache_hit() {
    let mut cache = make_cache();
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));

    let result = cache.get_node_prop(1, 10);
    assert_eq!(result, Some(&Some(PropValue::I64(42))));
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_node_prop_null_value() {
    let mut cache = make_cache();
    // Cache a "property does not exist" result
    cache.set_node_prop(1, 10, None);

    let result = cache.get_node_prop(1, 10);
    assert_eq!(result, Some(&None));
    assert_eq!(cache.stats().hits, 1);
  }

  #[test]
  fn test_node_prop_update() {
    let mut cache = make_cache();
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_node_prop(1, 10, Some(PropValue::I64(100)));

    let result = cache.get_node_prop(1, 10);
    assert_eq!(result, Some(&Some(PropValue::I64(100))));
    assert_eq!(cache.node_cache_size(), 1);
  }

  #[test]
  fn test_node_invalidation() {
    let mut cache = make_cache();

    // Cache multiple props for same node
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_node_prop(1, 20, Some(PropValue::String("hello".to_string())));
    cache.set_node_prop(1, 30, Some(PropValue::Bool(true)));

    // Cache prop for different node
    cache.set_node_prop(2, 10, Some(PropValue::I64(100)));

    assert_eq!(cache.node_cache_size(), 4);

    // Invalidate node 1
    cache.invalidate_node(1);

    // Node 1 props should be gone
    assert_eq!(cache.get_node_prop(1, 10), None);
    assert_eq!(cache.get_node_prop(1, 20), None);
    assert_eq!(cache.get_node_prop(1, 30), None);

    // Node 2 prop should still exist
    assert_eq!(cache.get_node_prop(2, 10), Some(&Some(PropValue::I64(100))));

    assert_eq!(cache.node_cache_size(), 1);
  }

  #[test]
  fn test_edge_prop_cache_miss() {
    let mut cache = make_cache();
    assert_eq!(cache.get_edge_prop(1, 1, 2, 10), None);
    assert_eq!(cache.stats().misses, 1);
  }

  #[test]
  fn test_edge_prop_cache_hit() {
    let mut cache = make_cache();
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::F64(3.14)));

    let result = cache.get_edge_prop(1, 1, 2, 10);
    assert_eq!(result, Some(&Some(PropValue::F64(3.14))));
    assert_eq!(cache.stats().hits, 1);
  }

  #[test]
  fn test_edge_prop_null_value() {
    let mut cache = make_cache();
    cache.set_edge_prop(1, 1, 2, 10, None);

    let result = cache.get_edge_prop(1, 1, 2, 10);
    assert_eq!(result, Some(&None));
  }

  #[test]
  fn test_edge_invalidation() {
    let mut cache = make_cache();

    // Cache multiple props for same edge
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(42)));
    cache.set_edge_prop(1, 1, 2, 20, Some(PropValue::String("weight".to_string())));

    // Cache prop for different edge
    cache.set_edge_prop(1, 2, 3, 10, Some(PropValue::I64(100)));

    assert_eq!(cache.edge_cache_size(), 3);

    // Invalidate edge (1, 1, 2)
    cache.invalidate_edge(1, 1, 2);

    // Edge (1,1,2) props should be gone
    assert_eq!(cache.get_edge_prop(1, 1, 2, 10), None);
    assert_eq!(cache.get_edge_prop(1, 1, 2, 20), None);

    // Edge (1,2,3) prop should still exist
    assert_eq!(
      cache.get_edge_prop(1, 2, 3, 10),
      Some(&Some(PropValue::I64(100)))
    );

    assert_eq!(cache.edge_cache_size(), 1);
  }

  #[test]
  fn test_clear() {
    let mut cache = make_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(100)));

    // Generate some hits/misses
    cache.get_node_prop(1, 10);
    cache.get_node_prop(999, 10);

    cache.clear();

    assert!(cache.is_node_cache_empty());
    assert!(cache.is_edge_cache_empty());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_peek_does_not_update_lru() {
    let mut cache = PropertyCache::new(PropertyCacheConfig {
      max_node_props: 3,
      max_edge_props: 3,
    });

    cache.set_node_prop(1, 10, Some(PropValue::I64(1)));
    cache.set_node_prop(2, 10, Some(PropValue::I64(2)));
    cache.set_node_prop(3, 10, Some(PropValue::I64(3)));

    // Peek at node 1 (should NOT make it most recently used)
    cache.peek_node_prop(1, 10);

    // Add node 4 - should evict node 1 (oldest)
    cache.set_node_prop(4, 10, Some(PropValue::I64(4)));

    assert_eq!(cache.peek_node_prop(1, 10), None);
    assert!(cache.peek_node_prop(2, 10).is_some());
    assert!(cache.peek_node_prop(3, 10).is_some());
    assert!(cache.peek_node_prop(4, 10).is_some());
  }

  #[test]
  fn test_get_updates_lru() {
    let mut cache = PropertyCache::new(PropertyCacheConfig {
      max_node_props: 3,
      max_edge_props: 3,
    });

    cache.set_node_prop(1, 10, Some(PropValue::I64(1)));
    cache.set_node_prop(2, 10, Some(PropValue::I64(2)));
    cache.set_node_prop(3, 10, Some(PropValue::I64(3)));

    // Get node 1 (SHOULD make it most recently used)
    cache.get_node_prop(1, 10);

    // Add node 4 - should evict node 2 (oldest after node 1 was accessed)
    cache.set_node_prop(4, 10, Some(PropValue::I64(4)));

    assert!(cache.peek_node_prop(1, 10).is_some());
    assert_eq!(cache.peek_node_prop(2, 10), None);
    assert!(cache.peek_node_prop(3, 10).is_some());
    assert!(cache.peek_node_prop(4, 10).is_some());
  }

  #[test]
  fn test_stats() {
    let mut cache = make_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(100)));

    // 2 hits
    cache.get_node_prop(1, 10);
    cache.get_edge_prop(1, 1, 2, 10);

    // 2 misses
    cache.get_node_prop(999, 10);
    cache.get_edge_prop(999, 1, 2, 10);

    let stats = cache.stats();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.hit_rate(), 0.5);
    assert_eq!(stats.node_cache_size, 1);
    assert_eq!(stats.edge_cache_size, 1);
    assert_eq!(stats.total_size(), 2);
  }

  #[test]
  fn test_reset_stats() {
    let mut cache = make_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.get_node_prop(1, 10);
    cache.get_node_prop(999, 10);

    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().misses, 1);

    cache.reset_stats();

    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);

    // Cache should still have data
    assert_eq!(cache.node_cache_size(), 1);
  }

  #[test]
  fn test_all_prop_value_types() {
    let mut cache = make_cache();

    cache.set_node_prop(1, 1, Some(PropValue::Null));
    cache.set_node_prop(1, 2, Some(PropValue::Bool(true)));
    cache.set_node_prop(1, 3, Some(PropValue::I64(-123)));
    cache.set_node_prop(1, 4, Some(PropValue::F64(3.14159)));
    cache.set_node_prop(1, 5, Some(PropValue::String("hello world".to_string())));
    cache.set_node_prop(1, 6, Some(PropValue::VectorF32(vec![1.0, 2.0, 3.0])));

    assert_eq!(cache.get_node_prop(1, 1), Some(&Some(PropValue::Null)));
    assert_eq!(
      cache.get_node_prop(1, 2),
      Some(&Some(PropValue::Bool(true)))
    );
    assert_eq!(cache.get_node_prop(1, 3), Some(&Some(PropValue::I64(-123))));
    assert_eq!(
      cache.get_node_prop(1, 4),
      Some(&Some(PropValue::F64(3.14159)))
    );
    assert_eq!(
      cache.get_node_prop(1, 5),
      Some(&Some(PropValue::String("hello world".to_string())))
    );
    assert_eq!(
      cache.get_node_prop(1, 6),
      Some(&Some(PropValue::VectorF32(vec![1.0, 2.0, 3.0])))
    );
  }

  #[test]
  fn test_invalidate_nonexistent_node() {
    let mut cache = make_cache();
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));

    // Should not panic or affect other entries
    cache.invalidate_node(999);

    assert_eq!(cache.get_node_prop(1, 10), Some(&Some(PropValue::I64(42))));
  }

  #[test]
  fn test_invalidate_nonexistent_edge() {
    let mut cache = make_cache();
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(42)));

    // Should not panic or affect other entries
    cache.invalidate_edge(999, 1, 2);

    assert_eq!(
      cache.get_edge_prop(1, 1, 2, 10),
      Some(&Some(PropValue::I64(42)))
    );
  }
}
