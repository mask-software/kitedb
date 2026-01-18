//! IVF (Inverted File) index
//!
//! Provides approximate nearest neighbor search using inverted file structure.

pub mod index;
pub mod kmeans;
pub mod serialize;

// Re-export main types
pub use index::{IvfError, IvfIndex, IvfStats, SearchOptions};
pub use kmeans::{kmeans, kmeans_parallel, KMeansConfig, KMeansError, KMeansResult};
pub use serialize::{
  deserialize_ivf, deserialize_manifest, ivf_serialized_size, manifest_serialized_size, read_ivf,
  read_manifest, serialize_ivf, serialize_manifest, write_ivf, write_manifest, SerializeError,
};
