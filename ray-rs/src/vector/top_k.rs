//! Bounded top-k selection by distance
//!
//! Shared by the brute-force, IVF, IVF-PQ and PQ search paths.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Keeps the `k` items with the smallest distances seen so far.
///
/// A NaN distance is never kept: it compares false against everything, so a
/// NaN at the heap root would stop every later candidate from entering.
pub(crate) struct TopK<T> {
  k: usize,
  heap: BinaryHeap<Entry<T>>,
}

impl<T> TopK<T> {
  pub(crate) fn new(k: usize) -> Self {
    Self {
      k,
      heap: BinaryHeap::with_capacity(k.min(1024)),
    }
  }

  /// Offers an item; keeps it if it is among the `k` closest so far.
  #[inline]
  pub(crate) fn push(&mut self, item: T, distance: f32) {
    if distance.is_nan() || self.k == 0 {
      return;
    }
    if self.heap.len() < self.k {
      self.heap.push(Entry { distance, item });
    } else if let Some(mut worst) = self.heap.peek_mut() {
      if distance < worst.distance {
        *worst = Entry { distance, item };
      }
    }
  }

  /// Whether an item at `distance` would be kept. Lets callers skip work
  /// (a node lookup, a filter call) for candidates that cannot enter.
  #[inline]
  pub(crate) fn admits(&self, distance: f32) -> bool {
    if distance.is_nan() || self.k == 0 {
      return false;
    }
    self.heap.len() < self.k
      || self
        .heap
        .peek()
        .is_some_and(|worst| distance < worst.distance)
  }

  pub(crate) fn len(&self) -> usize {
    self.heap.len()
  }

  /// The kept items, closest first.
  pub(crate) fn into_sorted_vec(self) -> Vec<(T, f32)> {
    self
      .heap
      .into_sorted_vec()
      .into_iter()
      .map(|entry| (entry.item, entry.distance))
      .collect()
  }
}

/// Heap entry ordered by distance alone. Distances are never NaN, so
/// `total_cmp` agrees with `<` on every stored value.
struct Entry<T> {
  distance: f32,
  item: T,
}

impl<T> PartialEq for Entry<T> {
  fn eq(&self, other: &Self) -> bool {
    self.cmp(other) == Ordering::Equal
  }
}

impl<T> Eq for Entry<T> {}

impl<T> PartialOrd for Entry<T> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<T> Ord for Entry<T> {
  fn cmp(&self, other: &Self) -> Ordering {
    self.distance.total_cmp(&other.distance)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn keeps_the_k_smallest_in_order() {
    let mut top = TopK::new(3);
    for (item, distance) in [(1, 0.5), (2, 0.3), (3, 0.8), (4, 0.1), (5, 0.4)] {
      top.push(item, distance);
    }
    assert_eq!(top.into_sorted_vec(), vec![(4, 0.1), (2, 0.3), (5, 0.4)]);
  }

  #[test]
  fn nan_never_enters_or_blocks_later_items() {
    let mut top = TopK::new(2);
    top.push(0, f32::NAN);
    assert!(!top.admits(f32::NAN));
    for (item, distance) in [(1, 3.0), (2, 1.0), (3, 2.0)] {
      top.push(item, distance);
    }
    top.push(4, f32::NAN);
    assert_eq!(top.into_sorted_vec(), vec![(2, 1.0), (3, 2.0)]);
  }

  #[test]
  fn infinities_rank_at_the_ends() {
    let mut top = TopK::new(3);
    for (item, distance) in [(1, f32::INFINITY), (2, 0.0), (3, f32::NEG_INFINITY)] {
      top.push(item, distance);
    }
    assert_eq!(
      top.into_sorted_vec(),
      vec![(3, f32::NEG_INFINITY), (2, 0.0), (1, f32::INFINITY)]
    );
  }

  #[test]
  fn zero_k_keeps_nothing() {
    let mut top = TopK::new(0);
    assert!(!top.admits(1.0));
    top.push(1, 1.0);
    assert_eq!(top.len(), 0);
    assert!(top.into_sorted_vec().is_empty());
  }
}
