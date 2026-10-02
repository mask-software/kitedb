//! Replication primitives and runtime wiring.
//!
//! Phase A focuses on deterministic token/cursor parsing and durable sidecar
//! storage primitives.

pub mod durability;
pub mod log_store;
pub mod manifest;
pub mod primary;
pub mod progress;
pub mod replica;
pub mod token;
pub mod transport;
pub mod types;

pub use primary::PrimaryReplicationStatus;
pub use replica::ReplicaReplicationStatus;
pub use types::{CommitToken, ReplicationCursor, ReplicationRole};
