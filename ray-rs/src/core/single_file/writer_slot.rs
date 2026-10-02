//! Which write transactions may be open at once.
//!
//! Every write transaction claims the writer slot when it begins and releases
//! it when it is settled. Read-only transactions never claim it.
//! - MVCC write transactions claim it shared: they run together, and conflict
//!   detection refuses a commit that overlaps one committed since it began.
//! - A bulk load claims it exclusively, in either mode: it records nothing for
//!   conflict checks, so no other write transaction may be open beside it.
//! - Without MVCC (deprecated), every write transaction claims it
//!   exclusively: nothing detects conflicts between write transactions, so
//!   two open at once would lose updates (both read, both write, the later
//!   commit wins) and commit edges or vectors whose node the other deleted.
//!   They run one at a time, like SQLite.
//!
//! An exclusive claim waits for every open write transaction to finish, and
//! goes before the shared claims made while it waits, so a steady stream of
//! MVCC writers cannot starve a bulk load.
//!
//! Lock order: the slot is taken first, by a `begin` that holds no other lock
//! and has no transaction open (after its `TransactionInProgress` check, before
//! the checkpoint gate). It is released when the transaction is settled
//! (`ActiveTransactionGuard`, also on every failure path), after the commit's
//! locks and before the auto-checkpoint; a failed `begin` releases it too
//! (`WriterClaim`); and a thread that ends inside its write transaction
//! releases it from its thread-local destructor (`tx_registry`). A holder
//! never waits for a thread that waits for the slot: checkpoints, compaction
//! and background checkpoint installs never take it, and a waiter holds no
//! checkpoint gate permit and does not count as an open transaction.

use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};

/// How a write transaction holds the writer slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterMode {
  /// Beside other shared holders (MVCC write transactions).
  Shared,
  /// Alone (bulk loads, and write transactions without MVCC).
  Exclusive,
}

/// Set in `WriterSlot::word` while an exclusive claim holds or waits for the
/// slot; the other bits count the shared holders.
const BLOCKED: usize = 1 << (usize::BITS - 1);

#[derive(Default)]
struct SlotState {
  /// An exclusive holder has the slot.
  exclusive: bool,
  /// Exclusive claims waiting; shared claims wait behind them.
  exclusive_waiting: usize,
  /// Threads waiting in `claim` (test instrumentation).
  #[cfg(test)]
  waiting: usize,
}

/// Shared claims, the common case (every MVCC write transaction makes one),
/// take and release the slot with one atomic operation on `word` while no
/// exclusive claim holds or waits for it; the rest goes through `state`.
#[derive(Default)]
pub(crate) struct WriterSlot {
  /// The shared holders, and `BLOCKED`, which only changes under `state`'s
  /// lock: set iff `exclusive || exclusive_waiting > 0`.
  word: AtomicUsize,
  state: Mutex<SlotState>,
  released: Condvar,
}

impl WriterSlot {
  /// Wait until the slot can be held in `mode`, then hold it, until the
  /// returned claim is kept (`WriterClaim::keep`) or dropped.
  pub(crate) fn claim(&self, mode: WriterMode) -> WriterClaim<'_> {
    if mode == WriterMode::Shared {
      let mut word = self.word.load(Ordering::Acquire);
      while word & BLOCKED == 0 {
        match self
          .word
          .compare_exchange_weak(word, word + 1, Ordering::AcqRel, Ordering::Acquire)
        {
          Ok(_) => {
            return WriterClaim {
              slot: Some(self),
              mode,
            }
          }
          Err(current) => word = current,
        }
      }
    }
    let mut state = self.state.lock();
    #[cfg(test)]
    {
      state.waiting += 1;
    }
    match mode {
      WriterMode::Shared => {
        while state.exclusive || state.exclusive_waiting > 0 {
          self.released.wait(&mut state);
        }
        // `BLOCKED` is clear, and only set under this lock.
        self.word.fetch_add(1, Ordering::AcqRel);
      }
      WriterMode::Exclusive => {
        state.exclusive_waiting += 1;
        self.word.fetch_or(BLOCKED, Ordering::AcqRel);
        while state.exclusive || self.word.load(Ordering::Acquire) & !BLOCKED > 0 {
          self.released.wait(&mut state);
        }
        state.exclusive_waiting -= 1;
        state.exclusive = true;
      }
    }
    #[cfg(test)]
    {
      state.waiting -= 1;
    }
    WriterClaim {
      slot: Some(self),
      mode,
    }
  }

  /// Threads waiting to claim the slot (test instrumentation).
  #[cfg(test)]
  pub(crate) fn waiting(&self) -> usize {
    self.state.lock().waiting
  }

  /// Shared holders (test instrumentation).
  #[cfg(test)]
  fn shared(&self) -> usize {
    self.word.load(Ordering::Acquire) & !BLOCKED
  }

  /// Release the slot held in `mode` by a kept claim.
  pub(crate) fn release(&self, mode: WriterMode) {
    match mode {
      WriterMode::Shared => {
        let previous = self.word.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
          previous & !BLOCKED > 0,
          "released a shared writer claim nobody held"
        );
        // Only an exclusive claim waits for the shared holders. Notifying
        // under the lock keeps it from missing the wakeup between its check
        // and its wait.
        if previous == BLOCKED | 1 {
          let _state = self.state.lock();
          self.released.notify_all();
        }
      }
      WriterMode::Exclusive => {
        let mut state = self.state.lock();
        debug_assert!(
          state.exclusive,
          "released an exclusive writer claim nobody held"
        );
        state.exclusive = false;
        if state.exclusive_waiting == 0 {
          self.word.fetch_and(!BLOCKED, Ordering::AcqRel);
        }
        self.released.notify_all();
      }
    }
  }
}

/// The slot, held by a `begin` until its transaction is registered.
pub(crate) struct WriterClaim<'a> {
  slot: Option<&'a WriterSlot>,
  mode: WriterMode,
}

impl WriterClaim<'_> {
  /// How the slot is held.
  pub(crate) fn mode(&self) -> WriterMode {
    self.mode
  }

  /// Hand the slot to the transaction; it releases it when settled.
  pub(crate) fn keep(mut self) {
    self.slot = None;
  }
}

impl Drop for WriterClaim<'_> {
  fn drop(&mut self) {
    if let Some(slot) = self.slot {
      slot.release(self.mode);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;
  use std::time::{Duration, Instant};

  fn wait_for_waiters(slot: &WriterSlot, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while slot.waiting() != count {
      assert!(
        Instant::now() < deadline,
        "timed out waiting for {count} waiters"
      );
      std::thread::sleep(Duration::from_millis(1));
    }
  }

  #[test]
  fn shared_claims_hold_the_slot_together() {
    let slot = WriterSlot::default();
    let first = slot.claim(WriterMode::Shared);
    let second = slot.claim(WriterMode::Shared);
    assert_eq!(slot.shared(), 2);
    drop(first);
    drop(second);
    assert_eq!(slot.shared(), 0);
  }

  /// An exclusive claim waits for the shared holders, shared claims made
  /// while it waits wait for it, and each gets the slot in that order.
  #[test]
  fn exclusive_claim_waits_for_shared_holders_and_goes_before_later_ones() {
    let slot = Arc::new(WriterSlot::default());
    slot.claim(WriterMode::Shared).keep();
    let order = Arc::new(Mutex::new(Vec::new()));
    let spawn = |name: &'static str, mode: WriterMode| {
      let slot = Arc::clone(&slot);
      let order = Arc::clone(&order);
      std::thread::spawn(move || {
        let claim = slot.claim(mode);
        order.lock().push(name);
        drop(claim);
      })
    };
    let exclusive = spawn("exclusive", WriterMode::Exclusive);
    wait_for_waiters(&slot, 1);
    let later = spawn("later", WriterMode::Shared);
    wait_for_waiters(&slot, 2);
    assert!(order.lock().is_empty());
    slot.release(WriterMode::Shared);
    exclusive.join().expect("exclusive");
    later.join().expect("later");
    assert_eq!(*order.lock(), vec!["exclusive", "later"]);
  }

  #[test]
  fn shared_claim_waits_for_an_exclusive_holder() {
    let slot = Arc::new(WriterSlot::default());
    slot.claim(WriterMode::Exclusive).keep();
    let waiter = {
      let slot = Arc::clone(&slot);
      std::thread::spawn(move || drop(slot.claim(WriterMode::Shared)))
    };
    wait_for_waiters(&slot, 1);
    slot.release(WriterMode::Exclusive);
    waiter.join().expect("waiter");
    let state = slot.state.lock();
    assert!(!state.exclusive && slot.shared() == 0 && state.exclusive_waiting == 0);
    assert_eq!(slot.word.load(Ordering::Acquire), 0);
  }
}
