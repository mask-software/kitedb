//! Cache Manager
//!
//! Coordinates property cache, traversal cache, query cache, and key lookup cache.
//! Provides unified invalidation and statistics APIs.
//!
//! Ported from src/cache/index.ts

use super::lru::LruCache;
use super::property::PropertyCache;
use super::query::QueryCache;
use super::traversal::{CachedNeighbors, TraversalCache, TraversalDirection};
use crate::types::{CacheOptions, CacheStats, ETypeId, Edge, NodeId, PropKeyId, PropValue};

#[cfg(test)]
use crate::types::{PropertyCacheConfig, QueryCacheConfig, TraversalCacheConfig};

// ============================================================================
// Default Configuration
// ============================================================================

const DEFAULT_KEY_CACHE_SIZE: usize = 10000;

// ============================================================================
// Cache Manager Statistics
// ============================================================================

/// Combined statistics for all caches
#[derive(Debug, Clone, Default)]
pub struct CacheManagerStats {
  /// Property cache stats
  pub property_cache_hits: u64,
  pub property_cache_misses: u64,
  pub property_cache_size: usize,
  pub property_cache_max_size: usize,

  /// Traversal cache stats  
  pub traversal_cache_hits: u64,
  pub traversal_cache_misses: u64,
  pub traversal_cache_size: usize,
  pub traversal_cache_max_size: usize,

  /// Query cache stats
  pub query_cache_hits: u64,
  pub query_cache_misses: u64,
  pub query_cache_size: usize,
  pub query_cache_max_size: usize,

  /// Key cache stats
  pub key_cache_hits: u64,
  pub key_cache_misses: u64,
  pub key_cache_size: usize,
  pub key_cache_max_size: usize,
}

impl CacheManagerStats {
  /// Total hits across all caches
  pub fn total_hits(&self) -> u64 {
    self.property_cache_hits
      + self.traversal_cache_hits
      + self.query_cache_hits
      + self.key_cache_hits
  }

  /// Total misses across all caches
  pub fn total_misses(&self) -> u64 {
    self.property_cache_misses
      + self.traversal_cache_misses
      + self.query_cache_misses
      + self.key_cache_misses
  }

  /// Overall hit rate
  pub fn hit_rate(&self) -> f64 {
    let total = self.total_hits() + self.total_misses();
    if total > 0 {
      self.total_hits() as f64 / total as f64
    } else {
      0.0
    }
  }

  /// Convert to the simpler CacheStats type
  pub fn to_cache_stats(&self) -> CacheStats {
    CacheStats {
      property_cache_hits: self.property_cache_hits,
      property_cache_misses: self.property_cache_misses,
      property_cache_size: self.property_cache_size,
      traversal_cache_hits: self.traversal_cache_hits,
      traversal_cache_misses: self.traversal_cache_misses,
      traversal_cache_size: self.traversal_cache_size,
      query_cache_hits: self.query_cache_hits,
      query_cache_misses: self.query_cache_misses,
      query_cache_size: self.query_cache_size,
    }
  }
}

// ============================================================================
// Cache Manager
// ============================================================================

/// Cache manager coordinating all caches
///
/// Provides unified access to:
/// - Property cache: node and edge property lookups
/// - Traversal cache: neighbor traversal results
/// - Query cache: complex query results with optional TTL
/// - Key cache: string key -> NodeId lookups
///
/// # Example
/// ```
/// use raydb::cache::manager::CacheManager;
/// use raydb::types::{CacheOptions, PropValue};
///
/// let mut cache = CacheManager::new(CacheOptions {
///     enabled: true,
///     ..Default::default()
/// });
///
/// // Property cache operations
/// cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
/// assert_eq!(cache.get_node_prop(1, 10), Some(&Some(PropValue::I64(42))));
///
/// // Invalidation
/// cache.invalidate_node(1);
/// assert_eq!(cache.get_node_prop(1, 10), None);
/// ```
pub struct CacheManager {
  /// Property cache for node/edge properties
  property_cache: PropertyCache,

  /// Traversal cache for neighbor lookups
  traversal_cache: TraversalCache,

  /// Query cache for complex query results
  query_cache: QueryCache,

  /// Key lookup cache: string key -> NodeId (or None for negative cache)
  key_cache: LruCache<String, Option<NodeId>>,

  /// Key cache statistics (tracked separately since LruCache doesn't track)
  key_cache_hits: u64,
  key_cache_misses: u64,

  /// Whether caching is enabled
  enabled: bool,
}

impl CacheManager {
  /// Create a new cache manager with the given options
  pub fn new(options: CacheOptions) -> Self {
    let enabled = options.enabled;

    let prop_config = options.property_cache.unwrap_or_default();
    let trav_config = options.traversal_cache.unwrap_or_default();
    let query_config = options.query_cache.unwrap_or_default();

    Self {
      property_cache: PropertyCache::new(prop_config),
      traversal_cache: TraversalCache::new(trav_config),
      query_cache: QueryCache::new(query_config),
      key_cache: LruCache::new(DEFAULT_KEY_CACHE_SIZE),
      key_cache_hits: 0,
      key_cache_misses: 0,
      enabled,
    }
  }

  /// Create a cache manager with custom key cache size
  pub fn with_key_cache_size(options: CacheOptions, key_cache_size: usize) -> Self {
    let enabled = options.enabled;

    let prop_config = options.property_cache.unwrap_or_default();
    let trav_config = options.traversal_cache.unwrap_or_default();
    let query_config = options.query_cache.unwrap_or_default();

    Self {
      property_cache: PropertyCache::new(prop_config),
      traversal_cache: TraversalCache::new(trav_config),
      query_cache: QueryCache::new(query_config),
      key_cache: LruCache::new(key_cache_size),
      key_cache_hits: 0,
      key_cache_misses: 0,
      enabled,
    }
  }

  /// Create a disabled cache manager (all operations are no-ops)
  pub fn disabled() -> Self {
    Self::new(CacheOptions {
      enabled: false,
      ..Default::default()
    })
  }

  /// Check if caching is enabled
  pub fn is_enabled(&self) -> bool {
    self.enabled
  }

  // ========================================================================
  // Property Cache API
  // ========================================================================

  /// Get a node property from cache
  pub fn get_node_prop(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    if !self.enabled {
      return None;
    }
    self.property_cache.get_node_prop(node_id, prop_key_id)
  }

  /// Set a node property in cache
  pub fn set_node_prop(
    &mut self,
    node_id: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
  ) {
    if !self.enabled {
      return;
    }
    self
      .property_cache
      .set_node_prop(node_id, prop_key_id, value);
  }

  /// Get an edge property from cache
  pub fn get_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
  ) -> Option<&Option<PropValue>> {
    if !self.enabled {
      return None;
    }
    self
      .property_cache
      .get_edge_prop(src, etype, dst, prop_key_id)
  }

  /// Set an edge property in cache
  pub fn set_edge_prop(
    &mut self,
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
    prop_key_id: PropKeyId,
    value: Option<PropValue>,
  ) {
    if !self.enabled {
      return;
    }
    self
      .property_cache
      .set_edge_prop(src, etype, dst, prop_key_id, value);
  }

  // ========================================================================
  // Traversal Cache API
  // ========================================================================

  /// Get cached neighbors for a node
  pub fn get_traversal(
    &mut self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
  ) -> Option<&CachedNeighbors> {
    if !self.enabled {
      return None;
    }
    self.traversal_cache.get(node_id, etype, direction)
  }

  /// Set cached neighbors for a node
  pub fn set_traversal(
    &mut self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
    neighbors: Vec<Edge>,
  ) {
    if !self.enabled {
      return;
    }
    self
      .traversal_cache
      .set(node_id, etype, direction, neighbors);
  }

  // ========================================================================
  // Query Cache API
  // ========================================================================

  /// Get a cached query result
  pub fn get_query<T: 'static>(&mut self, query_key: &str) -> Option<&T> {
    if !self.enabled {
      return None;
    }
    self.query_cache.get(query_key)
  }

  /// Set a query result in cache
  pub fn set_query<T: 'static + Send>(&mut self, query_key: String, value: T) {
    if !self.enabled {
      return;
    }
    self.query_cache.set(query_key, value);
  }

  /// Generate a cache key from query parameters
  pub fn generate_query_key<K, V>(&self, params: impl IntoIterator<Item = (K, V)>) -> String
  where
    K: AsRef<str> + Ord,
    V: std::fmt::Debug,
  {
    super::query::generate_key_params(params)
  }

  // ========================================================================
  // Key Lookup Cache API
  // ========================================================================

  /// Get a node ID from cache by key
  ///
  /// Returns:
  /// - `Some(&Some(node_id))` - Key found, maps to node_id
  /// - `Some(&None)` - Key was looked up but not found (negative cache)
  /// - `None` - Not in cache
  pub fn get_node_by_key(&mut self, key: &str) -> Option<&Option<NodeId>> {
    if !self.enabled {
      return None;
    }
    let result = self.key_cache.get(key);
    if result.is_some() {
      self.key_cache_hits += 1;
    } else {
      self.key_cache_misses += 1;
    }
    result
  }

  /// Set a node ID in cache by key
  ///
  /// Pass `None` to cache a "not found" result (negative cache).
  pub fn set_node_by_key(&mut self, key: String, node_id: Option<NodeId>) {
    if !self.enabled {
      return;
    }
    self.key_cache.set(key, node_id);
  }

  /// Invalidate a cached key lookup
  pub fn invalidate_key(&mut self, key: &str) {
    if !self.enabled {
      return;
    }
    self.key_cache.delete(key);
  }

  // ========================================================================
  // Invalidation API
  // ========================================================================

  /// Invalidate all caches for a node
  ///
  /// This invalidates:
  /// - All property cache entries for the node
  /// - All traversal cache entries involving the node
  pub fn invalidate_node(&mut self, node_id: NodeId) {
    if !self.enabled {
      return;
    }
    self.property_cache.invalidate_node(node_id);
    self.traversal_cache.invalidate_node(node_id);
    // Query cache is not invalidated by node (queries are content-addressed)
    // Key cache is not invalidated here - use invalidate_key() instead
  }

  /// Invalidate caches for a specific edge
  ///
  /// This invalidates:
  /// - All property cache entries for the edge
  /// - Relevant traversal cache entries (outgoing from src, incoming to dst)
  pub fn invalidate_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    if !self.enabled {
      return;
    }
    self.property_cache.invalidate_edge(src, etype, dst);
    self.traversal_cache.invalidate_edge(src, etype, dst);
    // Query cache is not invalidated by edge
  }

  /// Clear all caches
  pub fn clear(&mut self) {
    if !self.enabled {
      return;
    }
    self.property_cache.clear();
    self.traversal_cache.clear();
    self.query_cache.clear();
    self.key_cache.clear();
    self.key_cache_hits = 0;
    self.key_cache_misses = 0;
  }

  /// Clear only the query cache (useful for manual invalidation)
  pub fn clear_query_cache(&mut self) {
    if !self.enabled {
      return;
    }
    self.query_cache.clear();
  }

  /// Clear only the key cache (useful after checkpoint)
  pub fn clear_key_cache(&mut self) {
    if !self.enabled {
      return;
    }
    self.key_cache.clear();
    self.key_cache_hits = 0;
    self.key_cache_misses = 0;
  }

  /// Clear only the property cache
  pub fn clear_property_cache(&mut self) {
    if !self.enabled {
      return;
    }
    self.property_cache.clear();
  }

  /// Clear only the traversal cache
  pub fn clear_traversal_cache(&mut self) {
    if !self.enabled {
      return;
    }
    self.traversal_cache.clear();
  }

  // ========================================================================
  // Statistics API
  // ========================================================================

  /// Get detailed statistics for all caches
  pub fn stats(&self) -> CacheManagerStats {
    let prop_stats = self.property_cache.stats();
    let trav_stats = self.traversal_cache.stats();
    let query_stats = self.query_cache.stats();

    CacheManagerStats {
      property_cache_hits: prop_stats.hits,
      property_cache_misses: prop_stats.misses,
      property_cache_size: prop_stats.node_cache_size + prop_stats.edge_cache_size,
      property_cache_max_size: prop_stats.max_node_cache_size + prop_stats.max_edge_cache_size,

      traversal_cache_hits: trav_stats.hits,
      traversal_cache_misses: trav_stats.misses,
      traversal_cache_size: trav_stats.cache_size,
      traversal_cache_max_size: trav_stats.max_cache_size,

      query_cache_hits: query_stats.hits,
      query_cache_misses: query_stats.misses,
      query_cache_size: query_stats.cache_size,
      query_cache_max_size: query_stats.max_cache_size,

      key_cache_hits: self.key_cache_hits,
      key_cache_misses: self.key_cache_misses,
      key_cache_size: self.key_cache.len(),
      key_cache_max_size: self.key_cache.max_size(),
    }
  }

  /// Get simple cache statistics (for CacheStats type)
  pub fn get_stats(&self) -> CacheStats {
    self.stats().to_cache_stats()
  }

  /// Reset all statistics counters
  pub fn reset_stats(&mut self) {
    self.property_cache.reset_stats();
    self.traversal_cache.reset_stats();
    self.query_cache.reset_stats();
    self.key_cache_hits = 0;
    self.key_cache_misses = 0;
  }
}

impl Default for CacheManager {
  fn default() -> Self {
    Self::new(CacheOptions::default())
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn make_enabled_cache() -> CacheManager {
    CacheManager::new(CacheOptions {
      enabled: true,
      property_cache: Some(PropertyCacheConfig {
        max_node_props: 100,
        max_edge_props: 100,
      }),
      traversal_cache: Some(TraversalCacheConfig {
        max_entries: 100,
        max_neighbors_per_entry: 10,
      }),
      query_cache: Some(QueryCacheConfig {
        max_entries: 100,
        ttl_ms: None,
      }),
    })
  }

  #[test]
  fn test_new_enabled() {
    let cache = make_enabled_cache();
    assert!(cache.is_enabled());
  }

  #[test]
  fn test_disabled() {
    let cache = CacheManager::disabled();
    assert!(!cache.is_enabled());
  }

  #[test]
  fn test_default() {
    let cache = CacheManager::default();
    // Default should have enabled: false based on CacheOptions::default()
    assert!(!cache.is_enabled());
  }

  #[test]
  fn test_disabled_noop() {
    let mut cache = CacheManager::disabled();

    // All operations should be no-ops
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    assert_eq!(cache.get_node_prop(1, 10), None);

    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(100)));
    assert_eq!(cache.get_edge_prop(1, 1, 2, 10), None);

    cache.set_traversal(1, Some(1), TraversalDirection::Out, vec![]);
    assert!(cache
      .get_traversal(1, Some(1), TraversalDirection::Out)
      .is_none());

    cache.set_query("key".to_string(), 42i32);
    let result: Option<&i32> = cache.get_query("key");
    assert!(result.is_none());

    cache.set_node_by_key("alice".to_string(), Some(1));
    assert_eq!(cache.get_node_by_key("alice"), None);
  }

  #[test]
  fn test_property_cache() {
    let mut cache = make_enabled_cache();

    // Node properties
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    assert_eq!(cache.get_node_prop(1, 10), Some(&Some(PropValue::I64(42))));

    // Edge properties
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::String("weight".to_string())));
    assert_eq!(
      cache.get_edge_prop(1, 1, 2, 10),
      Some(&Some(PropValue::String("weight".to_string())))
    );
  }

  #[test]
  fn test_traversal_cache() {
    let mut cache = make_enabled_cache();

    let neighbors = vec![
      Edge {
        src: 1,
        etype: 1,
        dst: 2,
      },
      Edge {
        src: 1,
        etype: 1,
        dst: 3,
      },
    ];

    cache.set_traversal(1, Some(1), TraversalDirection::Out, neighbors.clone());

    let result = cache.get_traversal(1, Some(1), TraversalDirection::Out);
    assert!(result.is_some());
    assert_eq!(result.unwrap().neighbors.len(), 2);
  }

  #[test]
  fn test_query_cache() {
    let mut cache = make_enabled_cache();

    cache.set_query("query1".to_string(), vec![1u64, 2, 3]);

    let result: Option<&Vec<u64>> = cache.get_query("query1");
    assert!(result.is_some());
    assert_eq!(result.unwrap(), &vec![1, 2, 3]);
  }

  #[test]
  fn test_key_cache() {
    let mut cache = make_enabled_cache();

    // Cache a key -> node mapping
    cache.set_node_by_key("alice".to_string(), Some(1));
    assert_eq!(cache.get_node_by_key("alice"), Some(&Some(1)));

    // Cache a negative result (key not found)
    cache.set_node_by_key("nonexistent".to_string(), None);
    assert_eq!(cache.get_node_by_key("nonexistent"), Some(&None));

    // Cache miss
    assert_eq!(cache.get_node_by_key("unknown"), None);
  }

  #[test]
  fn test_invalidate_node() {
    let mut cache = make_enabled_cache();

    // Set up cache entries
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_node_prop(1, 20, Some(PropValue::Bool(true)));
    cache.set_traversal(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![Edge {
        src: 1,
        etype: 1,
        dst: 2,
      }],
    );

    // Also cache for another node
    cache.set_node_prop(2, 10, Some(PropValue::I64(100)));

    // Invalidate node 1
    cache.invalidate_node(1);

    // Node 1 caches should be gone
    assert_eq!(cache.get_node_prop(1, 10), None);
    assert_eq!(cache.get_node_prop(1, 20), None);
    assert!(cache
      .get_traversal(1, Some(1), TraversalDirection::Out)
      .is_none());

    // Node 2 cache should remain
    assert_eq!(cache.get_node_prop(2, 10), Some(&Some(PropValue::I64(100))));
  }

  #[test]
  fn test_invalidate_edge() {
    let mut cache = make_enabled_cache();

    // Set up cache entries
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(42)));
    cache.set_traversal(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![Edge {
        src: 1,
        etype: 1,
        dst: 2,
      }],
    );
    cache.set_traversal(
      2,
      Some(1),
      TraversalDirection::In,
      vec![Edge {
        src: 1,
        etype: 1,
        dst: 2,
      }],
    );

    // Also cache for another edge
    cache.set_edge_prop(1, 2, 3, 10, Some(PropValue::I64(100)));

    // Invalidate edge (1, 1, 2)
    cache.invalidate_edge(1, 1, 2);

    // Edge (1,1,2) caches should be gone
    assert_eq!(cache.get_edge_prop(1, 1, 2, 10), None);

    // Edge (1,2,3) cache should remain
    assert_eq!(
      cache.get_edge_prop(1, 2, 3, 10),
      Some(&Some(PropValue::I64(100)))
    );
  }

  #[test]
  fn test_invalidate_key() {
    let mut cache = make_enabled_cache();

    cache.set_node_by_key("alice".to_string(), Some(1));
    cache.set_node_by_key("bob".to_string(), Some(2));

    cache.invalidate_key("alice");

    assert_eq!(cache.get_node_by_key("alice"), None);
    assert_eq!(cache.get_node_by_key("bob"), Some(&Some(2)));
  }

  #[test]
  fn test_clear() {
    let mut cache = make_enabled_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_edge_prop(1, 1, 2, 10, Some(PropValue::I64(100)));
    cache.set_traversal(1, Some(1), TraversalDirection::Out, vec![]);
    cache.set_query("q1".to_string(), 42i32);
    cache.set_node_by_key("alice".to_string(), Some(1));

    cache.clear();

    assert_eq!(cache.get_node_prop(1, 10), None);
    assert_eq!(cache.get_edge_prop(1, 1, 2, 10), None);
    assert!(cache
      .get_traversal(1, Some(1), TraversalDirection::Out)
      .is_none());
    let q: Option<&i32> = cache.get_query("q1");
    assert!(q.is_none());
    assert_eq!(cache.get_node_by_key("alice"), None);
  }

  #[test]
  fn test_clear_individual_caches() {
    let mut cache = make_enabled_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.set_traversal(1, Some(1), TraversalDirection::Out, vec![]);
    cache.set_query("q1".to_string(), 42i32);
    cache.set_node_by_key("alice".to_string(), Some(1));

    // Clear only property cache
    cache.clear_property_cache();
    assert_eq!(cache.get_node_prop(1, 10), None);
    assert!(cache
      .get_traversal(1, Some(1), TraversalDirection::Out)
      .is_some());

    // Clear only traversal cache
    cache.clear_traversal_cache();
    assert!(cache
      .get_traversal(1, Some(1), TraversalDirection::Out)
      .is_none());
    let q: Option<&i32> = cache.get_query("q1");
    assert!(q.is_some());

    // Clear only query cache
    cache.clear_query_cache();
    let q: Option<&i32> = cache.get_query("q1");
    assert!(q.is_none());
    assert_eq!(cache.get_node_by_key("alice"), Some(&Some(1)));

    // Clear only key cache
    cache.clear_key_cache();
    assert_eq!(cache.get_node_by_key("alice"), None);
  }

  #[test]
  fn test_stats() {
    let mut cache = make_enabled_cache();

    // Generate some cache activity
    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.get_node_prop(1, 10); // Hit
    cache.get_node_prop(999, 10); // Miss

    cache.set_traversal(1, Some(1), TraversalDirection::Out, vec![]);
    cache.get_traversal(1, Some(1), TraversalDirection::Out); // Hit
    cache.get_traversal(999, Some(1), TraversalDirection::Out); // Miss

    cache.set_query("q1".to_string(), 42i32);
    let _: Option<&i32> = cache.get_query("q1"); // Hit
    let _: Option<&i32> = cache.get_query("q2"); // Miss

    cache.set_node_by_key("alice".to_string(), Some(1));
    cache.get_node_by_key("alice"); // Hit
    cache.get_node_by_key("bob"); // Miss

    let stats = cache.stats();

    assert_eq!(stats.property_cache_hits, 1);
    assert_eq!(stats.property_cache_misses, 1);
    assert_eq!(stats.traversal_cache_hits, 1);
    assert_eq!(stats.traversal_cache_misses, 1);
    assert_eq!(stats.query_cache_hits, 1);
    assert_eq!(stats.query_cache_misses, 1);
    assert_eq!(stats.key_cache_hits, 1);
    assert_eq!(stats.key_cache_misses, 1);

    assert_eq!(stats.total_hits(), 4);
    assert_eq!(stats.total_misses(), 4);
    assert_eq!(stats.hit_rate(), 0.5);
  }

  #[test]
  fn test_reset_stats() {
    let mut cache = make_enabled_cache();

    cache.set_node_prop(1, 10, Some(PropValue::I64(42)));
    cache.get_node_prop(1, 10);
    cache.get_node_by_key("alice");

    assert!(cache.stats().total_hits() > 0 || cache.stats().total_misses() > 0);

    cache.reset_stats();

    assert_eq!(cache.stats().property_cache_hits, 0);
    assert_eq!(cache.stats().property_cache_misses, 0);
    assert_eq!(cache.stats().key_cache_hits, 0);
    assert_eq!(cache.stats().key_cache_misses, 0);
  }

  #[test]
  fn test_generate_query_key() {
    let cache = make_enabled_cache();

    let params = vec![("b", 2), ("a", 1)];

    let key = cache.generate_query_key(params);
    assert_eq!(key, "a:1|b:2");
  }

  #[test]
  fn test_with_key_cache_size() {
    let cache = CacheManager::with_key_cache_size(
      CacheOptions {
        enabled: true,
        ..Default::default()
      },
      500,
    );

    assert!(cache.is_enabled());
    assert_eq!(cache.stats().key_cache_max_size, 500);
  }
}
