//! Traversal Cache
//!
//! Caches neighbor iteration results to avoid repeated graph traversals.
//! Uses targeted invalidation via reverse index for O(1) node/edge invalidation.
//!
//! Ported from src/cache/traversal-cache.ts

use super::lru::LruCache;
use crate::types::{ETypeId, Edge, NodeId, TraversalCacheConfig};
use std::collections::{HashMap, HashSet};

// ============================================================================
// Cache Key Types
// ============================================================================

/// Traversal direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraversalDirection {
  Out,
  In,
}

/// Traversal cache key - packed as u64 for fast comparison and hashing
/// Pack: nodeId (53 bits) | etype (10 bits) | direction (1 bit)
/// With etype=0x3FF meaning "all edge types"
type TraversalKey = u64;

/// Sentinel value for "all edge types"
const ALL_ETYPES: u64 = 0x3FF; // 1023

// ============================================================================
// Cached Neighbors Result
// ============================================================================

/// Cached neighbors result
#[derive(Debug, Clone)]
pub struct CachedNeighbors {
  /// The cached neighbor edges
  pub neighbors: Vec<Edge>,
  /// True if the neighbors were truncated due to max_neighbors_per_entry
  pub truncated: bool,
}

// ============================================================================
// Traversal Cache Statistics
// ============================================================================

/// Traversal cache statistics
#[derive(Debug, Clone, Default)]
pub struct TraversalCacheStats {
  pub hits: u64,
  pub misses: u64,
  pub cache_size: usize,
  pub max_cache_size: usize,
}

impl TraversalCacheStats {
  /// Calculate hit rate
  pub fn hit_rate(&self) -> f64 {
    let total = self.hits + self.misses;
    if total > 0 {
      self.hits as f64 / total as f64
    } else {
      0.0
    }
  }
}

// ============================================================================
// Traversal Cache
// ============================================================================

/// Traversal cache for neighbor lookups
///
/// Uses targeted invalidation via reverse index mapping:
/// - `node_key_index`: Maps NodeId -> Set<TraversalKey> for O(1) node invalidation
///
/// When a node changes, we invalidate:
/// - All outgoing traversals from that node
/// - All incoming traversals to that node
/// - All traversals that include this node in their results (as a destination)
///
/// When an edge changes (src -> dst), we invalidate:
/// - Outgoing traversals from src (affected by edge addition/removal)
/// - Incoming traversals to dst (affected by edge addition/removal)
///
/// # Example
/// ```
/// use raydb::cache::traversal::{TraversalCache, TraversalDirection};
/// use raydb::types::{TraversalCacheConfig, Edge};
///
/// let config = TraversalCacheConfig {
///     max_entries: 100,
///     max_neighbors_per_entry: 50,
/// };
/// let mut cache = TraversalCache::new(config);
///
/// // Cache neighbors for a traversal
/// let neighbors = vec![
///     Edge { src: 1, etype: 1, dst: 2 },
///     Edge { src: 1, etype: 1, dst: 3 },
/// ];
/// cache.set(1, Some(1), TraversalDirection::Out, neighbors.clone());
///
/// // Retrieve from cache
/// let result = cache.get(1, Some(1), TraversalDirection::Out);
/// assert!(result.is_some());
/// ```
pub struct TraversalCache {
  /// The LRU cache for traversal results
  cache: LruCache<TraversalKey, CachedNeighbors>,

  /// Maximum neighbors to store per entry
  max_neighbors_per_entry: usize,

  /// Reverse index: NodeId -> Set of traversal keys that reference this node
  /// (as source OR destination)
  node_key_index: HashMap<NodeId, HashSet<TraversalKey>>,

  /// Cache statistics
  hits: u64,
  misses: u64,
}

impl TraversalCache {
  /// Create a new traversal cache with the given configuration
  pub fn new(config: TraversalCacheConfig) -> Self {
    Self {
      cache: LruCache::new(config.max_entries),
      max_neighbors_per_entry: config.max_neighbors_per_entry,
      node_key_index: HashMap::new(),
      hits: 0,
      misses: 0,
    }
  }

  /// Get cached neighbors for a node
  ///
  /// # Arguments
  /// - `node_id` - Source node ID
  /// - `etype` - Edge type ID, or None for all types
  /// - `direction` - Traversal direction (Out or In)
  ///
  /// # Returns
  /// Cached neighbors or None if not cached
  pub fn get(
    &mut self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
  ) -> Option<&CachedNeighbors> {
    let key = Self::make_key(node_id, etype, direction);
    let result = self.cache.get(&key);

    if result.is_some() {
      self.hits += 1;
    } else {
      self.misses += 1;
    }

    result
  }

  /// Peek at cached neighbors without affecting LRU order
  pub fn peek(
    &self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
  ) -> Option<&CachedNeighbors> {
    let key = Self::make_key(node_id, etype, direction);
    self.cache.peek(&key)
  }

  /// Set cached neighbors for a node
  ///
  /// # Arguments
  /// - `node_id` - Source node ID
  /// - `etype` - Edge type ID, or None for all types
  /// - `direction` - Traversal direction (Out or In)
  /// - `neighbors` - Array of neighbor edges
  pub fn set(
    &mut self,
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
    neighbors: Vec<Edge>,
  ) {
    let key = Self::make_key(node_id, etype, direction);

    // Truncate if exceeds max neighbors per entry
    let (cached_neighbors, truncated) = if neighbors.len() > self.max_neighbors_per_entry {
      (
        neighbors
          .into_iter()
          .take(self.max_neighbors_per_entry)
          .collect(),
        true,
      )
    } else {
      (neighbors, false)
    };

    // Track source node for invalidation
    self.add_to_node_index(node_id, key);

    // Track destination nodes for invalidation
    // This ensures that when a destination node changes, this cache entry is invalidated
    for edge in &cached_neighbors {
      let dest_id = match direction {
        TraversalDirection::Out => edge.dst,
        TraversalDirection::In => edge.src,
      };
      self.add_to_node_index(dest_id, key);
    }

    self.cache.set(
      key,
      CachedNeighbors {
        neighbors: cached_neighbors,
        truncated,
      },
    );
  }

  /// Invalidate all cached traversals for a node
  ///
  /// Complexity: O(k) where k = number of traversals referencing this node
  pub fn invalidate_node(&mut self, node_id: NodeId) {
    if let Some(keys) = self.node_key_index.remove(&node_id) {
      for key in keys {
        self.cache.delete(&key);
      }
    }
  }

  /// Invalidate traversals involving a specific edge
  ///
  /// When an edge (src, etype, dst) is added/removed:
  /// - Outgoing traversals from src are affected
  /// - Incoming traversals to dst are affected
  pub fn invalidate_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
    // Invalidate outgoing traversals from src
    self.invalidate_node_traversals(src, TraversalDirection::Out, etype);

    // Invalidate incoming traversals to dst
    self.invalidate_node_traversals(dst, TraversalDirection::In, etype);
  }

  /// Clear all cached traversals
  pub fn clear(&mut self) {
    self.cache.clear();
    self.node_key_index.clear();
    self.hits = 0;
    self.misses = 0;
  }

  /// Get cache statistics
  pub fn stats(&self) -> TraversalCacheStats {
    TraversalCacheStats {
      hits: self.hits,
      misses: self.misses,
      cache_size: self.cache.len(),
      max_cache_size: self.cache.max_size(),
    }
  }

  /// Get current cache size
  pub fn len(&self) -> usize {
    self.cache.len()
  }

  /// Check if cache is empty
  pub fn is_empty(&self) -> bool {
    self.cache.is_empty()
  }

  /// Reset statistics counters
  pub fn reset_stats(&mut self) {
    self.hits = 0;
    self.misses = 0;
  }

  // ========================================================================
  // Internal Methods
  // ========================================================================

  /// Generate cache key for traversal using u64 packing for faster comparison
  ///
  /// Pack: nodeId (53 bits) | etype (10 bits) | direction (1 bit)
  /// With etype=0x3FF meaning "all types"
  fn make_key(
    node_id: NodeId,
    etype: Option<ETypeId>,
    direction: TraversalDirection,
  ) -> TraversalKey {
    let etype_val = etype.map(|e| e as u64).unwrap_or(ALL_ETYPES);
    let dir_val = match direction {
      TraversalDirection::Out => 0u64,
      TraversalDirection::In => 1u64,
    };

    // nodeId << 11 | etype << 1 | direction
    (node_id << 11) | (etype_val << 1) | dir_val
  }

  /// Add a key to the node index
  fn add_to_node_index(&mut self, node_id: NodeId, key: TraversalKey) {
    self.node_key_index.entry(node_id).or_default().insert(key);
  }

  /// Invalidate specific traversals for a node
  fn invalidate_node_traversals(
    &mut self,
    node_id: NodeId,
    direction: TraversalDirection,
    etype: ETypeId,
  ) {
    let Some(keys) = self.node_key_index.get_mut(&node_id) else {
      return;
    };

    // Find keys that match this direction and etype (or "all")
    let specific_key = Self::make_key(node_id, Some(etype), direction);
    let all_key = Self::make_key(node_id, None, direction);

    let mut keys_to_delete = Vec::new();

    if keys.contains(&specific_key) {
      keys_to_delete.push(specific_key);
    }
    if keys.contains(&all_key) {
      keys_to_delete.push(all_key);
    }

    // Delete matched keys
    for key in keys_to_delete {
      self.cache.delete(&key);
      keys.remove(&key);
    }

    // Clean up empty index entries
    if keys.is_empty() {
      self.node_key_index.remove(&node_id);
    }
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  fn make_cache() -> TraversalCache {
    TraversalCache::new(TraversalCacheConfig {
      max_entries: 100,
      max_neighbors_per_entry: 10,
    })
  }

  fn make_edge(src: NodeId, etype: ETypeId, dst: NodeId) -> Edge {
    Edge { src, etype, dst }
  }

  #[test]
  fn test_new_cache() {
    let cache = make_cache();
    assert!(cache.is_empty());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_cache_miss() {
    let mut cache = make_cache();
    assert!(cache.get(1, Some(1), TraversalDirection::Out).is_none());
    assert_eq!(cache.stats().misses, 1);
  }

  #[test]
  fn test_cache_hit() {
    let mut cache = make_cache();

    let neighbors = vec![make_edge(1, 1, 2), make_edge(1, 1, 3)];
    cache.set(1, Some(1), TraversalDirection::Out, neighbors.clone());

    let result = cache.get(1, Some(1), TraversalDirection::Out);
    assert!(result.is_some());
    let cached = result.unwrap();
    assert_eq!(cached.neighbors.len(), 2);
    assert!(!cached.truncated);
    assert_eq!(cache.stats().hits, 1);
  }

  #[test]
  fn test_cache_all_etypes() {
    let mut cache = make_cache();

    let neighbors = vec![make_edge(1, 1, 2), make_edge(1, 2, 3)];
    cache.set(1, None, TraversalDirection::Out, neighbors);

    // Should hit with None etype
    assert!(cache.get(1, None, TraversalDirection::Out).is_some());

    // Should miss with specific etype
    assert!(cache.get(1, Some(1), TraversalDirection::Out).is_none());
  }

  #[test]
  fn test_different_directions() {
    let mut cache = make_cache();

    let out_neighbors = vec![make_edge(1, 1, 2)];
    let in_neighbors = vec![make_edge(3, 1, 1)];

    cache.set(1, Some(1), TraversalDirection::Out, out_neighbors);
    cache.set(1, Some(1), TraversalDirection::In, in_neighbors);

    // Check out result
    let out_result = cache.get(1, Some(1), TraversalDirection::Out);
    assert!(out_result.is_some());
    assert_eq!(out_result.unwrap().neighbors[0].dst, 2);

    // Check in result (separate borrow)
    let in_result = cache.get(1, Some(1), TraversalDirection::In);
    assert!(in_result.is_some());
    assert_eq!(in_result.unwrap().neighbors[0].src, 3);
  }

  #[test]
  fn test_truncation() {
    let mut cache = TraversalCache::new(TraversalCacheConfig {
      max_entries: 100,
      max_neighbors_per_entry: 3,
    });

    let neighbors = vec![
      make_edge(1, 1, 2),
      make_edge(1, 1, 3),
      make_edge(1, 1, 4),
      make_edge(1, 1, 5),
      make_edge(1, 1, 6),
    ];
    cache.set(1, Some(1), TraversalDirection::Out, neighbors);

    let result = cache.get(1, Some(1), TraversalDirection::Out).unwrap();
    assert_eq!(result.neighbors.len(), 3);
    assert!(result.truncated);
  }

  #[test]
  fn test_invalidate_node() {
    let mut cache = make_cache();

    // Cache traversals for multiple nodes
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );
    cache.set(
      2,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(2, 1, 3)],
    );

    assert_eq!(cache.len(), 2);

    // Invalidate node 1
    cache.invalidate_node(1);

    // Node 1 traversals should be gone
    assert!(cache.get(1, Some(1), TraversalDirection::Out).is_none());

    // Node 2 traversals should remain (note: hit counter increments)
    let stats_before = cache.stats().hits;
    assert!(cache.get(2, Some(1), TraversalDirection::Out).is_some());
    assert_eq!(cache.stats().hits, stats_before + 1);
  }

  #[test]
  fn test_invalidate_node_as_destination() {
    let mut cache = make_cache();

    // Cache traversal from node 1 that includes node 2 as destination
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2), make_edge(1, 1, 3)],
    );

    // Invalidate node 2 (which is a destination)
    cache.invalidate_node(2);

    // The entire traversal should be invalidated because it included node 2
    assert!(cache.get(1, Some(1), TraversalDirection::Out).is_none());
  }

  #[test]
  fn test_invalidate_edge() {
    let mut cache = make_cache();

    // Cache outgoing from node 1
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );

    // Cache incoming to node 2
    cache.set(2, Some(1), TraversalDirection::In, vec![make_edge(1, 1, 2)]);

    // Cache unrelated traversal
    cache.set(
      3,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(3, 1, 4)],
    );

    // Invalidate edge (1, 1, 2)
    cache.invalidate_edge(1, 1, 2);

    // Outgoing from 1 and incoming to 2 should be invalidated
    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_none());
    assert!(cache.peek(2, Some(1), TraversalDirection::In).is_none());

    // Unrelated traversal should remain
    assert!(cache.peek(3, Some(1), TraversalDirection::Out).is_some());
  }

  #[test]
  fn test_invalidate_edge_all_etypes() {
    let mut cache = make_cache();

    // Cache with "all etypes"
    cache.set(
      1,
      None,
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2), make_edge(1, 2, 3)],
    );

    // Also cache specific etype
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );

    // Invalidate edge with etype=1
    cache.invalidate_edge(1, 1, 2);

    // Both should be invalidated (specific and "all")
    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_none());
    assert!(cache.peek(1, None, TraversalDirection::Out).is_none());
  }

  #[test]
  fn test_clear() {
    let mut cache = make_cache();

    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );
    cache.get(1, Some(1), TraversalDirection::Out);
    cache.get(999, Some(1), TraversalDirection::Out);

    cache.clear();

    assert!(cache.is_empty());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_peek_does_not_update_lru() {
    let mut cache = TraversalCache::new(TraversalCacheConfig {
      max_entries: 2,
      max_neighbors_per_entry: 10,
    });

    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 10)],
    );
    cache.set(
      2,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(2, 1, 20)],
    );

    // Peek at node 1 (should NOT make it most recently used)
    cache.peek(1, Some(1), TraversalDirection::Out);

    // Add node 3 - should evict node 1 (oldest)
    cache.set(
      3,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(3, 1, 30)],
    );

    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_none());
    assert!(cache.peek(2, Some(1), TraversalDirection::Out).is_some());
    assert!(cache.peek(3, Some(1), TraversalDirection::Out).is_some());
  }

  #[test]
  fn test_get_updates_lru() {
    let mut cache = TraversalCache::new(TraversalCacheConfig {
      max_entries: 2,
      max_neighbors_per_entry: 10,
    });

    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 10)],
    );
    cache.set(
      2,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(2, 1, 20)],
    );

    // Get node 1 (SHOULD make it most recently used)
    cache.get(1, Some(1), TraversalDirection::Out);

    // Add node 3 - should evict node 2 (oldest after node 1 was accessed)
    cache.set(
      3,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(3, 1, 30)],
    );

    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_some());
    assert!(cache.peek(2, Some(1), TraversalDirection::Out).is_none());
    assert!(cache.peek(3, Some(1), TraversalDirection::Out).is_some());
  }

  #[test]
  fn test_stats() {
    let mut cache = make_cache();

    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );

    // 2 hits
    cache.get(1, Some(1), TraversalDirection::Out);
    cache.get(1, Some(1), TraversalDirection::Out);

    // 2 misses
    cache.get(999, Some(1), TraversalDirection::Out);
    cache.get(1, Some(2), TraversalDirection::Out);

    let stats = cache.stats();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.hit_rate(), 0.5);
  }

  #[test]
  fn test_reset_stats() {
    let mut cache = make_cache();

    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );
    cache.get(1, Some(1), TraversalDirection::Out);
    cache.get(999, Some(1), TraversalDirection::Out);

    cache.reset_stats();

    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);

    // Cache should still have data
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_key_uniqueness() {
    let mut cache = make_cache();

    // Different node IDs
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 100)],
    );
    cache.set(
      2,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(2, 1, 200)],
    );

    // Different etypes
    cache.set(
      1,
      Some(2),
      TraversalDirection::Out,
      vec![make_edge(1, 2, 300)],
    );

    // Different directions
    cache.set(
      1,
      Some(1),
      TraversalDirection::In,
      vec![make_edge(100, 1, 1)],
    );

    assert_eq!(cache.len(), 4);

    // Each should be retrievable independently
    assert_eq!(
      cache
        .peek(1, Some(1), TraversalDirection::Out)
        .unwrap()
        .neighbors[0]
        .dst,
      100
    );
    assert_eq!(
      cache
        .peek(2, Some(1), TraversalDirection::Out)
        .unwrap()
        .neighbors[0]
        .dst,
      200
    );
    assert_eq!(
      cache
        .peek(1, Some(2), TraversalDirection::Out)
        .unwrap()
        .neighbors[0]
        .dst,
      300
    );
    assert_eq!(
      cache
        .peek(1, Some(1), TraversalDirection::In)
        .unwrap()
        .neighbors[0]
        .src,
      100
    );
  }

  #[test]
  fn test_empty_neighbors() {
    let mut cache = make_cache();

    // Cache empty result (node has no neighbors)
    cache.set(1, Some(1), TraversalDirection::Out, vec![]);

    let result = cache.get(1, Some(1), TraversalDirection::Out);
    assert!(result.is_some());
    assert!(result.unwrap().neighbors.is_empty());
    assert!(!result.unwrap().truncated);
  }

  #[test]
  fn test_invalidate_nonexistent_node() {
    let mut cache = make_cache();
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );

    // Should not panic or affect other entries
    cache.invalidate_node(999);

    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_some());
  }

  #[test]
  fn test_invalidate_nonexistent_edge() {
    let mut cache = make_cache();
    cache.set(
      1,
      Some(1),
      TraversalDirection::Out,
      vec![make_edge(1, 1, 2)],
    );

    // Should not panic or affect other entries
    cache.invalidate_edge(999, 1, 888);

    assert!(cache.peek(1, Some(1), TraversalDirection::Out).is_some());
  }
}
