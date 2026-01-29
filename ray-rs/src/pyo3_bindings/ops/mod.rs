//! Database operations organized as trait extensions
//!
//! This module provides trait-based organization of database operations.
//! Each trait groups related functionality and can be implemented for the
//! Database struct.
//!
//! The traits are designed to be used internally - the actual Python methods
//! are defined in the main database module and delegate to these traits.

pub mod cache;
pub mod edges;
pub mod export_import;
pub mod graph_traversal;
pub mod labels;
pub mod maintenance;
pub mod nodes;
pub mod properties;
pub mod schema;
pub mod streaming;
pub mod transaction;
pub mod vectors;

// Re-export all operation traits
pub use cache::CacheOps;
pub use edges::EdgeOps;
pub use export_import::ExportImportOps;
pub use graph_traversal::GraphTraversalOps;
pub use labels::LabelOps;
pub use maintenance::MaintenanceOps;
pub use nodes::NodeOps;
pub use properties::PropertyOps;
pub use schema::SchemaOps;
pub use streaming::StreamingOps;
pub use transaction::TransactionOps;
pub use vectors::VectorOps;
