//! NAPI bindings for RayDB
//!
//! Exposes SingleFileDB and related types to Node.js/Bun.

pub mod database;
pub mod traversal;
pub mod vector;

pub use database::{
  open_database, Database, DbStats, JsEdge, JsFullEdge, JsNodeProp, JsPropValue, OpenOptions,
  PropType,
};

pub use traversal::{
  path_config, traversal_step, JsEdgeInput, JsGraphAccessor, JsPathConfig, JsPathEdge,
  JsPathResult, JsTraversalDirection, JsTraversalResult, JsTraversalStep, JsTraverseOptions,
};

pub use vector::{
  brute_force_search, JsAggregation, JsBruteForceResult, JsDistanceMetric, JsIvfConfig, JsIvfIndex,
  JsIvfPqIndex, JsIvfStats, JsPqConfig, JsSearchOptions, JsSearchResult,
};
