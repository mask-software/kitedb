//! LRU (Least Recently Used) Cache
//!
//! Generic LRU cache implementation with O(1) get/set/delete operations.
//! Uses a HashMap for O(1) lookups and a doubly-linked list for O(1) eviction.
//!
//! Ported from src/util/lru.ts

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::ptr::NonNull;

// ============================================================================
// Node structure for doubly-linked list
// ============================================================================

struct LruNode<K, V> {
  key: K,
  value: V,
  prev: Option<NonNull<LruNode<K, V>>>,
  next: Option<NonNull<LruNode<K, V>>>,
}

impl<K, V> LruNode<K, V> {
  fn new(key: K, value: V) -> Self {
    Self {
      key,
      value,
      prev: None,
      next: None,
    }
  }
}

// ============================================================================
// LRU Cache
// ============================================================================

/// LRU Cache implementation
///
/// Maintains items in order of access, automatically evicting the least
/// recently used item when capacity is exceeded.
///
/// # Type Parameters
/// - `K`: Key type (must be Hash + Eq + Clone)
/// - `V`: Value type
///
/// # Example
/// ```
/// use raydb::cache::lru::LruCache;
///
/// let mut cache = LruCache::new(3);
/// cache.set("a", 1);
/// cache.set("b", 2);
/// cache.set("c", 3);
///
/// assert_eq!(cache.get(&"a"), Some(&1));
///
/// // Adding a 4th item evicts "b" (least recently used)
/// cache.set("d", 4);
/// assert_eq!(cache.get(&"b"), None);
/// ```
pub struct LruCache<K: Hash + Eq + Clone, V> {
  max_size: usize,
  map: HashMap<K, NonNull<LruNode<K, V>>>,
  head: Option<NonNull<LruNode<K, V>>>,
  tail: Option<NonNull<LruNode<K, V>>>,
}

impl<K: Hash + Eq + Clone, V> LruCache<K, V> {
  /// Create a new LRU cache with specified maximum capacity
  ///
  /// # Panics
  /// Panics if max_size is 0
  pub fn new(max_size: usize) -> Self {
    assert!(max_size > 0, "LRU cache max_size must be greater than 0");
    Self {
      max_size,
      map: HashMap::with_capacity(max_size),
      head: None,
      tail: None,
    }
  }

  /// Create a new LRU cache with specified capacity hint
  pub fn with_capacity(max_size: usize, initial_capacity: usize) -> Self {
    assert!(max_size > 0, "LRU cache max_size must be greater than 0");
    Self {
      max_size,
      map: HashMap::with_capacity(initial_capacity.min(max_size)),
      head: None,
      tail: None,
    }
  }

  /// Get a reference to a value in the cache
  /// O(1) time complexity
  ///
  /// This marks the item as recently used.
  pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    let node_ptr = self.map.get(key).copied()?;

    // Move to front (most recently used)
    self.move_to_front(node_ptr);

    // SAFETY: node_ptr is valid because it's in our map
    unsafe { Some(&(*node_ptr.as_ptr()).value) }
  }

  /// Get a mutable reference to a value in the cache
  /// O(1) time complexity
  ///
  /// This marks the item as recently used.
  pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    let node_ptr = self.map.get(key).copied()?;

    // Move to front (most recently used)
    self.move_to_front(node_ptr);

    // SAFETY: node_ptr is valid because it's in our map
    unsafe { Some(&mut (*node_ptr.as_ptr()).value) }
  }

  /// Peek at a value without marking it as recently used
  /// O(1) time complexity
  pub fn peek<Q>(&self, key: &Q) -> Option<&V>
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    let node_ptr = self.map.get(key)?;
    // SAFETY: node_ptr is valid because it's in our map
    unsafe { Some(&(*node_ptr.as_ptr()).value) }
  }

  /// Set a value in the cache
  /// O(1) time complexity
  ///
  /// If the key already exists, updates the value and marks as recently used.
  /// If at capacity, evicts the least recently used item.
  pub fn set(&mut self, key: K, value: V) {
    if let Some(&node_ptr) = self.map.get(&key) {
      // Update existing value and move to front
      unsafe {
        (*node_ptr.as_ptr()).value = value;
      }
      self.move_to_front(node_ptr);
      return;
    }

    // Create new node
    let node = Box::new(LruNode::new(key.clone(), value));
    let node_ptr = NonNull::new(Box::into_raw(node)).unwrap();

    // Add to front
    self.push_front(node_ptr);

    // Add to map
    self.map.insert(key, node_ptr);

    // Evict if over capacity
    if self.map.len() > self.max_size {
      self.evict();
    }
  }

  /// Insert a value, returning the old value if present
  /// O(1) time complexity
  pub fn insert(&mut self, key: K, value: V) -> Option<V> {
    if let Some(&node_ptr) = self.map.get(&key) {
      // Update existing value and move to front
      let old_value = unsafe {
        let node = &mut *node_ptr.as_ptr();
        std::mem::replace(&mut node.value, value)
      };
      self.move_to_front(node_ptr);
      return Some(old_value);
    }

    // Create new node
    let node = Box::new(LruNode::new(key.clone(), value));
    let node_ptr = NonNull::new(Box::into_raw(node)).unwrap();

    // Add to front
    self.push_front(node_ptr);

    // Add to map
    self.map.insert(key, node_ptr);

    // Evict if over capacity
    if self.map.len() > self.max_size {
      self.evict();
    }

    None
  }

  /// Delete a value from the cache
  /// O(1) time complexity
  ///
  /// Returns true if the key existed and was deleted
  pub fn delete<Q>(&mut self, key: &Q) -> bool
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    if let Some(node_ptr) = self.map.remove(key) {
      self.remove_node(node_ptr);
      // SAFETY: We just removed this from the map, so we own it now
      unsafe {
        let _ = Box::from_raw(node_ptr.as_ptr());
      }
      true
    } else {
      false
    }
  }

  /// Remove a value from the cache, returning it if present
  /// O(1) time complexity
  pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    if let Some(node_ptr) = self.map.remove(key) {
      self.remove_node(node_ptr);
      // SAFETY: We just removed this from the map, so we own it now
      unsafe {
        let node = Box::from_raw(node_ptr.as_ptr());
        Some(node.value)
      }
    } else {
      None
    }
  }

  /// Check if a key exists in the cache
  /// O(1) time complexity
  ///
  /// This does NOT mark the item as recently used.
  pub fn contains_key<Q>(&self, key: &Q) -> bool
  where
    K: Borrow<Q>,
    Q: Hash + Eq + ?Sized,
  {
    self.map.contains_key(key)
  }

  /// Clear all entries from the cache
  pub fn clear(&mut self) {
    // Free all nodes
    let mut current = self.head;
    while let Some(node_ptr) = current {
      unsafe {
        current = (*node_ptr.as_ptr()).next;
        let _ = Box::from_raw(node_ptr.as_ptr());
      }
    }

    self.map.clear();
    self.head = None;
    self.tail = None;
  }

  /// Get the current number of cached items
  #[inline]
  pub fn len(&self) -> usize {
    self.map.len()
  }

  /// Check if the cache is empty
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.map.is_empty()
  }

  /// Get the maximum cache size
  #[inline]
  pub fn max_size(&self) -> usize {
    self.max_size
  }

  /// Iterate over entries in order from most to least recently used
  pub fn iter(&self) -> LruIter<'_, K, V> {
    LruIter {
      current: self.head,
      _marker: std::marker::PhantomData,
    }
  }

  // ========================================================================
  // Internal linked list operations
  // ========================================================================

  /// Push a node to the front of the list
  fn push_front(&mut self, node_ptr: NonNull<LruNode<K, V>>) {
    unsafe {
      let node = node_ptr.as_ptr();
      (*node).prev = None;
      (*node).next = self.head;

      if let Some(head) = self.head {
        (*head.as_ptr()).prev = Some(node_ptr);
      }

      self.head = Some(node_ptr);

      if self.tail.is_none() {
        self.tail = Some(node_ptr);
      }
    }
  }

  /// Move a node to the front of the list
  fn move_to_front(&mut self, node_ptr: NonNull<LruNode<K, V>>) {
    if self.head == Some(node_ptr) {
      return; // Already at front
    }

    // Remove from current position
    self.remove_node(node_ptr);

    // Add to front
    self.push_front(node_ptr);
  }

  /// Remove a node from the linked list (but don't free it)
  fn remove_node(&mut self, node_ptr: NonNull<LruNode<K, V>>) {
    unsafe {
      let node = node_ptr.as_ptr();
      let prev = (*node).prev;
      let next = (*node).next;

      if let Some(prev_ptr) = prev {
        (*prev_ptr.as_ptr()).next = next;
      } else {
        // Node is head
        self.head = next;
      }

      if let Some(next_ptr) = next {
        (*next_ptr.as_ptr()).prev = prev;
      } else {
        // Node is tail
        self.tail = prev;
      }

      (*node).prev = None;
      (*node).next = None;
    }
  }

  /// Evict the least recently used item (tail of the list)
  fn evict(&mut self) {
    if let Some(tail_ptr) = self.tail {
      unsafe {
        let key = (*tail_ptr.as_ptr()).key.clone();
        self.remove_node(tail_ptr);
        self.map.remove(&key);
        let _ = Box::from_raw(tail_ptr.as_ptr());
      }
    }
  }
}

impl<K: Hash + Eq + Clone, V> Drop for LruCache<K, V> {
  fn drop(&mut self) {
    self.clear();
  }
}

// SAFETY: LruCache can be sent between threads if K and V are Send
unsafe impl<K: Hash + Eq + Clone + Send, V: Send> Send for LruCache<K, V> {}

// ============================================================================
// Iterator
// ============================================================================

/// Iterator over LRU cache entries (most to least recently used)
pub struct LruIter<'a, K: Hash + Eq + Clone, V> {
  current: Option<NonNull<LruNode<K, V>>>,
  _marker: std::marker::PhantomData<&'a LruCache<K, V>>,
}

impl<'a, K: Hash + Eq + Clone, V> Iterator for LruIter<'a, K, V> {
  type Item = (&'a K, &'a V);

  fn next(&mut self) -> Option<Self::Item> {
    self.current.map(|node_ptr| unsafe {
      let node = node_ptr.as_ptr();
      self.current = (*node).next;
      (&(*node).key, &(*node).value)
    })
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_new_cache() {
    let cache: LruCache<String, i32> = LruCache::new(10);
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    assert_eq!(cache.max_size(), 10);
  }

  #[test]
  #[should_panic(expected = "max_size must be greater than 0")]
  fn test_zero_capacity_panics() {
    let _cache: LruCache<String, i32> = LruCache::new(0);
  }

  #[test]
  fn test_set_and_get() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    assert_eq!(cache.get(&"a"), Some(&1));
    assert_eq!(cache.get(&"b"), Some(&2));
    assert_eq!(cache.get(&"c"), Some(&3));
    assert_eq!(cache.get(&"d"), None);
    assert_eq!(cache.len(), 3);
  }

  #[test]
  fn test_update_value() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    assert_eq!(cache.get(&"a"), Some(&1));

    cache.set("a", 100);
    assert_eq!(cache.get(&"a"), Some(&100));
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_eviction() {
    let mut cache = LruCache::new(3);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    // All should be present
    assert_eq!(cache.len(), 3);
    assert!(cache.contains_key(&"a"));
    assert!(cache.contains_key(&"b"));
    assert!(cache.contains_key(&"c"));

    // Adding a 4th item should evict "a" (oldest)
    cache.set("d", 4);
    assert_eq!(cache.len(), 3);
    assert!(!cache.contains_key(&"a"));
    assert!(cache.contains_key(&"b"));
    assert!(cache.contains_key(&"c"));
    assert!(cache.contains_key(&"d"));
  }

  #[test]
  fn test_lru_order() {
    let mut cache = LruCache::new(3);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    // Access "a" to make it most recently used
    cache.get(&"a");

    // "b" is now least recently used
    cache.set("d", 4);
    assert!(!cache.contains_key(&"b"));
    assert!(cache.contains_key(&"a"));
    assert!(cache.contains_key(&"c"));
    assert!(cache.contains_key(&"d"));
  }

  #[test]
  fn test_delete() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    cache.set("b", 2);

    assert!(cache.delete(&"a"));
    assert!(!cache.delete(&"a")); // Already deleted
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.get(&"a"), None);
    assert_eq!(cache.get(&"b"), Some(&2));
  }

  #[test]
  fn test_remove() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    cache.set("b", 2);

    assert_eq!(cache.remove(&"a"), Some(1));
    assert_eq!(cache.remove(&"a"), None);
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_clear() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.get(&"a"), None);
  }

  #[test]
  fn test_peek() {
    let mut cache = LruCache::new(3);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    // Peek at "a" - should not affect LRU order
    assert_eq!(cache.peek(&"a"), Some(&1));

    // "a" should still be least recently used (not "b")
    cache.set("d", 4);
    assert!(!cache.contains_key(&"a"));
    assert!(cache.contains_key(&"b"));
  }

  #[test]
  fn test_insert_returns_old_value() {
    let mut cache = LruCache::new(10);
    assert_eq!(cache.insert("a", 1), None);
    assert_eq!(cache.insert("a", 2), Some(1));
    assert_eq!(cache.get(&"a"), Some(&2));
  }

  #[test]
  fn test_get_mut() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);

    if let Some(val) = cache.get_mut(&"a") {
      *val = 100;
    }

    assert_eq!(cache.get(&"a"), Some(&100));
  }

  #[test]
  fn test_iter() {
    let mut cache = LruCache::new(10);
    cache.set("a", 1);
    cache.set("b", 2);
    cache.set("c", 3);

    // Access "a" to make it most recently used
    cache.get(&"a");

    let items: Vec<_> = cache.iter().collect();
    // Most recently used first
    assert_eq!(items.len(), 3);
    assert_eq!(*items[0].0, "a"); // Most recently accessed
    assert_eq!(*items[1].0, "c"); // Second most recent (last added before access)
    assert_eq!(*items[2].0, "b"); // Least recently used
  }

  #[test]
  fn test_single_item() {
    let mut cache = LruCache::new(1);
    cache.set("a", 1);
    assert_eq!(cache.get(&"a"), Some(&1));

    cache.set("b", 2);
    assert_eq!(cache.get(&"a"), None);
    assert_eq!(cache.get(&"b"), Some(&2));
    assert_eq!(cache.len(), 1);
  }

  #[test]
  fn test_complex_keys() {
    let mut cache: LruCache<(u32, u32), String> = LruCache::new(10);
    cache.set((1, 2), "one-two".to_string());
    cache.set((3, 4), "three-four".to_string());

    assert_eq!(cache.get(&(1, 2)), Some(&"one-two".to_string()));
    assert_eq!(cache.get(&(3, 4)), Some(&"three-four".to_string()));
    assert_eq!(cache.get(&(5, 6)), None);
  }

  #[test]
  fn test_with_capacity() {
    let cache: LruCache<String, i32> = LruCache::with_capacity(100, 50);
    assert_eq!(cache.max_size(), 100);
    assert_eq!(cache.len(), 0);
  }
}
