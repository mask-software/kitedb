//! Which MVCC transactions a database has open, kept without a lock.
//!
//! Every transaction registers its snapshot here when it begins and leaves
//! when it ends ([`OpenTransactions::register`], [`OpenTransactions::unregister`]),
//! so a begin, and the end of a read-only or rolled-back transaction, take
//! no lock: they used to take the transaction manager's, which a commit
//! group holds while it checks and commits its members, so with several
//! writers begins waited for commit groups and commit groups for begins.
//!
//! Each registration takes a slot of its own, on a cache line of its own:
//! a thread reuses the slot it took last, so its begins write a line no
//! other thread writes. Readers of the registry (the transaction manager,
//! under its lock: commits, conflict checks, GC) count the open
//! transactions with one atomic load, and find the oldest snapshot by
//! scanning the slots in use.
//!
//! A begin registers between two reads of the database's publish sequence
//! (see `SingleFileDB::begin_with_mode`) and begins again if a commit group
//! published meanwhile. Commits read the registry inside their publish
//! section, so a commit that does not see a registration is one the
//! registered transaction's snapshot holds. All the atomics here are
//! sequentially consistent, as that argument needs.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use hashbrown::HashMap;
use parking_lot::Mutex;

use crate::types::{Timestamp, TxId};

/// Slots per registry; registrations beyond these go to a locked overflow.
const SLOTS: usize = 64;

/// Where a registered transaction is (see [`OpenTransactions::register`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenSlot {
  /// Slot `.0`.
  Slot(u32),
  /// In the overflow, every slot being taken when it registered.
  Overflow,
}

/// A slot: the id of the transaction holding it (0 when free) and its
/// snapshot, on a cache line of their own.
#[repr(align(128))]
#[derive(Default)]
struct Slot {
  txid: AtomicU64,
  start_ts: AtomicU64,
}

/// A value on a cache line of its own.
#[repr(align(128))]
#[derive(Default)]
struct Line<T>(T);

/// Registries ever made, for each one's id.
static NEXT_REGISTRY: AtomicU64 = AtomicU64::new(1);

thread_local! {
  /// The slot this thread took last in each registry it used lately: (registry id, slot).
  static SLOT_HINTS: RefCell<Vec<(u64, u32)>> = const { RefCell::new(Vec::new()) };
}

/// Registries a thread remembers its last slot in.
const HINTS_KEPT: usize = 8;

/// The open MVCC transactions of a database: their snapshots, without a
/// lock (see the module docs). The transaction manager holds it, and
/// mirrors its next commit timestamp into it, for begins to take as their
/// snapshot.
pub struct OpenTransactions {
  id: u64,
  /// The next commit timestamp (`TxManager::next_commit_ts`): a snapshot
  /// begun now holds every commit before it.
  snapshot_ts: Line<AtomicU64>,
  /// Registered transactions, overflow included.
  count: Line<AtomicUsize>,
  slots: Box<[Slot]>,
  /// Slots `[0, used)` have been taken at some point.
  used: AtomicUsize,
  overflow: Mutex<HashMap<TxId, Timestamp>>,
  /// Transactions in `overflow`, so scans skip its lock while it is empty.
  overflowed: AtomicUsize,
}

impl std::fmt::Debug for OpenTransactions {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("OpenTransactions")
      .field("snapshot_ts", &self.snapshot_ts())
      .field("count", &self.count())
      .finish_non_exhaustive()
  }
}

impl OpenTransactions {
  /// A registry with no transaction open, whose next commit is at
  /// `snapshot_ts`.
  pub fn new(snapshot_ts: Timestamp) -> Self {
    Self {
      id: NEXT_REGISTRY.fetch_add(1, Ordering::Relaxed),
      snapshot_ts: Line(AtomicU64::new(snapshot_ts)),
      count: Line::default(),
      slots: (0..SLOTS).map(|_| Slot::default()).collect(),
      used: AtomicUsize::new(0),
      overflow: Mutex::new(HashMap::new()),
      overflowed: AtomicUsize::new(0),
    }
  }

  /// The snapshot a transaction begun now takes: the next commit timestamp.
  pub fn snapshot_ts(&self) -> Timestamp {
    self.snapshot_ts.0.load(Ordering::SeqCst)
  }

  /// Mirror the transaction manager's next commit timestamp.
  pub(crate) fn set_snapshot_ts(&self, snapshot_ts: Timestamp) {
    self.snapshot_ts.0.store(snapshot_ts, Ordering::SeqCst);
  }

  /// Register transaction `txid` (never 0) as open with snapshot
  /// `start_ts`, in the slot this thread took last if it is free.
  pub fn register(&self, txid: TxId, start_ts: Timestamp) -> OpenSlot {
    debug_assert_ne!(txid, 0, "transaction id 0 marks a free slot");
    let hint = SLOT_HINTS
      .try_with(|hints| {
        hints
          .borrow()
          .iter()
          .find(|&&(id, _)| id == self.id)
          .map(|&(_, slot)| slot as usize)
      })
      .ok()
      .flatten()
      .unwrap_or(0);
    let taken = (hint..SLOTS).chain(0..hint).find(|&index| {
      let slot = &self.slots[index];
      slot.txid.load(Ordering::Relaxed) == 0
        && slot
          .txid
          .compare_exchange(0, txid, Ordering::SeqCst, Ordering::Relaxed)
          .is_ok()
    });
    let place = match taken {
      Some(index) => {
        // A reader that sees the id before the snapshot reads the slot's
        // last snapshot, an older one: it then keeps more, never less.
        self.slots[index].start_ts.store(start_ts, Ordering::SeqCst);
        // Read first: the line is shared, and the slot is mostly in use already.
        if self.used.load(Ordering::SeqCst) <= index {
          self.used.fetch_max(index + 1, Ordering::SeqCst);
        }
        if index != hint {
          self.remember(index as u32);
        }
        OpenSlot::Slot(index as u32)
      }
      None => {
        self.overflow.lock().insert(txid, start_ts);
        self.overflowed.fetch_add(1, Ordering::SeqCst);
        OpenSlot::Overflow
      }
    };
    self.count.0.fetch_add(1, Ordering::SeqCst);
    place
  }

  /// Note `slot` as this thread's slot in this registry.
  fn remember(&self, slot: u32) {
    let _ = SLOT_HINTS.try_with(|hints| {
      let mut hints = hints.borrow_mut();
      match hints.iter_mut().find(|(id, _)| *id == self.id) {
        Some(hint) => hint.1 = slot,
        None => {
          if hints.len() >= HINTS_KEPT {
            hints.remove(0);
          }
          hints.push((self.id, slot));
        }
      }
    });
  }

  /// Unregister transaction `txid` from `slot`, unless it is unregistered
  /// already (each end of a transaction may try: only the first counts).
  /// Returns whether this call unregistered it.
  pub fn unregister(&self, slot: OpenSlot, txid: TxId) -> bool {
    let unregistered = match slot {
      OpenSlot::Slot(index) => self.slots[index as usize]
        .txid
        .compare_exchange(txid, 0, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok(),
      OpenSlot::Overflow => {
        let removed = self.overflow.lock().remove(&txid).is_some();
        if removed {
          self.overflowed.fetch_sub(1, Ordering::SeqCst);
        }
        removed
      }
    };
    if unregistered {
      self.count.0.fetch_sub(1, Ordering::SeqCst);
    }
    unregistered
  }

  /// Registered transactions.
  pub fn count(&self) -> usize {
    self.count.0.load(Ordering::SeqCst)
  }

  /// The oldest registered snapshot, if any (a scan of the slots in use).
  pub fn min_start_ts(&self) -> Option<Timestamp> {
    let mut min = self.overflow_min();
    for slot in &self.slots[..self.used.load(Ordering::SeqCst)] {
      if slot.txid.load(Ordering::SeqCst) != 0 {
        let start_ts = slot.start_ts.load(Ordering::SeqCst);
        min = Some(min.map_or(start_ts, |min: Timestamp| min.min(start_ts)));
      }
    }
    min
  }

  fn overflow_min(&self) -> Option<Timestamp> {
    if self.overflowed.load(Ordering::SeqCst) == 0 {
      return None;
    }
    let overflow = self.overflow.lock();
    overflow.values().copied().min()
  }

  /// The registered transactions' ids.
  pub fn txids(&self) -> Vec<TxId> {
    let mut txids: Vec<TxId> = self.slots[..self.used.load(Ordering::SeqCst)]
      .iter()
      .map(|slot| slot.txid.load(Ordering::SeqCst))
      .filter(|&txid| txid != 0)
      .collect();
    if self.overflowed.load(Ordering::SeqCst) > 0 {
      txids.extend(self.overflow.lock().keys().copied());
    }
    txids
  }

  /// Whether transaction `txid` is registered.
  pub fn contains(&self, txid: TxId) -> bool {
    self.txids().contains(&txid)
  }

  /// Slots taken at some point: what a scan reads.
  pub fn slots_used(&self) -> usize {
    self.used.load(Ordering::SeqCst)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;

  #[test]
  fn registrations_count_and_give_the_oldest_snapshot() {
    let open = OpenTransactions::new(10);
    assert_eq!((open.count(), open.min_start_ts()), (0, None));
    let a = open.register(1, 10);
    let b = open.register(2, 12);
    assert_eq!((open.count(), open.min_start_ts()), (2, Some(10)));
    assert!(open.unregister(a, 1));
    assert!(!open.unregister(a, 1), "a second unregister does nothing");
    assert_eq!((open.count(), open.min_start_ts()), (1, Some(12)));
    assert!(open.unregister(b, 2));
    assert_eq!((open.count(), open.min_start_ts()), (0, None));
  }

  #[test]
  fn a_thread_reuses_its_slot() {
    let open = OpenTransactions::new(1);
    let first = open.register(1, 1);
    open.unregister(first, 1);
    let second = open.register(2, 1);
    assert_eq!(first, second);
    open.unregister(second, 2);
    assert_eq!(open.slots_used(), 1);
  }

  #[test]
  fn unregistering_a_reused_slot_with_an_old_id_leaves_its_holder() {
    let open = OpenTransactions::new(1);
    let slot = open.register(1, 1);
    assert!(open.unregister(slot, 1));
    let again = open.register(2, 1);
    assert_eq!(slot, again);
    assert!(
      !open.unregister(slot, 1),
      "the old id no longer holds the slot"
    );
    assert_eq!(open.count(), 1);
    assert!(open.contains(2));
  }

  #[test]
  fn registrations_past_the_slots_overflow() {
    let open = Arc::new(OpenTransactions::new(5));
    let places: Vec<_> = (1..=SLOTS as u64 + 3)
      .map(|txid| (open.register(txid, 5 + txid), txid))
      .collect();
    assert_eq!(open.count(), SLOTS + 3);
    assert!(places.iter().any(|(place, _)| *place == OpenSlot::Overflow));
    assert_eq!(open.min_start_ts(), Some(6));
    for (place, txid) in places {
      assert!(open.unregister(place, txid));
    }
    assert_eq!((open.count(), open.min_start_ts()), (0, None));
  }

  #[test]
  fn threads_register_and_unregister_concurrently() {
    let open = Arc::new(OpenTransactions::new(1));
    let threads: Vec<_> = (0..8u64)
      .map(|t| {
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
          for i in 0..2_000u64 {
            let txid = t * 1_000_000 + i + 1;
            let place = open.register(txid, i);
            assert!(open.contains(txid));
            assert!(open.unregister(place, txid));
          }
        })
      })
      .collect();
    for thread in threads {
      thread.join().expect("thread");
    }
    assert_eq!((open.count(), open.min_start_ts()), (0, None));
  }
}
