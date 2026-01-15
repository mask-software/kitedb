//! NAPI bindings for RayDB
//!
//! Exposes SingleFileDB and related types to Node.js/Bun.

pub mod database;
pub mod vector;

pub use database::{
  open_database, Database, DbStats, JsEdge, JsNodeProp, JsPropValue, OpenOptions, PropType,
};

pub use vector::{
  brute_force_search, JsAggregation, JsBruteForceResult, JsDistanceMetric, JsIvfConfig, JsIvfIndex,
  JsIvfPqIndex, JsIvfStats, JsPqConfig, JsSearchOptions, JsSearchResult,
};
