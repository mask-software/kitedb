//! Heap data structures for pathfinding and k-nearest neighbor search
//!
//! Ported from src/util/heap.ts

use std::cmp::Ordering;
use std::collections::BinaryHeap;

// ============================================================================
// MinHeap wrapper
// ============================================================================

/// Wrapper for reverse ordering to create a min-heap from BinaryHeap
#[derive(Debug, Clone, PartialEq)]
pub struct MinHeapItem<T>(pub T);

impl<T: PartialEq> Eq for MinHeapItem<T> {}

impl<T: PartialOrd + PartialEq> Ord for MinHeapItem<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    other.0.partial_cmp(&self.0).unwrap_or(Ordering::Equal)
  }
}

impl<T: PartialOrd + PartialEq> PartialOrd for MinHeapItem<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// Min-heap for finding smallest elements
pub struct MinHeap<T> {
  heap: BinaryHeap<MinHeapItem<T>>,
}

impl<T: PartialOrd + PartialEq> MinHeap<T> {
  /// Create a new empty min-heap
  pub fn new() -> Self {
    Self {
      heap: BinaryHeap::new(),
    }
  }

  /// Create with capacity
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      heap: BinaryHeap::with_capacity(capacity),
    }
  }

  /// Push an item onto the heap
  pub fn push(&mut self, item: T) {
    self.heap.push(MinHeapItem(item));
  }

  /// Pop the minimum item
  pub fn pop(&mut self) -> Option<T> {
    self.heap.pop().map(|MinHeapItem(item)| item)
  }

  /// Peek at the minimum item
  pub fn peek(&self) -> Option<&T> {
    self.heap.peek().map(|MinHeapItem(item)| item)
  }

  /// Get the number of items
  pub fn len(&self) -> usize {
    self.heap.len()
  }

  /// Check if empty
  pub fn is_empty(&self) -> bool {
    self.heap.is_empty()
  }

  /// Clear all items
  pub fn clear(&mut self) {
    self.heap.clear();
  }
}

impl<T: PartialOrd + PartialEq> Default for MinHeap<T> {
  fn default() -> Self {
    Self::new()
  }
}

// ============================================================================
// Indexed Priority Queue (for Dijkstra/A*)
// ============================================================================

use std::collections::HashMap;
use std::hash::Hash;

/// A priority queue item with key and priority
#[derive(Debug, Clone)]
struct IndexedItem<K> {
  key: K,
  priority: f64,
}

impl<K> PartialEq for IndexedItem<K> {
  fn eq(&self, other: &Self) -> bool {
    self.priority == other.priority
  }
}

impl<K> Eq for IndexedItem<K> {}

impl<K> Ord for IndexedItem<K> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .priority
      .partial_cmp(&self.priority)
      .unwrap_or(Ordering::Equal)
  }
}

impl<K> PartialOrd for IndexedItem<K> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// Indexed min-priority queue for Dijkstra's algorithm
///
/// Supports O(log n) insert, extract_min, and decrease_priority operations.
/// Note: decrease_priority is implemented by re-inserting, which is O(log n)
/// but may leave stale entries in the heap (they are skipped during extract).
pub struct IndexedMinHeap<K: Clone + Hash + Eq> {
  heap: BinaryHeap<IndexedItem<K>>,
  priorities: HashMap<K, f64>,
}

impl<K: Clone + Hash + Eq> IndexedMinHeap<K> {
  /// Create a new indexed priority queue
  pub fn new() -> Self {
    Self {
      heap: BinaryHeap::new(),
      priorities: HashMap::new(),
    }
  }

  /// Insert a key with priority
  pub fn insert(&mut self, key: K, priority: f64) {
    self.priorities.insert(key.clone(), priority);
    self.heap.push(IndexedItem { key, priority });
  }

  /// Extract the minimum priority item
  pub fn extract_min(&mut self) -> Option<K> {
    while let Some(item) = self.heap.pop() {
      // Check if this is the current priority for this key
      // (may be stale if we did decrease_priority)
      if let Some(&current_priority) = self.priorities.get(&item.key) {
        if (item.priority - current_priority).abs() < f64::EPSILON {
          self.priorities.remove(&item.key);
          return Some(item.key);
        }
        // Stale entry, skip it
      }
      // Key was already extracted, skip
    }
    None
  }

  /// Decrease the priority of a key (or update if higher)
  pub fn decrease_priority(&mut self, key: K, new_priority: f64) {
    // Simply re-insert with new priority
    // The old entry becomes stale and will be skipped in extract_min
    self.priorities.insert(key.clone(), new_priority);
    self.heap.push(IndexedItem {
      key,
      priority: new_priority,
    });
  }

  /// Check if the queue is empty
  pub fn is_empty(&self) -> bool {
    self.priorities.is_empty()
  }

  /// Get the number of unique keys
  pub fn len(&self) -> usize {
    self.priorities.len()
  }

  /// Check if a key exists
  pub fn contains(&self, key: &K) -> bool {
    self.priorities.contains_key(key)
  }

  /// Get the priority of a key
  pub fn get_priority(&self, key: &K) -> Option<f64> {
    self.priorities.get(key).copied()
  }
}

impl<K: Clone + Hash + Eq> Default for IndexedMinHeap<K> {
  fn default() -> Self {
    Self::new()
  }
}

// ============================================================================
// MaxHeap (just use BinaryHeap directly)
// ============================================================================

/// Max-heap alias (BinaryHeap is already a max-heap)
pub type MaxHeap<T> = BinaryHeap<T>;

// ============================================================================
// Scored item for k-nearest search
// ============================================================================

/// Item with a score (distance) for heap operations
#[derive(Debug, Clone)]
pub struct ScoredItem<T> {
  pub score: f32,
  pub item: T,
}

impl<T> ScoredItem<T> {
  pub fn new(score: f32, item: T) -> Self {
    Self { score, item }
  }
}

impl<T> PartialEq for ScoredItem<T> {
  fn eq(&self, other: &Self) -> bool {
    self.score == other.score
  }
}

impl<T> Eq for ScoredItem<T> {}

impl<T> Ord for ScoredItem<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    self
      .score
      .partial_cmp(&other.score)
      .unwrap_or(Ordering::Equal)
  }
}

impl<T> PartialOrd for ScoredItem<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

/// Min-heap of scored items (for k-nearest neighbor search)
pub type MinScoredHeap<T> = MinHeap<ScoredItem<T>>;

/// Max-heap of scored items (for maintaining top-k with cutoff)
pub type MaxScoredHeap<T> = BinaryHeap<ScoredItem<T>>;

// ============================================================================
// K-nearest helper
// ============================================================================

/// Helper for finding k-nearest items with a max-heap cutoff strategy
pub struct KNearestHeap<T> {
  heap: BinaryHeap<ScoredItem<T>>,
  k: usize,
}

impl<T> KNearestHeap<T> {
  /// Create a new k-nearest heap
  pub fn new(k: usize) -> Self {
    Self {
      heap: BinaryHeap::with_capacity(k + 1),
      k,
    }
  }

  /// Push an item, potentially evicting the worst item if over capacity
  pub fn push(&mut self, score: f32, item: T) {
    if self.heap.len() < self.k {
      self.heap.push(ScoredItem::new(score, item));
    } else if let Some(worst) = self.heap.peek() {
      if score < worst.score {
        self.heap.pop();
        self.heap.push(ScoredItem::new(score, item));
      }
    }
  }

  /// Get the current worst (highest) score in the heap
  pub fn worst_score(&self) -> Option<f32> {
    self.heap.peek().map(|s| s.score)
  }

  /// Check if the heap is full
  pub fn is_full(&self) -> bool {
    self.heap.len() >= self.k
  }

  /// Get the number of items
  pub fn len(&self) -> usize {
    self.heap.len()
  }

  /// Check if empty
  pub fn is_empty(&self) -> bool {
    self.heap.is_empty()
  }

  /// Extract results sorted by score (ascending)
  pub fn into_sorted(self) -> Vec<(f32, T)> {
    let mut items: Vec<_> = self.heap.into_iter().map(|s| (s.score, s.item)).collect();
    items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    items
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_min_heap() {
    let mut heap = MinHeap::new();
    heap.push(5);
    heap.push(3);
    heap.push(7);
    heap.push(1);

    assert_eq!(heap.pop(), Some(1));
    assert_eq!(heap.pop(), Some(3));
    assert_eq!(heap.pop(), Some(5));
    assert_eq!(heap.pop(), Some(7));
    assert_eq!(heap.pop(), None);
  }

  #[test]
  fn test_k_nearest_heap() {
    let mut heap = KNearestHeap::new(3);
    heap.push(5.0, "e");
    heap.push(2.0, "b");
    heap.push(8.0, "h");
    heap.push(1.0, "a");
    heap.push(3.0, "c");

    assert_eq!(heap.len(), 3);

    let results = heap.into_sorted();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].1, "a");
    assert_eq!(results[1].1, "b");
    assert_eq!(results[2].1, "c");
  }

  #[test]
  fn test_k_nearest_heap_cutoff() {
    let mut heap = KNearestHeap::new(2);
    heap.push(10.0, "bad");
    heap.push(1.0, "good");
    heap.push(5.0, "ok");

    // 10.0 should have been evicted
    let results = heap.into_sorted();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(s, _)| *s < 10.0));
  }
}
