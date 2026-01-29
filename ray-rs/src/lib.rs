//! RayDB - High-performance embedded graph database
//!
//! A Rust implementation of RayDB with NAPI bindings for Node.js/Bun.
//!
//! # Architecture
//!
//! RayDB uses a **Snapshot + Delta + WAL** architecture:
//!
//! - **Snapshot**: Memory-mapped CSR format for fast reads
//! - **Delta**: In-memory overlay for pending changes
//! - **WAL**: Write-ahead log for durability and crash recovery
//!
//! # Features
//!
//! - Zero-copy reads via mmap
//! - ACID transactions with optional MVCC
//! - Vector embeddings with IVF index
//! - Single-file and multi-file formats
//! - Compression support (zstd, gzip, deflate)

#![deny(clippy::all)]
#![allow(dead_code)] // Allow during development

// Core modules
pub mod constants;
pub mod error;
pub mod types;
pub mod util;

// Storage layer modules (Phase 2)
pub mod core;

// Snapshot integrity checks
pub mod check;

// Graph database modules (Phase 3)
pub mod graph;

// MVCC modules (Phase 4)
pub mod mvcc;

// Vector embeddings modules (Phase 5)
pub mod vector;

// Backup and metrics modules
pub mod backup;
pub mod metrics;

// Cache modules
pub mod cache;

// Export/import
pub mod export;

// Streaming/pagination
pub mod streaming;

// High-level API modules (Phase 6)
pub mod api;

// NAPI bindings module
#[cfg(feature = "napi")]
pub mod napi_bindings;

// PyO3 Python bindings module
#[cfg(feature = "python")]
pub mod pyo3_bindings;

// Re-export commonly used items
pub use error::{RayError, Result};
pub use check::{check_snapshot, quick_check};

// Re-export schema builders for convenience
pub use api::schema::{
  define_edge, define_node, prop, DatabaseSchema, EdgeSchema, NodeSchema, PropDef, SchemaType,
  ValidationError,
};

// ============================================================================
// NAPI Exports
// ============================================================================

#[cfg(feature = "napi")]
use napi_derive::napi;

/// Test function to verify NAPI bindings work
#[cfg(feature = "napi")]
#[napi]
pub fn plus_100(input: u32) -> u32 {
  input + 100
}

/// Get RayDB version
#[cfg(feature = "napi")]
#[napi]
pub fn version() -> String {
  env!("CARGO_PKG_VERSION").to_string()
}

// Re-export the PropValueTag enum for NAPI
pub use types::PropValueTag;

// Re-export NAPI database types
#[cfg(feature = "napi")]
pub use napi_bindings::{
  open_database, ray, Database, DbStats, EdgePage, EdgeWithProps, JsEdge, JsFullEdge, JsNodeProp,
  JsPropValue, NodePage, NodeWithProps, OpenOptions, PaginationOptions, PropType, Ray,
  StreamOptions,
};

// ============================================================================
// PyO3 Exports
// ============================================================================

#[cfg(feature = "python")]
pub use pyo3_bindings::raydb;

// Note: Full NAPI exports will be added as we implement each module
