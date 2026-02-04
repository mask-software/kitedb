//! Multi-Version Concurrency Control (MVCC)
//!
//! This module implements MVCC for KiteDB, enabling snapshot isolation for
//! concurrent transactions. MVCC allows multiple transactions to read data
//! concurrently without blocking each other, while maintaining consistency.
//!
//! # Concurrency Model
//!
//! KiteDB supports two levels of concurrency:
//!
//! ## 1. RwLock-based Concurrent Reads
//!
//! At the API level (Kite, NAPI bindings, Python bindings), read operations
//! use a shared read lock (`RwLock::read()`), allowing multiple concurrent
//! readers. Write operations use an exclusive write lock (`RwLock::write()`).
//!
//! ```text
//! ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
//! │  Reader 1   │     │  Reader 2   │     │  Reader 3   │
//! │  (shared)   │     │  (shared)   │     │  (shared)   │
//! └──────┬──────┘     └──────┬──────┘     └──────┬──────┘
//!        │                   │                   │
//!        └───────────────────┼───────────────────┘
//!                            │
//!                     ┌──────▼──────┐
//!                     │   RwLock    │
//!                     │  (shared)   │
//!                     └──────┬──────┘
//!                            │
//!                     ┌──────▼──────┐
//!                     │   KiteDB    │
//!                     └─────────────┘
//! ```
//!
//! ## 2. MVCC Transaction Isolation
//!
//! When MVCC is enabled, transactions get snapshot isolation:
//! - Each transaction sees a consistent snapshot from its start time
//! - Concurrent transactions can read without blocking
//! - Conflicts are detected at commit time (optimistic concurrency)
//!
//! # Components
//!
//! - [`tx_manager`] - Transaction lifecycle management (begin, commit, abort)
//! - [`version_chain`] - Version chain storage for nodes, edges, and properties
//! - [`visibility`] - Visibility rules for determining which versions a transaction can see
//! - [`gc`] - Garbage collection for old versions
//! - [`conflict`] - Conflict detection for optimistic concurrency control
//!
//! # Conflict Types
//!
//! - **Read-Write Conflict**: Transaction read a key modified by a concurrent committed transaction
//! - **Write-Write Conflict**: Transaction wrote a key also written by a concurrent committed transaction
//!
//! See [`ConflictDetector`] for conflict detection APIs.

pub mod conflict;
pub mod gc;
pub mod manager;
pub mod tx_manager;
pub mod version_chain;
pub mod visibility;

// Re-export main types for convenience
pub use conflict::{ConflictDetector, ConflictError, ConflictInfo, ConflictType};
pub use gc::{GarbageCollector, GcConfig, GcResult, GcStats, SharedGcState};
pub use manager::MvccManager;
pub use tx_manager::{CommittedWritesStats, TxManager, TxManagerError};
pub use version_chain::{
  PooledVersion, SoaPropertyVersions, VersionChainCounts, VersionChainManager,
};
pub use visibility::{
  edge_exists, is_visible, node_exists, visible_version, visible_version_mut, EdgeLike,
  VersionedRecord,
};
