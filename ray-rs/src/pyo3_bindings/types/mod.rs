//! Type definitions for Python bindings
//!
//! This module contains all the basic types used in the Python API:
//! - Property values
//! - Edge types
//! - Node types
//! - Pagination results

pub mod edges;
pub mod nodes;
pub mod results;
pub mod values;

// Re-export all types for convenience
pub use edges::{Edge, FullEdge};
pub use nodes::{EdgeWithProps, NodeProp, NodeWithProps};
pub use results::{EdgePage, NodePage, NodePageIterator};
pub use values::{PropType, PropValue};
