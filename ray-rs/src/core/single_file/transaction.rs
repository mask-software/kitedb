//! Transaction management for SingleFileDB
//!
//! Handles begin, commit, and rollback operations.

use crate::core::wal::record::{
  build_begin_payload, build_commit_payload, build_rollback_payload, WalRecord,
};
use crate::error::{RayError, Result};
use crate::types::*;

use super::open::SyncMode;
use super::{SingleFileDB, SingleFileTxState};

impl SingleFileDB {
  /// Begin a new transaction
  pub fn begin(&self, read_only: bool) -> Result<TxId> {
    if self.read_only && !read_only {
      return Err(RayError::ReadOnly);
    }

    let mut current_tx = self.current_tx.lock();
    if current_tx.is_some() {
      return Err(RayError::TransactionInProgress);
    }

    let txid = self.alloc_tx_id();
    let snapshot_ts = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_nanos() as u64)
      .unwrap_or(0);

    // Write BEGIN record to WAL (for write transactions)
    if !read_only {
      let record = WalRecord::new(WalRecordType::Begin, txid, build_begin_payload());
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;
    }

    *current_tx = Some(SingleFileTxState::new(txid, read_only, snapshot_ts));
    Ok(txid)
  }

  /// Commit the current transaction
  pub fn commit(&self) -> Result<()> {
    // Take the transaction and release the lock immediately
    let tx = {
      let mut current_tx = self.current_tx.lock();
      current_tx.take().ok_or(RayError::NoTransaction)?
    };

    if tx.read_only {
      // Read-only transactions don't need WAL
      return Ok(());
    }

    // Write COMMIT record to WAL
    let record = WalRecord::new(WalRecordType::Commit, tx.txid, build_commit_payload());
    {
      let mut pager = self.pager.lock();
      let mut wal = self.wal_buffer.lock();
      wal.write_record(&record, &mut pager)?;

      // Flush WAL to disk based on sync mode
      match self.sync_mode {
        SyncMode::Full => {
          // Full durability: flush + fsync on every commit (~3ms)
          wal.flush(&mut pager)?;
          pager.sync()?;
        }
        SyncMode::Normal => {
          // Normal: flush to OS cache but no fsync (~1-10us)
          // Data is in kernel buffer cache, safe from app crash
          // Will be synced on checkpoint or close
          wal.flush(&mut pager)?;
          // No pager.sync() - let OS handle write-back
        }
        SyncMode::Off => {
          // Off: don't even flush immediately, just buffer in memory
          // Most dangerous but fastest for bulk operations
          // Data only written when buffer is full or on checkpoint
        }
      }
    }

    // Apply pending vector operations
    self.apply_pending_vectors();

    // Check if auto-checkpoint should be triggered
    // Note: We release all locks above first to avoid deadlock during checkpoint
    if self.auto_checkpoint && self.should_checkpoint(self.checkpoint_threshold) {
      // Don't trigger if checkpoint is already running
      if !self.is_checkpoint_running() {
        // Use background or blocking checkpoint based on config
        let result = if self.background_checkpoint {
          self.background_checkpoint()
        } else {
          self.checkpoint()
        };

        // Log errors but don't fail the commit
        if let Err(e) = result {
          eprintln!("Warning: Auto-checkpoint failed: {e}");
        }
      }
    }

    Ok(())
  }

  /// Rollback the current transaction
  pub fn rollback(&self) -> Result<()> {
    let mut current_tx = self.current_tx.lock();
    let tx = current_tx.take().ok_or(RayError::NoTransaction)?;

    if tx.read_only {
      // Read-only transactions don't need WAL
      return Ok(());
    }

    // Write ROLLBACK record to WAL
    let record = WalRecord::new(WalRecordType::Rollback, tx.txid, build_rollback_payload());
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;

    // Discard pending writes (rollback doesn't need to be durable)
    wal.discard_pending();

    // TODO: Discard delta changes for this transaction

    Ok(())
  }

  /// Check if there's an active transaction
  pub fn has_transaction(&self) -> bool {
    self.current_tx.lock().is_some()
  }

  /// Get the current transaction ID (if any)
  pub fn current_txid(&self) -> Option<TxId> {
    self.current_tx.lock().as_ref().map(|tx| tx.txid)
  }

  /// Write a WAL record (internal helper)
  pub(crate) fn write_wal(&self, record: WalRecord) -> Result<()> {
    let mut pager = self.pager.lock();
    let mut wal = self.wal_buffer.lock();
    wal.write_record(&record, &mut pager)?;
    Ok(())
  }

  /// Get current transaction ID or error
  pub(crate) fn require_write_tx(&self) -> Result<TxId> {
    let current_tx = self.current_tx.lock();
    match current_tx.as_ref() {
      Some(tx) if !tx.read_only => Ok(tx.txid),
      Some(_) => Err(RayError::ReadOnly),
      None => Err(RayError::NoTransaction),
    }
  }
}
