//! Query Cache
//!
//! Caches complex query results with optional TTL (time-to-live).
//! Uses content-addressed keys based on query parameters.
//!
//! Ported from src/cache/query-cache.ts

use super::lru::LruCache;
use crate::types::QueryCacheConfig;
use std::any::Any;
use std::time::{Duration, Instant};

// ============================================================================
// Query Cache Entry
// ============================================================================

/// Cached query result with timestamp for TTL support
struct CachedQueryResult {
  /// The cached value (type-erased)
  value: Box<dyn Any + Send>,
  /// When the entry was cached (for TTL)
  timestamp: Instant,
}

// ============================================================================
// Query Cache Statistics
// ============================================================================

/// Query cache statistics
#[derive(Debug, Clone, Default)]
pub struct QueryCacheStats {
  pub hits: u64,
  pub misses: u64,
  pub expirations: u64,
  pub cache_size: usize,
  pub max_cache_size: usize,
}

impl QueryCacheStats {
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
// Query Cache
// ============================================================================

/// Query cache for complex query results
///
/// Supports optional TTL (time-to-live) for automatic expiration.
/// Uses type-erased storage to support arbitrary result types.
///
/// # Example
/// ```
/// use raydb::cache::query::QueryCache;
/// use raydb::types::QueryCacheConfig;
///
/// let config = QueryCacheConfig {
///     max_entries: 100,
///     ttl_ms: Some(60000), // 1 minute TTL
/// };
/// let mut cache = QueryCache::new(config);
///
/// // Cache a query result
/// cache.set("query1".to_string(), vec![1u64, 2, 3]);
///
/// // Retrieve the result
/// let result: Option<&Vec<u64>> = cache.get("query1");
/// assert!(result.is_some());
/// assert_eq!(result.unwrap(), &vec![1u64, 2, 3]);
/// ```
pub struct QueryCache {
  /// The LRU cache for query results
  cache: LruCache<String, CachedQueryResult>,

  /// Optional TTL in duration
  ttl: Option<Duration>,

  /// Cache statistics
  hits: u64,
  misses: u64,
  expirations: u64,
}

impl QueryCache {
  /// Create a new query cache with the given configuration
  pub fn new(config: QueryCacheConfig) -> Self {
    Self {
      cache: LruCache::new(config.max_entries),
      ttl: config.ttl_ms.map(Duration::from_millis),
      hits: 0,
      misses: 0,
      expirations: 0,
    }
  }

  /// Create a new query cache with no TTL
  pub fn new_without_ttl(max_entries: usize) -> Self {
    Self {
      cache: LruCache::new(max_entries),
      ttl: None,
      hits: 0,
      misses: 0,
      expirations: 0,
    }
  }

  /// Create a new query cache with TTL
  pub fn new_with_ttl(max_entries: usize, ttl: Duration) -> Self {
    Self {
      cache: LruCache::new(max_entries),
      ttl: Some(ttl),
      hits: 0,
      misses: 0,
      expirations: 0,
    }
  }

  /// Get a cached query result
  ///
  /// Returns None if:
  /// - Key is not in cache
  /// - Entry has expired (if TTL is configured)
  /// - Type mismatch
  pub fn get<T: 'static>(&mut self, key: &str) -> Option<&T> {
    // Use peek first to check if entry exists without affecting LRU
    let has_entry = self.cache.peek(key).is_some();

    if !has_entry {
      self.misses += 1;
      return None;
    }

    // Check TTL if configured
    if let Some(ttl) = self.ttl {
      let entry = self.cache.peek(key)?;
      let age = entry.timestamp.elapsed();

      if age > ttl {
        // Expired, remove from cache
        self.cache.delete(key);
        self.misses += 1;
        self.expirations += 1;
        return None;
      }
    }

    // Get the entry (this updates LRU)
    let entry = self.cache.get(key)?;

    // Try to downcast to expected type
    match entry.value.downcast_ref::<T>() {
      Some(value) => {
        self.hits += 1;
        Some(value)
      }
      None => {
        // Type mismatch - treat as miss
        self.misses += 1;
        None
      }
    }
  }

  /// Peek at a cached query result without affecting LRU or TTL
  pub fn peek<T: 'static>(&self, key: &str) -> Option<&T> {
    let entry = self.cache.peek(key)?;

    // Check TTL if configured (but don't remove)
    if let Some(ttl) = self.ttl {
      if entry.timestamp.elapsed() > ttl {
        return None;
      }
    }

    entry.value.downcast_ref::<T>()
  }

  /// Set a query result in cache
  pub fn set<T: 'static + Send>(&mut self, key: String, value: T) {
    let entry = CachedQueryResult {
      value: Box::new(value),
      timestamp: Instant::now(),
    };
    self.cache.set(key, entry);
  }

  /// Delete a cached query result
  pub fn delete(&mut self, key: &str) -> bool {
    self.cache.delete(key)
  }

  /// Check if a key exists in cache (without checking TTL)
  pub fn contains_key(&self, key: &str) -> bool {
    self.cache.contains_key(key)
  }

  /// Check if a key exists and is not expired
  pub fn contains_key_valid(&self, key: &str) -> bool {
    let Some(entry) = self.cache.peek(key) else {
      return false;
    };

    if let Some(ttl) = self.ttl {
      if entry.timestamp.elapsed() > ttl {
        return false;
      }
    }

    true
  }

  /// Clear all cached queries
  pub fn clear(&mut self) {
    self.cache.clear();
    self.hits = 0;
    self.misses = 0;
    self.expirations = 0;
  }

  /// Get cache statistics
  pub fn stats(&self) -> QueryCacheStats {
    QueryCacheStats {
      hits: self.hits,
      misses: self.misses,
      expirations: self.expirations,
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

  /// Get the configured TTL
  pub fn ttl(&self) -> Option<Duration> {
    self.ttl
  }

  /// Set a new TTL (affects new entries and future gets)
  pub fn set_ttl(&mut self, ttl: Option<Duration>) {
    self.ttl = ttl;
  }

  /// Reset statistics counters
  pub fn reset_stats(&mut self) {
    self.hits = 0;
    self.misses = 0;
    self.expirations = 0;
  }

  /// Prune expired entries (useful for periodic cleanup)
  pub fn prune_expired(&mut self) -> usize {
    let Some(ttl) = self.ttl else {
      return 0;
    };

    // Collect expired keys
    let expired_keys: Vec<String> = self
      .cache
      .iter()
      .filter(|(_, entry)| entry.timestamp.elapsed() > ttl)
      .map(|(k, _)| k.clone())
      .collect();

    let count = expired_keys.len();

    // Remove expired entries
    for key in expired_keys {
      self.cache.delete(&key);
      self.expirations += 1;
    }

    count
  }
}

// ============================================================================
// Key Generation Utilities
// ============================================================================

/// Generate a cache key from a string
pub fn generate_key_string(s: &str) -> String {
  s.to_string()
}

/// Generate a cache key from query parameters
///
/// Sorts keys for consistent hashing and serializes values.
pub fn generate_key_params<K, V>(params: impl IntoIterator<Item = (K, V)>) -> String
where
  K: AsRef<str> + Ord,
  V: std::fmt::Debug,
{
  let mut items: Vec<_> = params.into_iter().collect();
  items.sort_by(|a, b| a.0.as_ref().cmp(b.0.as_ref()));

  items
    .into_iter()
    .map(|(k, v)| format!("{}:{:?}", k.as_ref(), v))
    .collect::<Vec<_>>()
    .join("|")
}

/// Generate a cache key by hashing with xxHash64
pub fn generate_key_hash(data: &[u8]) -> String {
  use xxhash_rust::xxh64::xxh64;
  xxh64(data, 0).to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use std::thread;
  use std::time::Duration;

  fn make_cache() -> QueryCache {
    QueryCache::new(QueryCacheConfig {
      max_entries: 100,
      ttl_ms: None,
    })
  }

  fn make_cache_with_ttl(ttl_ms: u64) -> QueryCache {
    QueryCache::new(QueryCacheConfig {
      max_entries: 100,
      ttl_ms: Some(ttl_ms),
    })
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
    let result: Option<&i32> = cache.get("nonexistent");
    assert!(result.is_none());
    assert_eq!(cache.stats().misses, 1);
  }

  #[test]
  fn test_cache_hit() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), vec![1i32, 2, 3]);

    let result: Option<&Vec<i32>> = cache.get("query1");
    assert!(result.is_some());
    assert_eq!(result.unwrap(), &vec![1, 2, 3]);
    assert_eq!(cache.stats().hits, 1);
  }

  #[test]
  fn test_type_mismatch() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), 42i32);

    // Try to get as wrong type
    let result: Option<&String> = cache.get("query1");
    assert!(result.is_none());
    assert_eq!(cache.stats().misses, 1);

    // Get as correct type should work
    let result: Option<&i32> = cache.get("query1");
    assert_eq!(result, Some(&42));
    assert_eq!(cache.stats().hits, 1);
  }

  #[test]
  fn test_update_value() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), 42i32);
    cache.set("query1".to_string(), 100i32);

    let result: Option<&i32> = cache.get("query1");
    assert_eq!(result, Some(&100));
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_delete() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), 42i32);

    assert!(cache.delete("query1"));
    assert!(!cache.delete("query1")); // Already deleted

    let result: Option<&i32> = cache.get("query1");
    assert!(result.is_none());
  }

  #[test]
  fn test_contains_key() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), 42i32);

    assert!(cache.contains_key("query1"));
    assert!(!cache.contains_key("query2"));
  }

  #[test]
  fn test_clear() {
    let mut cache = make_cache();
    cache.set("query1".to_string(), 42i32);
    cache.set("query2".to_string(), 100i32);

    // Generate stats
    let _: Option<&i32> = cache.get("query1");
    let _: Option<&i32> = cache.get("nonexistent");

    cache.clear();

    assert!(cache.is_empty());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_ttl_expiration() {
    let mut cache = make_cache_with_ttl(50); // 50ms TTL

    cache.set("query1".to_string(), 42i32);

    // Should be accessible immediately
    let result: Option<&i32> = cache.get("query1");
    assert_eq!(result, Some(&42));
    assert_eq!(cache.stats().hits, 1);

    // Wait for TTL to expire
    thread::sleep(Duration::from_millis(60));

    // Should be expired now
    let result: Option<&i32> = cache.get("query1");
    assert!(result.is_none());
    assert_eq!(cache.stats().misses, 1);
    assert_eq!(cache.stats().expirations, 1);
  }

  #[test]
  fn test_contains_key_valid_with_ttl() {
    let mut cache = make_cache_with_ttl(50);

    cache.set("query1".to_string(), 42i32);

    assert!(cache.contains_key_valid("query1"));

    thread::sleep(Duration::from_millis(60));

    // Key still exists but is expired
    assert!(cache.contains_key("query1"));
    assert!(!cache.contains_key_valid("query1"));
  }

  #[test]
  fn test_peek_does_not_affect_stats() {
    let cache = make_cache();

    // Peek should not affect stats
    let result: Option<&i32> = cache.peek("nonexistent");
    assert!(result.is_none());
    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);
  }

  #[test]
  fn test_prune_expired() {
    let mut cache = make_cache_with_ttl(50);

    cache.set("query1".to_string(), 1i32);
    cache.set("query2".to_string(), 2i32);

    // Wait for TTL to expire
    thread::sleep(Duration::from_millis(60));

    // Add a fresh entry
    cache.set("query3".to_string(), 3i32);

    // Prune expired
    let pruned = cache.prune_expired();
    assert_eq!(pruned, 2);

    // Only query3 should remain
    assert_eq!(cache.len(), 1);
    let result: Option<&i32> = cache.get("query3");
    assert_eq!(result, Some(&3));
  }

  #[test]
  fn test_set_ttl() {
    let mut cache = make_cache();
    assert!(cache.ttl().is_none());

    cache.set_ttl(Some(Duration::from_millis(100)));
    assert_eq!(cache.ttl(), Some(Duration::from_millis(100)));

    cache.set_ttl(None);
    assert!(cache.ttl().is_none());
  }

  #[test]
  fn test_various_types() {
    let mut cache = make_cache();

    cache.set("int".to_string(), 42i64);
    cache.set("float".to_string(), 3.14f64);
    cache.set("string".to_string(), "hello".to_string());
    cache.set("vec".to_string(), vec![1u32, 2, 3]);
    cache.set("tuple".to_string(), (1i32, "a".to_string()));

    assert_eq!(cache.get::<i64>("int"), Some(&42));
    assert_eq!(cache.get::<f64>("float"), Some(&3.14));
    assert_eq!(cache.get::<String>("string"), Some(&"hello".to_string()));
    assert_eq!(cache.get::<Vec<u32>>("vec"), Some(&vec![1, 2, 3]));
    assert_eq!(
      cache.get::<(i32, String)>("tuple"),
      Some(&(1, "a".to_string()))
    );
  }

  #[test]
  fn test_stats() {
    let mut cache = make_cache();

    cache.set("query1".to_string(), 42i32);

    // 2 hits
    let _: Option<&i32> = cache.get("query1");
    let _: Option<&i32> = cache.get("query1");

    // 2 misses
    let _: Option<&i32> = cache.get("missing");
    let _: Option<&String> = cache.get("query1"); // Type mismatch

    let stats = cache.stats();
    assert_eq!(stats.hits, 2);
    assert_eq!(stats.misses, 2);
    assert_eq!(stats.hit_rate(), 0.5);
  }

  #[test]
  fn test_reset_stats() {
    let mut cache = make_cache();

    cache.set("query1".to_string(), 42i32);
    let _: Option<&i32> = cache.get("query1");
    let _: Option<&i32> = cache.get("missing");

    cache.reset_stats();

    assert_eq!(cache.stats().hits, 0);
    assert_eq!(cache.stats().misses, 0);

    // Cache should still have data
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_generate_key_string() {
    assert_eq!(generate_key_string("hello"), "hello");
  }

  #[test]
  fn test_generate_key_params() {
    let params = vec![("b", 2), ("a", 1), ("c", 3)];

    // Should be sorted by key
    let key = generate_key_params(params);
    assert_eq!(key, "a:1|b:2|c:3");
  }

  #[test]
  fn test_new_without_ttl() {
    let cache = QueryCache::new_without_ttl(50);
    assert!(cache.ttl().is_none());
    assert_eq!(cache.stats().max_cache_size, 50);
  }

  #[test]
  fn test_new_with_ttl() {
    let cache = QueryCache::new_with_ttl(50, Duration::from_secs(60));
    assert_eq!(cache.ttl(), Some(Duration::from_secs(60)));
    assert_eq!(cache.stats().max_cache_size, 50);
  }
}
