//! The single writer of non-MVCC mode.
//!
//! Without MVCC nothing detects conflicts between write transactions, so two
//! open at once lose updates (both read, both write, the later commit wins)
//! and commit edges or vectors whose node the other deleted. Non-MVCC mode
//! therefore runs one write transaction at a time, like SQLite: a write or
//! bulk-load `begin` waits here until no other write transaction is open.
//! Read-only transactions and MVCC mode never use the slot.
//!
//! Interim: this goes away with non-MVCC mode.
//!
//! Lock order: the slot is taken first, by a `begin` that holds no other lock
//! and has no transaction open (after its `TransactionInProgress` check, before
//! the checkpoint gate). It is released when the transaction is settled
//! (`ActiveTransactionGuard`, also on every failure path), after the commit's
//! locks and before the auto-checkpoint; a failed `begin` releases it too
//! (`WriterClaim`); and a thread that ends inside its write transaction
//! releases it from its thread-local destructor (`tx_registry`). The holder
//! never waits for a thread that waits for the slot: checkpoints, compaction
//! and background checkpoint installs never take it, and a waiter holds no
//! checkpoint gate permit and does not count as an open transaction.

use parking_lot::{Condvar, Mutex};

#[derive(Default)]
pub(crate) struct WriterSlot {
  held: Mutex<bool>,
  released: Condvar,
  /// Threads waiting in `claim` (test instrumentation).
  #[cfg(test)]
  waiting: std::sync::atomic::AtomicUsize,
}

impl WriterSlot {
  /// Wait until the slot is free, then take it, until the returned claim is
  /// kept (`WriterClaim::keep`) or dropped.
  pub(crate) fn claim(&self) -> WriterClaim<'_> {
    let mut held = self.held.lock();
    #[cfg(test)]
    self
      .waiting
      .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    while *held {
      self.released.wait(&mut held);
    }
    #[cfg(test)]
    self
      .waiting
      .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    *held = true;
    WriterClaim { slot: Some(self) }
  }

  /// Threads waiting to claim the slot (test instrumentation).
  #[cfg(test)]
  pub(crate) fn waiting(&self) -> usize {
    self.waiting.load(std::sync::atomic::Ordering::SeqCst)
  }

  /// Release the slot taken by a kept claim.
  pub(crate) fn release(&self) {
    let mut held = self.held.lock();
    debug_assert!(*held, "released a writer slot nobody held");
    *held = false;
    self.released.notify_one();
  }
}

/// The slot, held by a `begin` until its transaction is registered.
pub(crate) struct WriterClaim<'a> {
  slot: Option<&'a WriterSlot>,
}

impl WriterClaim<'_> {
  /// Hand the slot to the transaction; it releases it when settled.
  pub(crate) fn keep(mut self) {
    self.slot = None;
  }
}

impl Drop for WriterClaim<'_> {
  fn drop(&mut self) {
    if let Some(slot) = self.slot {
      slot.release();
    }
  }
}
