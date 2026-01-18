//! Vector embeddings and similarity search
//!
//! This module provides vector storage and approximate nearest neighbor search
//! using IVF (Inverted File) and PQ (Product Quantization) algorithms.
//!
//! # Components
//!
//! - [`types`] - Core types (VectorStoreConfig, VectorManifest, Fragment, etc.)
//! - [`store`] - Columnar vector store with fragment-based storage
//! - [`distance`] - Distance functions (cosine, euclidean, dot product)
//! - [`ivf`] - IVF index for approximate nearest neighbor search
//! - [`pq`] - Product quantization for vector compression
//! - [`ivf_pq`] - Combined IVF-PQ index for efficient approximate nearest neighbor search

pub mod compaction;
pub mod distance;
pub mod fragment;
pub mod ivf;
pub mod ivf_pq;
pub mod normalize;
pub mod pq;
pub mod row_group;
pub mod store;
pub mod types;

// Re-export main types for convenience
pub use distance::{
  batch_cosine_distance, batch_dot_product_distance, batch_squared_euclidean, cosine_distance,
  cosine_similarity, dot_product, dot_product_at, euclidean_distance, l2_norm, normalize,
  normalize_in_place, squared_euclidean, squared_euclidean_at,
};
pub use ivf::{
  IvfError, IvfIndex, IvfStats, KMeansConfig, KMeansError, KMeansResult, SearchOptions,
};
pub use ivf_pq::{
  deserialize_ivf_pq, ivf_pq_serialized_size, serialize_ivf_pq, IvfPqConfig, IvfPqError,
  IvfPqIndex, IvfPqSearchOptions, IvfPqStats,
};
pub use pq::{PqError, PqIndex, PqSearchResult, PqStats};
pub use store::{
  create_vector_store, vector_store_batch_insert, vector_store_clear, vector_store_delete,
  vector_store_fragment_stats, vector_store_get, vector_store_get_all_vectors,
  vector_store_get_by_id, vector_store_get_location, vector_store_get_node_id,
  vector_store_get_vector_id, vector_store_has, vector_store_insert, vector_store_seal_active,
  vector_store_stats, FragmentStats, VectorStoreError, VectorStoreStats,
};
pub use types::{
  DistanceMetric, Fragment, FragmentState, IvfConfig, MultiQueryAggregation, PqConfig, RowGroup,
  VectorLocation, VectorManifest, VectorSearchResult, VectorStoreConfig,
};
