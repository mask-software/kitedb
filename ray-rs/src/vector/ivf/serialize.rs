//! Binary serialization for IVF index and vector store
//!
//! Provides efficient binary serialization/deserialization for IVF indexes
//! and vector manifests.
//!
//! Ported from src/vector/ivf-serialize.ts

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};

use crate::vector::ivf::IvfIndex;
use crate::vector::store::validate_vector_manifest;
use crate::vector::types::{
  DistanceMetric, Fragment, FragmentState, IvfConfig, RowGroup, VectorLocation, VectorManifest,
  VectorStoreConfig,
};

// ============================================================================
// Constants
// ============================================================================

/// Magic number for IVF index: "IVF1"
const IVF_MAGIC: u32 = 0x49564631;
/// Header size for IVF index
const IVF_HEADER_SIZE: usize = 32;

/// Magic number for vector manifest: "VEC1"
const MANIFEST_MAGIC: u32 = 0x56454331;
/// Header size for vector manifest (4+4+4+4+4+4+4+4+4+4+8+20 = 68)
const MANIFEST_HEADER_SIZE: usize = 68;

/// Fragment header size
const FRAGMENT_HEADER_SIZE: usize = 32;
/// Row group header size
const ROW_GROUP_HEADER_SIZE: usize = 16;

// ============================================================================
// Errors
// ============================================================================

/// Serialization errors
#[derive(Debug)]
pub enum SerializeError {
  /// IO error during read/write
  Io(io::Error),
  /// Invalid magic number
  InvalidMagic { expected: u32, got: u32 },
  /// Buffer underflow
  BufferUnderflow {
    context: String,
    offset: usize,
    needed: usize,
    available: usize,
  },
  /// Invalid metric value
  InvalidMetric(u32),
  /// Structurally inconsistent input
  InvalidStructure(String),
}

impl std::fmt::Display for SerializeError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      SerializeError::Io(e) => write!(f, "IO error: {e}"),
      SerializeError::InvalidMagic { expected, got } => {
        write!(
          f,
          "Invalid magic: expected 0x{expected:08X}, got 0x{got:08X}"
        )
      }
      SerializeError::BufferUnderflow {
        context,
        offset,
        needed,
        available,
      } => {
        write!(
          f,
          "Buffer underflow in {context}: need {needed} bytes at offset {offset}, but only {available} available"
        )
      }
      SerializeError::InvalidMetric(n) => {
        write!(
          f,
          "Invalid metric value: {n}. Expected 0 (cosine), 1 (euclidean), or 2 (dot)"
        )
      }
      SerializeError::InvalidStructure(msg) => write!(f, "Invalid vector structure: {msg}"),
    }
  }
}

impl std::error::Error for SerializeError {}

impl From<io::Error> for SerializeError {
  fn from(e: io::Error) -> Self {
    SerializeError::Io(e)
  }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Convert DistanceMetric to u8
fn metric_to_u8(metric: DistanceMetric) -> u8 {
  match metric {
    DistanceMetric::Cosine => 0,
    DistanceMetric::Euclidean => 1,
    DistanceMetric::DotProduct => 2,
  }
}

/// Convert u8 to DistanceMetric
fn u8_to_metric(n: u8) -> Result<DistanceMetric, SerializeError> {
  match n {
    0 => Ok(DistanceMetric::Cosine),
    1 => Ok(DistanceMetric::Euclidean),
    2 => Ok(DistanceMetric::DotProduct),
    _ => Err(SerializeError::InvalidMetric(n as u32)),
  }
}

fn metric_from_u32(n: u32) -> Result<DistanceMetric, SerializeError> {
  match n {
    0 => Ok(DistanceMetric::Cosine),
    1 => Ok(DistanceMetric::Euclidean),
    2 => Ok(DistanceMetric::DotProduct),
    _ => Err(SerializeError::InvalidMetric(n)),
  }
}

/// Ensure buffer has enough bytes remaining
fn ensure_bytes(
  buf_len: usize,
  offset: usize,
  needed: usize,
  context: &str,
) -> Result<(), SerializeError> {
  let end = offset
    .checked_add(needed)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} size overflow")))?;
  if end > buf_len {
    return Err(SerializeError::BufferUnderflow {
      context: context.to_string(),
      offset,
      needed,
      available: buf_len.saturating_sub(offset),
    });
  }
  Ok(())
}

fn ensure_count_bytes(
  buf_len: usize,
  offset: usize,
  count: usize,
  minimum_size: usize,
  context: &str,
) -> Result<(), SerializeError> {
  let needed = count
    .checked_mul(minimum_size)
    .ok_or_else(|| SerializeError::InvalidStructure(format!("{context} count size overflow")))?;
  ensure_bytes(buf_len, offset, needed, context)
}

fn read_u32_at(buffer: &[u8], offset: usize, context: &str) -> Result<u32, SerializeError> {
  ensure_bytes(buffer.len(), offset, 4, context)?;
  let mut bytes = [0u8; 4];
  bytes.copy_from_slice(&buffer[offset..offset + 4]);
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64_at(buffer: &[u8], offset: usize, context: &str) -> Result<u64, SerializeError> {
  ensure_bytes(buffer.len(), offset, 8, context)?;
  let mut bytes = [0u8; 8];
  bytes.copy_from_slice(&buffer[offset..offset + 8]);
  Ok(u64::from_le_bytes(bytes))
}

fn read_f32_at(buffer: &[u8], offset: usize, context: &str) -> Result<f32, SerializeError> {
  let bits = read_u32_at(buffer, offset, context)?;
  Ok(f32::from_bits(bits))
}

// ============================================================================
// IVF Index Serialization
// ============================================================================

/// Calculate serialized size of IVF index
pub fn ivf_serialized_size(index: &IvfIndex) -> usize {
  let mut size = IVF_HEADER_SIZE;

  // Centroid count + centroids
  size += 4 + index.centroids.len() * 4;

  // Number of lists
  size += 4;

  // Inverted lists
  for list in index.inverted_lists.values() {
    size += 4 + 4 + list.len() * 8; // cluster ID + list length + vector IDs (u64)
  }

  size
}

/// Serialize IVF index to binary
///
/// # Format
/// - Header (32 bytes)
///   - magic (4): "IVF1"
///   - n_clusters (4)
///   - dimensions (4)
///   - n_probe (4)
///   - trained (1)
///   - reserved (1)
///   - metric (1): 0=cosine, 1=euclidean, 2=dot
///   - reserved (13)
/// - centroid_count (4) - actual number of f32 values in centroids
/// - Centroids (centroid_count * 4 bytes)
/// - num_lists (4)
/// - For each inverted list:
///   - cluster ID (4)
///   - list length (4)
///   - vector IDs (length * 8)
pub fn serialize_ivf(index: &IvfIndex) -> Vec<u8> {
  let size = ivf_serialized_size(index);
  let mut buffer = Vec::with_capacity(size);

  // Header
  buffer.extend_from_slice(&IVF_MAGIC.to_le_bytes());
  buffer.extend_from_slice(&(index.config.n_clusters as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.dimensions as u32).to_le_bytes());
  buffer.extend_from_slice(&(index.config.n_probe as u32).to_le_bytes());
  buffer.push(if index.trained { 1 } else { 0 });
  buffer.push(0); // reserved
  buffer.push(metric_to_u8(index.config.metric));
  buffer.extend_from_slice(&[0u8; 13]); // reserved

  // Centroid count + Centroids
  buffer.extend_from_slice(&(index.centroids.len() as u32).to_le_bytes());
  for &val in &index.centroids {
    buffer.extend_from_slice(&val.to_le_bytes());
  }

  // Inverted lists
  buffer.extend_from_slice(&(index.inverted_lists.len() as u32).to_le_bytes());

  for (&cluster, list) in &index.inverted_lists {
    buffer.extend_from_slice(&(cluster as u32).to_le_bytes());
    buffer.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for &vector_id in list {
      buffer.extend_from_slice(&vector_id.to_le_bytes());
    }
  }

  buffer
}

/// Deserialize IVF index from binary
pub fn deserialize_ivf(buffer: &[u8]) -> Result<IvfIndex, SerializeError> {
  let buf_len = buffer.len();
  ensure_bytes(buf_len, 0, IVF_HEADER_SIZE, "IVF header")?;

  let mut offset = 0;

  // Header
  let magic = read_u32_at(buffer, offset, "IVF magic")?;
  offset += 4;
  if magic != IVF_MAGIC {
    return Err(SerializeError::InvalidMagic {
      expected: IVF_MAGIC,
      got: magic,
    });
  }

  let n_clusters = read_u32_at(buffer, offset, "IVF n_clusters")? as usize;
  offset += 4;
  let dimensions = read_u32_at(buffer, offset, "IVF dimensions")? as usize;
  offset += 4;
  let n_probe = read_u32_at(buffer, offset, "IVF n_probe")? as usize;
  offset += 4;
  let trained = match buffer[offset] {
    0 => false,
    1 => true,
    value => {
      return Err(SerializeError::InvalidStructure(format!(
        "IVF trained flag {value} is invalid"
      )));
    }
  };
  offset += 1;
  offset += 1; // skip reserved
  let metric = u8_to_metric(buffer[offset])?;
  offset += 1;
  offset += 13; // skip reserved

  if n_clusters == 0 {
    return Err(SerializeError::InvalidStructure(
      "IVF n_clusters must be nonzero".into(),
    ));
  }
  if dimensions == 0 {
    return Err(SerializeError::InvalidStructure(
      "IVF dimensions must be nonzero".into(),
    ));
  }
  if n_probe == 0 {
    return Err(SerializeError::InvalidStructure(
      "IVF n_probe must be nonzero".into(),
    ));
  }

  let config = IvfConfig {
    n_clusters,
    n_probe,
    metric,
  };

  // Centroid count + Centroids
  ensure_bytes(buf_len, offset, 4, "IVF centroid count")?;
  let centroid_count = read_u32_at(buffer, offset, "IVF centroid count")? as usize;
  offset += 4;

  ensure_count_bytes(buf_len, offset, centroid_count, 4, "IVF centroids")?;
  let expected_centroid_count = if trained {
    n_clusters
      .checked_mul(dimensions)
      .ok_or_else(|| SerializeError::InvalidStructure("IVF centroid shape overflow".into()))?
  } else {
    0
  };
  if centroid_count != expected_centroid_count {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF centroid count {centroid_count} does not match expected {expected_centroid_count}"
    )));
  }

  let mut centroids = Vec::with_capacity(centroid_count);
  for _ in 0..centroid_count {
    let val = read_f32_at(buffer, offset, "IVF centroid")?;
    centroids.push(val);
    offset += 4;
  }

  // Inverted lists
  ensure_bytes(buf_len, offset, 4, "IVF inverted list count")?;
  let num_lists = read_u32_at(buffer, offset, "IVF inverted list count")? as usize;
  offset += 4;
  if num_lists > n_clusters {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF inverted list count {num_lists} exceeds n_clusters {n_clusters}"
    )));
  }
  if !trained && num_lists != 0 {
    return Err(SerializeError::InvalidStructure(
      "untrained IVF contains inverted lists".into(),
    ));
  }
  ensure_count_bytes(buf_len, offset, num_lists, 8, "IVF inverted list headers")?;

  let mut inverted_lists: HashMap<usize, Vec<u64>> = HashMap::with_capacity(num_lists);
  let mut vector_ids = HashSet::new();

  for i in 0..num_lists {
    let cluster = read_u32_at(buffer, offset, "IVF inverted list cluster")? as usize;
    offset += 4;
    if cluster >= n_clusters {
      return Err(SerializeError::InvalidStructure(format!(
        "IVF inverted list {i} cluster {cluster} is outside n_clusters {n_clusters}"
      )));
    }
    if inverted_lists.contains_key(&cluster) {
      return Err(SerializeError::InvalidStructure(format!(
        "duplicate IVF inverted list cluster {cluster}"
      )));
    }
    let list_length = read_u32_at(buffer, offset, "IVF inverted list length")? as usize;
    offset += 4;

    ensure_count_bytes(
      buf_len,
      offset,
      list_length,
      8,
      &format!("IVF inverted list {i} data"),
    )?;
    let mut list = Vec::with_capacity(list_length);
    for _ in 0..list_length {
      let vector_id = read_u64_at(buffer, offset, "IVF vector id")?;
      list.push(vector_id);
      if !vector_ids.insert(vector_id) {
        return Err(SerializeError::InvalidStructure(format!(
          "duplicate IVF vector id {vector_id} in inverted lists"
        )));
      }
      offset += 8;
    }

    inverted_lists.insert(cluster, list);
  }

  if offset != buf_len {
    return Err(SerializeError::InvalidStructure(format!(
      "IVF payload has {} trailing bytes",
      buf_len - offset
    )));
  }

  Ok(IvfIndex::from_serialized(
    config,
    centroids,
    inverted_lists,
    dimensions,
    trained,
  ))
}

// ============================================================================
// Vector Manifest Serialization
// ============================================================================

/// Calculate serialized size of vector manifest
pub fn manifest_serialized_size(manifest: &VectorManifest) -> usize {
  let mut size = MANIFEST_HEADER_SIZE;

  // Fragments
  for fragment in &manifest.fragments {
    size += FRAGMENT_HEADER_SIZE;

    // Row groups
    for rg in &fragment.row_groups {
      size += ROW_GROUP_HEADER_SIZE;
      size += rg.data.len() * 4; // f32 data
    }

    // Deletion bitmap
    size += fragment.deletion_bitmap.len() * 4;
  }

  // Node ID to Vector ID mapping
  size += 4; // count
  size += manifest.node_to_vector.len() * 16; // nodeId (8) + vectorId (8)

  // Vector ID to Location mapping
  size += 4; // count
  size += manifest.vector_locations.len() * 16; // vectorId (8) + fragmentId (4) + localIndex (4)

  size
}

/// Serialize vector manifest to binary
pub fn serialize_manifest(manifest: &VectorManifest) -> Vec<u8> {
  let size = manifest_serialized_size(manifest);
  let mut buffer = Vec::with_capacity(size);

  // Header
  buffer.extend_from_slice(&MANIFEST_MAGIC.to_le_bytes());
  buffer.extend_from_slice(&(manifest.config.dimensions as u32).to_le_bytes());
  buffer.extend_from_slice(&(metric_to_u8(manifest.config.metric) as u32).to_le_bytes());
  buffer.extend_from_slice(&(manifest.config.row_group_size as u32).to_le_bytes());
  buffer.extend_from_slice(&(manifest.config.fragment_target_size as u32).to_le_bytes());
  buffer.push(if manifest.config.normalize_on_insert {
    1
  } else {
    0
  });
  buffer.extend_from_slice(&[0u8; 3]); // padding
  buffer.extend_from_slice(&(manifest.fragments.len() as u32).to_le_bytes());
  buffer.extend_from_slice(&(manifest.active_fragment_id as u32).to_le_bytes());
  buffer.extend_from_slice(&(manifest.total_vectors as u32).to_le_bytes());
  buffer.extend_from_slice(&(manifest.total_deleted as u32).to_le_bytes());
  buffer.extend_from_slice(&manifest.next_vector_id.to_le_bytes());
  buffer.extend_from_slice(&[0u8; 20]); // reserved

  // Fragments
  for fragment in &manifest.fragments {
    // Fragment header
    buffer.extend_from_slice(&(fragment.id as u32).to_le_bytes());
    buffer.push(if fragment.state == FragmentState::Active {
      0
    } else {
      1
    });
    buffer.extend_from_slice(&[0u8; 3]); // padding
    buffer.extend_from_slice(&(fragment.row_groups.len() as u32).to_le_bytes());
    buffer.extend_from_slice(&(fragment.total_vectors as u32).to_le_bytes());
    buffer.extend_from_slice(&(fragment.deleted_count as u32).to_le_bytes());
    buffer.extend_from_slice(&((fragment.deletion_bitmap.len() * 4) as u32).to_le_bytes());
    buffer.extend_from_slice(&[0u8; 8]); // reserved

    // Row groups
    for rg in &fragment.row_groups {
      buffer.extend_from_slice(&(rg.id as u32).to_le_bytes());
      buffer.extend_from_slice(&(rg.count as u32).to_le_bytes());
      buffer.extend_from_slice(&((rg.data.len() * 4) as u32).to_le_bytes());
      buffer.extend_from_slice(&[0u8; 4]); // reserved

      // Row group data
      for &val in &rg.data {
        buffer.extend_from_slice(&val.to_le_bytes());
      }
    }

    // Deletion bitmap
    for &word in &fragment.deletion_bitmap {
      buffer.extend_from_slice(&word.to_le_bytes());
    }
  }

  // Node ID to Vector ID mapping
  buffer.extend_from_slice(&(manifest.node_to_vector.len() as u32).to_le_bytes());
  for (&node_id, &vector_id) in &manifest.node_to_vector {
    buffer.extend_from_slice(&node_id.to_le_bytes());
    buffer.extend_from_slice(&vector_id.to_le_bytes()); // u64
  }

  // Vector ID to Location mapping
  buffer.extend_from_slice(&(manifest.vector_locations.len() as u32).to_le_bytes());
  for (&vector_id, location) in &manifest.vector_locations {
    buffer.extend_from_slice(&vector_id.to_le_bytes());
    buffer.extend_from_slice(&(location.fragment_id as u32).to_le_bytes());
    buffer.extend_from_slice(&(location.local_index as u32).to_le_bytes());
  }

  buffer
}

/// Deserialize vector manifest from binary
pub fn deserialize_manifest(buffer: &[u8]) -> Result<VectorManifest, SerializeError> {
  let buf_len = buffer.len();
  ensure_bytes(buf_len, 0, MANIFEST_HEADER_SIZE, "manifest header")?;

  let mut offset = 0;

  // Header
  let magic = read_u32_at(buffer, offset, "manifest magic")?;
  offset += 4;
  if magic != MANIFEST_MAGIC {
    return Err(SerializeError::InvalidMagic {
      expected: MANIFEST_MAGIC,
      got: magic,
    });
  }

  let dimensions = read_u32_at(buffer, offset, "manifest dimensions")? as usize;
  offset += 4;
  let metric = metric_from_u32(read_u32_at(buffer, offset, "manifest metric")?)?;
  offset += 4;
  let row_group_size = read_u32_at(buffer, offset, "manifest row_group_size")? as usize;
  offset += 4;
  let fragment_target_size = read_u32_at(buffer, offset, "manifest fragment_target_size")? as usize;
  offset += 4;
  let normalize_on_insert = match buffer[offset] {
    0 => false,
    1 => true,
    value => {
      return Err(SerializeError::InvalidStructure(format!(
        "manifest normalize_on_insert flag {value} is invalid"
      )));
    }
  };
  offset += 1;
  offset += 3; // padding
  let num_fragments = read_u32_at(buffer, offset, "manifest num_fragments")? as usize;
  offset += 4;
  let active_fragment_id = read_u32_at(buffer, offset, "manifest active_fragment_id")? as usize;
  offset += 4;
  let total_vectors = read_u32_at(buffer, offset, "manifest total_vectors")? as usize;
  offset += 4;
  let total_deleted = read_u32_at(buffer, offset, "manifest total_deleted")? as usize;
  offset += 4;
  let next_vector_id = read_u64_at(buffer, offset, "manifest next_vector_id")?;
  offset += 8;
  offset += 20; // reserved

  if dimensions == 0 {
    return Err(SerializeError::InvalidStructure(
      "manifest dimensions must be nonzero".into(),
    ));
  }
  if row_group_size == 0 {
    return Err(SerializeError::InvalidStructure(
      "manifest row_group_size must be nonzero".into(),
    ));
  }
  if fragment_target_size == 0 {
    return Err(SerializeError::InvalidStructure(
      "manifest fragment_target_size must be nonzero".into(),
    ));
  }
  if num_fragments == 0 {
    return Err(SerializeError::InvalidStructure(
      "manifest must contain at least one fragment".into(),
    ));
  }

  let config = VectorStoreConfig {
    dimensions,
    metric,
    row_group_size,
    fragment_target_size,
    normalize_on_insert,
  };

  // Fragments
  ensure_count_bytes(
    buf_len,
    offset,
    num_fragments,
    FRAGMENT_HEADER_SIZE,
    "manifest fragment headers",
  )?;
  let mut fragments: Vec<Fragment> = Vec::with_capacity(num_fragments);

  for f in 0..num_fragments {
    ensure_bytes(
      buf_len,
      offset,
      FRAGMENT_HEADER_SIZE,
      &format!("fragment {f} header"),
    )?;

    let id = read_u32_at(buffer, offset, "fragment id")? as usize;
    offset += 4;
    let state = match buffer[offset] {
      0 => FragmentState::Active,
      1 => FragmentState::Sealed,
      value => {
        return Err(SerializeError::InvalidStructure(format!(
          "fragment {f} state {value} is invalid"
        )));
      }
    };
    offset += 1;
    offset += 3; // padding
    let num_row_groups = read_u32_at(buffer, offset, "fragment num_row_groups")? as usize;
    offset += 4;
    let frag_total_vectors = read_u32_at(buffer, offset, "fragment total_vectors")? as usize;
    offset += 4;
    let deleted_count = read_u32_at(buffer, offset, "fragment deleted_count")? as usize;
    offset += 4;
    let deletion_bitmap_length =
      read_u32_at(buffer, offset, "fragment deletion_bitmap_length")? as usize;
    offset += 4;
    offset += 8; // reserved

    // Row groups
    ensure_count_bytes(
      buf_len,
      offset,
      num_row_groups,
      ROW_GROUP_HEADER_SIZE,
      &format!("fragment {f} row group headers"),
    )?;
    let mut row_groups: Vec<RowGroup> = Vec::with_capacity(num_row_groups);

    for r in 0..num_row_groups {
      ensure_bytes(
        buf_len,
        offset,
        ROW_GROUP_HEADER_SIZE,
        &format!("fragment {f} row group {r} header"),
      )?;

      let rg_id = read_u32_at(buffer, offset, "row group id")? as usize;
      offset += 4;
      let count = read_u32_at(buffer, offset, "row group count")? as usize;
      offset += 4;
      let data_length = read_u32_at(buffer, offset, "row group data_length")? as usize;
      offset += 4;
      offset += 4; // reserved

      if data_length % 4 != 0 {
        return Err(SerializeError::InvalidStructure(format!(
          "fragment {f} row group {r} data length {data_length} is not a multiple of 4"
        )));
      }
      let expected_value_count = count.checked_mul(dimensions).ok_or_else(|| {
        SerializeError::InvalidStructure(format!("fragment {f} row group {r} value count overflow"))
      })?;
      let expected_data_length = expected_value_count.checked_mul(4).ok_or_else(|| {
        SerializeError::InvalidStructure(format!("fragment {f} row group {r} data length overflow"))
      })?;
      if data_length != expected_data_length {
        return Err(SerializeError::InvalidStructure(format!(
          "fragment {f} row group {r} data length {data_length} does not match count {count} * dimensions {dimensions} * 4"
        )));
      }

      // Copy row group data
      ensure_bytes(
        buf_len,
        offset,
        data_length,
        &format!("fragment {f} row group {r} data"),
      )?;
      let mut data = Vec::with_capacity(data_length / 4);
      for _ in 0..(data_length / 4) {
        let val = read_f32_at(buffer, offset, "row group data")?;
        data.push(val);
        offset += 4;
      }

      row_groups.push(RowGroup {
        id: rg_id,
        count,
        data,
      });
    }

    // Deletion bitmap
    if deletion_bitmap_length % 4 != 0 {
      return Err(SerializeError::InvalidStructure(format!(
        "fragment {f} deletion bitmap length {deletion_bitmap_length} is not a multiple of 4"
      )));
    }
    ensure_bytes(
      buf_len,
      offset,
      deletion_bitmap_length,
      &format!("fragment {f} deletion bitmap"),
    )?;
    let mut deletion_bitmap = Vec::with_capacity(deletion_bitmap_length / 4);
    for _ in 0..(deletion_bitmap_length / 4) {
      let word = read_u32_at(buffer, offset, "fragment deletion bitmap")?;
      deletion_bitmap.push(word);
      offset += 4;
    }

    fragments.push(Fragment {
      id,
      state,
      row_groups,
      total_vectors: frag_total_vectors,
      deletion_bitmap,
      deleted_count,
    });
  }

  // Node ID to Vector ID mapping
  ensure_bytes(buf_len, offset, 4, "node-to-vector mapping count")?;
  let node_to_vector_count = read_u32_at(buffer, offset, "node-to-vector count")? as usize;
  offset += 4;

  ensure_count_bytes(
    buf_len,
    offset,
    node_to_vector_count,
    16,
    "node-to-vector mapping data",
  )?;
  let mut node_to_vector: HashMap<u64, u64> = HashMap::with_capacity(node_to_vector_count);
  let mut vector_to_node: HashMap<u64, u64> = HashMap::with_capacity(node_to_vector_count);

  for _ in 0..node_to_vector_count {
    let node_id = read_u64_at(buffer, offset, "node-to-vector node_id")?;
    offset += 8;
    let vector_id = read_u64_at(buffer, offset, "node-to-vector vector_id")?;
    offset += 8;
    if node_to_vector.contains_key(&node_id) || vector_to_node.contains_key(&vector_id) {
      return Err(SerializeError::InvalidStructure(
        "duplicate node-to-vector mapping key".into(),
      ));
    }
    node_to_vector.insert(node_id, vector_id);
    vector_to_node.insert(vector_id, node_id);
  }

  // Vector ID to Location mapping
  ensure_bytes(buf_len, offset, 4, "vector-to-location mapping count")?;
  let vector_to_location_count = read_u32_at(buffer, offset, "vector-to-location count")? as usize;
  offset += 4;

  ensure_count_bytes(
    buf_len,
    offset,
    vector_to_location_count,
    16,
    "vector-to-location mapping data",
  )?;
  let mut vector_locations: HashMap<u64, VectorLocation> =
    HashMap::with_capacity(vector_to_location_count);

  for _ in 0..vector_to_location_count {
    let vector_id = read_u64_at(buffer, offset, "vector-to-location vector_id")?;
    offset += 8;
    let fragment_id = read_u32_at(buffer, offset, "vector-to-location fragment_id")? as usize;
    offset += 4;
    let local_index = read_u32_at(buffer, offset, "vector-to-location local_index")? as usize;
    offset += 4;
    if vector_locations.contains_key(&vector_id) {
      return Err(SerializeError::InvalidStructure(format!(
        "duplicate vector-to-location mapping for vector id {vector_id}"
      )));
    }
    vector_locations.insert(
      vector_id,
      VectorLocation {
        fragment_id,
        local_index,
      },
    );
  }

  if offset != buf_len {
    return Err(SerializeError::InvalidStructure(format!(
      "manifest payload has {} trailing bytes",
      buf_len - offset
    )));
  }

  let manifest = VectorManifest {
    config,
    fragments,
    active_fragment_id,
    total_vectors,
    total_deleted,
    next_vector_id,
    node_to_vector,
    vector_to_node,
    vector_locations,
  };
  validate_vector_manifest(&manifest)
    .map_err(|error| SerializeError::InvalidStructure(format!("manifest: {error}")))?;
  Ok(manifest)
}

// ============================================================================
// Write/Read to IO
// ============================================================================

/// Write IVF index to a writer
pub fn write_ivf<W: Write>(index: &IvfIndex, writer: &mut W) -> io::Result<usize> {
  let data = serialize_ivf(index);
  writer.write_all(&data)?;
  Ok(data.len())
}

/// Read IVF index from a reader
pub fn read_ivf<R: Read>(reader: &mut R) -> Result<IvfIndex, SerializeError> {
  let mut buffer = Vec::new();
  reader.read_to_end(&mut buffer)?;
  deserialize_ivf(&buffer)
}

/// Write vector manifest to a writer
pub fn write_manifest<W: Write>(manifest: &VectorManifest, writer: &mut W) -> io::Result<usize> {
  let data = serialize_manifest(manifest);
  writer.write_all(&data)?;
  Ok(data.len())
}

/// Read vector manifest from a reader
pub fn read_manifest<R: Read>(reader: &mut R) -> Result<VectorManifest, SerializeError> {
  let mut buffer = Vec::new();
  reader.read_to_end(&mut buffer)?;
  deserialize_manifest(&buffer)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vector::{create_vector_store, vector_store_insert, IvfConfig, VectorStoreConfig};

  #[test]
  fn test_metric_conversion() {
    assert_eq!(metric_to_u8(DistanceMetric::Cosine), 0);
    assert_eq!(metric_to_u8(DistanceMetric::Euclidean), 1);
    assert_eq!(metric_to_u8(DistanceMetric::DotProduct), 2);

    assert_eq!(
      u8_to_metric(0).expect("expected value"),
      DistanceMetric::Cosine
    );
    assert_eq!(
      u8_to_metric(1).expect("expected value"),
      DistanceMetric::Euclidean
    );
    assert_eq!(
      u8_to_metric(2).expect("expected value"),
      DistanceMetric::DotProduct
    );

    assert!(u8_to_metric(3).is_err());
  }

  #[test]
  fn test_ivf_round_trip_empty() {
    let config = IvfConfig::new(10).with_metric(DistanceMetric::Cosine);
    let index = IvfIndex::new(4, config);

    let serialized = serialize_ivf(&index);
    let deserialized = deserialize_ivf(&serialized).expect("expected value");

    assert_eq!(deserialized.config.n_clusters, 10);
    assert_eq!(deserialized.dimensions, 4);
    assert!(!deserialized.trained);
  }

  #[test]
  fn test_ivf_round_trip_with_data() {
    let config = IvfConfig::new(2).with_metric(DistanceMetric::Euclidean);
    let mut index = IvfIndex::new(4, config);

    // Simulate a trained index with centroids and inverted lists
    index.centroids = vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    index.inverted_lists.insert(0, vec![1, 2, 3]);
    index.inverted_lists.insert(1, vec![4, 5]);
    index.trained = true;

    let serialized = serialize_ivf(&index);
    let deserialized = deserialize_ivf(&serialized).expect("expected value");

    assert_eq!(deserialized.config.n_clusters, 2);
    assert_eq!(deserialized.config.metric, DistanceMetric::Euclidean);
    assert_eq!(deserialized.centroids.len(), 8);
    assert!(deserialized.trained);
    assert_eq!(deserialized.inverted_lists.len(), 2);
    assert_eq!(
      deserialized
        .inverted_lists
        .get(&0)
        .expect("expected value")
        .len(),
      3
    );
  }

  #[test]
  fn test_manifest_round_trip_empty() {
    let config = VectorStoreConfig::new(4)
      .with_metric(DistanceMetric::Cosine)
      .with_normalize(true);
    let manifest = create_vector_store(config);

    let serialized = serialize_manifest(&manifest);
    let deserialized = deserialize_manifest(&serialized).expect("expected value");

    assert_eq!(deserialized.config.dimensions, 4);
    assert_eq!(deserialized.config.metric, DistanceMetric::Cosine);
    assert!(deserialized.config.normalize_on_insert);
  }

  #[test]
  fn test_manifest_round_trip_with_data() {
    let config = VectorStoreConfig::new(4)
      .with_row_group_size(10)
      .with_normalize(false);
    let mut manifest = create_vector_store(config);

    // Insert some vectors
    for i in 0..5 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).expect("expected value");
    }

    let serialized = serialize_manifest(&manifest);
    let deserialized = deserialize_manifest(&serialized).expect("expected value");

    assert_eq!(deserialized.config.dimensions, 4);
    assert_eq!(deserialized.total_vectors, 5);
    assert_eq!(deserialized.node_to_vector.len(), 5);
    assert_eq!(deserialized.vector_locations.len(), 5);
  }

  #[test]
  fn test_invalid_magic() {
    // Buffer with wrong magic but full header size
    let mut buffer = vec![0u8; IVF_HEADER_SIZE];
    buffer[0..4].copy_from_slice(&0x00000000u32.to_le_bytes()); // Wrong magic
    let result = deserialize_ivf(&buffer);
    assert!(matches!(result, Err(SerializeError::InvalidMagic { .. })));
  }

  #[test]
  fn test_buffer_underflow() {
    let buffer = vec![]; // Empty buffer
    let result = deserialize_ivf(&buffer);
    assert!(matches!(
      result,
      Err(SerializeError::BufferUnderflow { .. })
    ));
  }

  #[test]
  #[allow(clippy::type_complexity)]
  fn test_ivf_corruption_matrix_returns_errors_without_panicking() {
    let config = IvfConfig::new(2);
    let mut index = IvfIndex::new(4, config);
    index.centroids = vec![1.0; 8];
    index.inverted_lists.insert(0, vec![1]);
    index.inverted_lists.insert(1, vec![2]);
    index.trained = true;
    let valid = serialize_ivf(&index);
    let lists_offset = IVF_HEADER_SIZE + 4 + index.centroids.len() * 4 + 4;
    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 3] = [
      (
        "zero n_probe",
        Box::new(|bytes| bytes[12..16].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "zero clusters",
        Box::new(|bytes| bytes[4..8].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "centroid count mismatch",
        Box::new(|bytes| bytes[32..36].copy_from_slice(&4u32.to_le_bytes())),
      ),
    ];

    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deserialize_ivf(&corrupted)));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }

    for (name, field_offset) in [
      ("IVF centroid count", 32usize),
      ("inverted list count", lists_offset - 4),
      ("inverted list length", lists_offset + 4),
    ] {
      let mut corrupted = valid.clone();
      corrupted[field_offset..field_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
      let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deserialize_ivf(&corrupted)));
      assert!(result.is_ok(), "{name} u32::MAX panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "{name} u32::MAX accepted"
      );
    }

    let mut duplicate_cluster = valid.clone();
    let first_cluster = u32::from_le_bytes(
      valid[lists_offset..lists_offset + 4]
        .try_into()
        .expect("cluster"),
    );
    duplicate_cluster[lists_offset + 16..lists_offset + 20]
      .copy_from_slice(&first_cluster.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf(&duplicate_cluster)
    }));
    assert!(result.is_ok(), "duplicate cluster panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "duplicate cluster accepted"
    );

    let mut duplicate_id = valid.clone();
    let first_id = u64::from_le_bytes(
      valid[lists_offset + 8..lists_offset + 16]
        .try_into()
        .expect("id"),
    );
    duplicate_id[lists_offset + 24..lists_offset + 32].copy_from_slice(&first_id.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_ivf(&duplicate_id)
    }));
    assert!(result.is_ok(), "duplicate vector ID panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "duplicate vector ID accepted"
    );

    for length in 0..valid.len() {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_ivf(&valid[..length])
      }));
      assert!(result.is_ok(), "truncation at {length} panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "truncation at {length} accepted"
      );
    }

    let mut trailing = valid.clone();
    trailing.push(0);
    let result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| deserialize_ivf(&trailing)));
    assert!(result.is_ok(), "trailing bytes panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "trailing bytes accepted"
    );
  }

  #[test]
  fn test_ivf_serialized_size() {
    let config = IvfConfig::new(2);
    let mut index = IvfIndex::new(4, config);
    index.centroids = vec![1.0; 8]; // 2 clusters * 4 dimensions
    index.inverted_lists.insert(0, vec![1, 2]);
    index.inverted_lists.insert(1, vec![3]);

    let size = ivf_serialized_size(&index);
    let serialized = serialize_ivf(&index);

    assert_eq!(size, serialized.len());
  }

  #[test]
  fn test_manifest_serialized_size() {
    let config = VectorStoreConfig::new(4).with_normalize(false);
    let mut manifest = create_vector_store(config);

    for i in 0..3 {
      let vector = vec![1.0 + i as f32, 2.0, 3.0, 4.0];
      vector_store_insert(&mut manifest, i, &vector).expect("expected value");
    }

    let size = manifest_serialized_size(&manifest);
    let serialized = serialize_manifest(&manifest);

    // Debug: print sizes if assertion fails
    if size != serialized.len() {
      eprintln!("Calculated size: {size}");
      eprintln!("Actual size: {}", serialized.len());
      eprintln!("MANIFEST_HEADER_SIZE: {MANIFEST_HEADER_SIZE}");
      eprintln!("Fragments: {}", manifest.fragments.len());
      eprintln!("node_to_vector len: {}", manifest.node_to_vector.len());
      eprintln!("vector_locations len: {}", manifest.vector_locations.len());
    }

    assert_eq!(size, serialized.len());
  }

  #[test]
  #[allow(clippy::type_complexity)]
  fn test_manifest_corruption_matrix_returns_errors_without_panicking() {
    let config = VectorStoreConfig::new(4);
    let manifest = create_vector_store(config);
    let valid = serialize_manifest(&manifest);
    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 4] = [
      (
        "zero dimensions",
        Box::new(|bytes| bytes[4..8].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "zero row group size",
        Box::new(|bytes| bytes[12..16].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "zero fragment target size",
        Box::new(|bytes| bytes[16..20].copy_from_slice(&0u32.to_le_bytes())),
      ),
      (
        "active fragment missing",
        Box::new(|bytes| bytes[28..32].copy_from_slice(&1u32.to_le_bytes())),
      ),
    ];

    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_manifest(&corrupted)
      }));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }

    for (name, field_offset) in [
      ("fragment count", 24usize),
      (
        "node-to-vector count",
        MANIFEST_HEADER_SIZE + FRAGMENT_HEADER_SIZE,
      ),
      (
        "vector-to-location count",
        MANIFEST_HEADER_SIZE + FRAGMENT_HEADER_SIZE + 4,
      ),
    ] {
      let mut corrupted = valid.clone();
      corrupted[field_offset..field_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_manifest(&corrupted)
      }));
      assert!(result.is_ok(), "{name} u32::MAX panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "{name} u32::MAX accepted"
      );
    }

    for length in 0..valid.len() {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deserialize_manifest(&valid[..length])
      }));
      assert!(result.is_ok(), "truncation at {length} panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "truncation at {length} accepted"
      );
    }

    let mut trailing = valid.clone();
    trailing.push(0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_manifest(&trailing)
    }));
    assert!(result.is_ok(), "trailing bytes panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "trailing bytes accepted"
    );

    let config = VectorStoreConfig::new(4)
      .with_row_group_size(10)
      .with_fragment_target_size(1);
    let mut manifest = create_vector_store(config);
    vector_store_insert(&mut manifest, 1, &[1.0, 2.0, 3.0, 4.0]).expect("first vector");
    vector_store_insert(&mut manifest, 2, &[2.0, 3.0, 4.0, 5.0]).expect("second vector");
    let valid = serialize_manifest(&manifest);
    let first_fragment_offset = MANIFEST_HEADER_SIZE;
    let first_row_group_offset = first_fragment_offset + FRAGMENT_HEADER_SIZE;
    let second_fragment_offset = first_row_group_offset + ROW_GROUP_HEADER_SIZE + 16 + 4;

    let mut duplicate_fragment_id = valid.clone();
    duplicate_fragment_id[second_fragment_offset..second_fragment_offset + 4]
      .copy_from_slice(&0u32.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_manifest(&duplicate_fragment_id)
    }));
    assert!(result.is_ok(), "duplicate fragment ID panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "duplicate fragment ID accepted"
    );

    let mut row_count_mismatch = valid.clone();
    row_count_mismatch[first_row_group_offset + 4..first_row_group_offset + 8]
      .copy_from_slice(&0u32.to_le_bytes());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      deserialize_manifest(&row_count_mismatch)
    }));
    assert!(result.is_ok(), "row count mismatch panicked");
    assert!(
      result.expect("panic checked").is_err(),
      "row count mismatch accepted"
    );
  }
}
