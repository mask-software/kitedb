//! Error types for KiteDB
//!
//! Uses thiserror for ergonomic error handling

use crate::types::{ETypeId, NodeId, TxId};
use std::borrow::Cow;
use thiserror::Error;

/// Main error type for KiteDB operations
#[derive(Error, Debug)]
pub enum KiteError {
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

  /// Edge not found
  #[error("Edge not found: {src} -[{etype}]-> {dst}")]
  EdgeNotFound {
    src: NodeId,
    etype: ETypeId,
    dst: NodeId,
  },

  /// Key not found in index
  #[error("Key not found: {0}")]
  KeyNotFound(String),

  /// Duplicate key - key already exists
  #[error("Duplicate key: {0}")]
  DuplicateKey(String),

  /// Transaction conflict (write-write conflict)
  #[error("Transaction {txid} conflict on keys: {keys:?}")]
  Conflict { txid: TxId, keys: Vec<String> },

  /// The WAL is full and cannot spill into WAL segments: they are at their
  /// limit (`wal_segment_limit`) and no checkpoint frees them (automatic
  /// checkpoints are off, open write transactions hold their records, or a
  /// blocking checkpoint waits for this writer's transaction), or the record
  /// cannot be written at all. A checkpoint makes room.
  #[error("WAL buffer full: checkpoint required before continuing writes")]
  WalBufferFull,

  /// A background checkpoint did not start, and nothing changed. The reason
  /// says what has to happen first (a blocking checkpoint, optimize, vacuum
  /// or WAL resize waiting for the checkpoint gate gets it first).
  #[error("Background checkpoint declined: {0}")]
  CheckpointDeclined(String),

  /// A write needs WAL segment space that only a checkpoint can free, and
  /// the last automatic checkpoint failed (with the error given; see
  /// `SingleFileDB::checkpoint_error`). Every commit acknowledged so far is
  /// safe; writes succeed again once a checkpoint does.
  #[error("Checkpoint failed, and the WAL segments are full: {0}")]
  CheckpointFailed(String),

  /// The database handle refuses writes for good: an operation panicked
  /// while holding the commit lock, the publish lock or the exclusive
  /// checkpoint gate (a commit, a spill, a checkpoint, optimize, vacuum, a
  /// WAL resize), or a checkpoint run panicked on the checkpoint thread,
  /// possibly between writes that keep memory and disk in step, so writing
  /// more could overwrite log records a durable header still names. Reads go
  /// on; closing persists nothing; a reopen recovers every acknowledged
  /// commit from disk.
  #[error("The database refuses writes until it is reopened: {0}")]
  WritesRefused(String),

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

  /// A savepoint that is not live in the current transaction: taken in
  /// another transaction, released, or taken after the savepoint the
  /// transaction last rolled back to or released.
  #[error("Savepoint is not live: {0}")]
  InvalidSavepoint(String),

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
  InvalidSchema(Cow<'static, str>),

  /// A property value violates its schema (missing required prop or wrong type)
  #[error("Schema violation: {0}")]
  SchemaViolation(String),

  /// Invalid query or builder usage
  #[error("Invalid query: {0}")]
  InvalidQuery(Cow<'static, str>),

  /// Replication metadata/record validation failure
  #[error("Invalid replication state: {0}")]
  InvalidReplication(String),
}

/// Result type alias for KiteDB operations
pub type Result<T> = std::result::Result<T, KiteError>;

/// Conflict error - specialized error for transaction conflicts
/// Allows extracting conflict details
impl KiteError {
  /// Create a conflict error
  pub fn conflict(txid: TxId, keys: Vec<String>) -> Self {
    KiteError::Conflict { txid, keys }
  }

  /// Check if this is a conflict error
  pub fn is_conflict(&self) -> bool {
    matches!(self, KiteError::Conflict { .. })
  }

  /// Get conflict keys if this is a conflict error
  pub fn conflict_keys(&self) -> Option<&[String]> {
    match self {
      KiteError::Conflict { keys, .. } => Some(keys),
      _ => None,
    }
  }
}

// ============================================================================
// Error conversion impls
// ============================================================================

impl From<serde_json::Error> for KiteError {
  fn from(err: serde_json::Error) -> Self {
    KiteError::Serialization(err.to_string())
  }
}
