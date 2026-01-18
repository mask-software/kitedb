//! Error types for RayDB
//!
//! Uses thiserror for ergonomic error handling

use crate::types::{NodeId, TxId};
use thiserror::Error;

/// Main error type for RayDB operations
#[derive(Error, Debug)]
pub enum RayError {
  /// I/O error from file operations
  #[error("IO error: {0}")]
  Io(#[from] std::io::Error),

  /// Invalid magic number in file header
  #[error("Invalid magic number: expected 0x{expected:08X}, got 0x{got:08X}")]
  InvalidMagic { expected: u32, got: u32 },

  /// Version mismatch - file requires newer reader
  #[error("Version mismatch: file requires version {required}, we support {current}")]
  VersionMismatch { required: u32, current: u32 },

  /// CRC checksum mismatch
  #[error("CRC mismatch: stored 0x{stored:08X}, computed 0x{computed:08X}")]
  CrcMismatch { stored: u32, computed: u32 },

  /// Node not found
  #[error("Node not found: {0}")]
  NodeNotFound(NodeId),

  /// Key not found in index
  #[error("Key not found: {0}")]
  KeyNotFound(String),

  /// Duplicate key - key already exists
  #[error("Duplicate key: {0}")]
  DuplicateKey(String),

  /// Transaction conflict (write-write conflict in MVCC)
  #[error("Transaction {txid} conflict on keys: {keys:?}")]
  Conflict { txid: TxId, keys: Vec<String> },

  /// WAL buffer is full, checkpoint required
  #[error("WAL buffer full: checkpoint required before continuing writes")]
  WalBufferFull,

  /// Attempted write on read-only database
  #[error("Database is read-only")]
  ReadOnly,

  /// Invalid or corrupted snapshot
  #[error("Invalid snapshot: {0}")]
  InvalidSnapshot(String),

  /// Invalid or corrupted WAL
  #[error("Invalid WAL: {0}")]
  InvalidWal(String),

  /// Compression/decompression error
  #[error("Compression error: {0}")]
  Compression(String),

  /// Transaction not active
  #[error("No active transaction")]
  NoTransaction,

  /// Transaction already exists
  #[error("Transaction already in progress")]
  TransactionInProgress,

  /// Database already closed
  #[error("Database is closed")]
  DatabaseClosed,

  /// Lock acquisition failed
  #[error("Failed to acquire lock: {0}")]
  LockFailed(String),

  /// Invalid section ID
  #[error("Invalid section ID: {0}")]
  InvalidSection(u32),

  /// Invalid property value tag
  #[error("Invalid property value tag: {0}")]
  InvalidPropTag(u8),

  /// Invalid WAL record type
  #[error("Invalid WAL record type: {0}")]
  InvalidWalRecordType(u8),

  /// Vector dimension mismatch
  #[error("Vector dimension mismatch: expected {expected}, got {got}")]
  VectorDimensionMismatch { expected: usize, got: usize },

  /// Invalid database path
  #[error("Invalid database path: {0}")]
  InvalidPath(String),

  /// Database creation failed
  #[error("Failed to create database: {0}")]
  CreateFailed(String),

  /// Serialization/deserialization error
  #[error("Serialization error: {0}")]
  Serialization(String),

  /// Internal error (should not happen)
  #[error("Internal error: {0}")]
  Internal(String),

  /// Invalid schema definition
  #[error("Invalid schema: {0}")]
  InvalidSchema(String),
}

/// Result type alias for RayDB operations
pub type Result<T> = std::result::Result<T, RayError>;

/// Conflict error - specialized error for MVCC conflicts
/// Allows extracting conflict details
impl RayError {
  /// Create a conflict error
  pub fn conflict(txid: TxId, keys: Vec<String>) -> Self {
    RayError::Conflict { txid, keys }
  }

  /// Check if this is a conflict error
  pub fn is_conflict(&self) -> bool {
    matches!(self, RayError::Conflict { .. })
  }

  /// Get conflict keys if this is a conflict error
  pub fn conflict_keys(&self) -> Option<&[String]> {
    match self {
      RayError::Conflict { keys, .. } => Some(keys),
      _ => None,
    }
  }
}

// ============================================================================
// Error conversion impls
// ============================================================================

impl From<serde_json::Error> for RayError {
  fn from(err: serde_json::Error) -> Self {
    RayError::Serialization(err.to_string())
  }
}
