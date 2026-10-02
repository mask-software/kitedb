//! Which transaction each thread has open.
//!
//! A transaction belongs to the thread that began it. Each thread keeps its
//! open transactions in a thread-local list, at most one per database, so a
//! read finds the caller's transaction without any lock shared between
//! threads. (It used to look it up in one map keyed by thread id, under a
//! mutex every read and every begin, commit and rollback in the process took.)
//!
//! A thread that ends with a transaction open abandons it: its thread-local
//! destructor hands the transaction to its database, which rolls it back
//! (`reap_abandoned_transactions`) at the next begin, background checkpoint,
//! or wait for open transactions. Left open, it would hold off every blocking
//! checkpoint forever, and, through its writer slot claim, the writers that
//! claim excludes.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, MutexGuard};

use super::{SingleFileDB, SingleFileTxState};

/// How often a thread waiting for open transactions to finish checks for
/// abandoned ones: a thread that ends inside its transaction cannot signal.
const ABANDONED_TRANSACTION_POLL: Duration = Duration::from_millis(50);

/// What a database shares with the thread-local entries of its transactions.
#[derive(Default)]
pub(crate) struct TxShared {
  /// Transactions whose threads ended with them open, to roll back.
  abandoned: Mutex<Vec<Arc<Mutex<SingleFileTxState>>>>,
  /// Set when a transaction is abandoned, so hot paths check without a lock.
  has_abandoned: AtomicBool,
  /// Which write transactions may be open at once (see `writer_slot`).
  pub(crate) writer: super::writer_slot::WriterSlot,
}

/// A transaction open on the current thread.
struct ThreadTx {
  db: Arc<TxShared>,
  state: Arc<Mutex<SingleFileTxState>>,
}

/// The current thread's open transactions. Dropped when the thread ends,
/// abandoning those still open.
struct ThreadTxs(Vec<ThreadTx>);

impl Drop for ThreadTxs {
  fn drop(&mut self) {
    for tx in self.0.drain(..) {
      // Other writers may be waiting for the slot now; the rollback of the
      // rest waits for the database to reap it.
      {
        let mut state = tx.state.lock();
        if let Some(mode) = state.writer.take() {
          tx.db.writer.release(mode);
        }
      }
      tx.db.abandoned.lock().push(tx.state);
      tx.db.has_abandoned.store(true, Ordering::Release);
    }
  }
}

thread_local! {
  static THREAD_TXS: RefCell<ThreadTxs> = const { RefCell::new(ThreadTxs(Vec::new())) };
}

impl SingleFileDB {
  /// The calling thread's open transaction on this database.
  pub(crate) fn current_tx_handle(&self) -> Option<Arc<Mutex<SingleFileTxState>>> {
    THREAD_TXS
      .try_with(|txs| {
        txs
          .borrow()
          .0
          .iter()
          .find(|tx| Arc::ptr_eq(&tx.db, &self.tx_shared))
          .map(|tx| Arc::clone(&tx.state))
      })
      .ok()
      .flatten()
  }

  /// Make `state` the calling thread's open transaction on this database.
  /// The caller checked that it has none.
  pub(crate) fn register_thread_transaction(&self, state: Arc<Mutex<SingleFileTxState>>) {
    THREAD_TXS.with(|txs| {
      let mut txs = txs.borrow_mut();
      // Entries of databases dropped with a transaction open (only this list
      // still holds them) go now.
      txs.0.retain(|tx| Arc::strong_count(&tx.db) > 1);
      txs.0.push(ThreadTx {
        db: Arc::clone(&self.tx_shared),
        state,
      });
    });
  }

  /// Remove and return the calling thread's open transaction on this
  /// database, to commit or roll back.
  pub(crate) fn take_thread_transaction(&self) -> Option<Arc<Mutex<SingleFileTxState>>> {
    THREAD_TXS
      .try_with(|txs| {
        let mut txs = txs.borrow_mut();
        let index = txs
          .0
          .iter()
          .position(|tx| Arc::ptr_eq(&tx.db, &self.tx_shared))?;
        Some(txs.0.swap_remove(index).state)
      })
      .ok()
      .flatten()
  }

  /// Roll back the transactions whose threads ended with them open. Callers
  /// hold no lock a rollback takes.
  pub(crate) fn reap_abandoned_transactions(&self) {
    if !self.tx_shared.has_abandoned.swap(false, Ordering::AcqRel) {
      return;
    }
    let abandoned = std::mem::take(&mut *self.tx_shared.abandoned.lock());
    for state in abandoned {
      let txid = state.lock().txid;
      if let Err(error) = self.rollback_transaction(&state) {
        eprintln!(
          "Warning: rolling back transaction {txid}, abandoned by a thread that ended with it \
           open, failed: {error}"
        );
      }
    }
  }

  /// Wait until no transaction is open, rolling back abandoned ones. Callers
  /// may hold the checkpoint gate (which a rollback does not take).
  pub(crate) fn wait_for_no_active_transactions(&self) {
    let mut wait = self.checkpoint_wait.lock();
    while self.active_transactions.load(Ordering::Acquire) != 0 {
      if self.tx_shared.has_abandoned.load(Ordering::Acquire) {
        MutexGuard::unlocked(&mut wait, || self.reap_abandoned_transactions());
        continue;
      }
      self
        .checkpoint_cv
        .wait_for(&mut wait, ABANDONED_TRANSACTION_POLL);
    }
  }
}
