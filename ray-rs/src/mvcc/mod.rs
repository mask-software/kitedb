//! Multi-Version Concurrency Control
//!
//! This module implements MVCC (Multi-Version Concurrency Control) for RayDB,
//! enabling snapshot isolation for concurrent transactions.
//!
//! # Components
//!
//! - [`tx_manager`] - Transaction lifecycle management (begin, commit, abort)
//! - [`version_chain`] - Version chain storage for nodes, edges, and properties
//! - [`visibility`] - Visibility rules for determining which versions a transaction can see
//! - [`gc`] - Garbage collection for old versions
//! - [`conflict`] - Conflict detection for optimistic concurrency control

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
  edge_exists, get_visible_version, get_visible_version_mut, is_visible, node_exists, EdgeLike,
  VersionedRecord,
};
