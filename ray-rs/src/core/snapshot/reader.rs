//! CSR Snapshot Reader - mmap-based snapshot reading
//!
//! Ported from src/core/snapshot-reader.ts
//!
//! Loading resolves every section once: uncompressed sections are ranges of
//! the mmap, compressed ones are inflated into memory. Accessors then index a
//! fixed table by `SectionId`, without locks or reference counts. Before
//! anything is inflated, every declared section size is checked against the
//! header counts and the snapshot size, so a decompression bomb is refused
//! from the section table alone. Then every section's contents are checked,
//! so a corrupt snapshot is refused at open rather than on first use. A
//! large snapshot spreads the CRC, the inflation and the checks over a few
//! threads; which error a corrupt one reports does not depend on that.

use crate::constants::*;
use crate::core::snapshot::node_map::{self, NodeIdMapLayout};
use crate::core::snapshot::parallel;
use crate::core::snapshot::sections::{
  inflation_budget, parse_section_table, string_offset_size_for_version,
};
use crate::error::{KiteError, Result};
use crate::types::*;
use crate::util::binary::*;
use crate::util::compression::{decompress_with_size, CompressionType};
use crate::util::crc::{crc32, crc32_chunked, Crc32Hasher};
use crate::util::hash::xxhash64_string;
use crate::util::mmap::{map_file, Mmap};
use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

// ============================================================================
// Snapshot Data Structure
// ============================================================================

/// Parsed snapshot data with resolved section views
pub struct SnapshotData {
  /// Memory-mapped file data
  mmap: Arc<Mmap>,
  /// Parsed header
  pub header: SnapshotHeaderV1,
  /// Section table
  sections: Vec<SectionEntry>,
  /// Bytes of every section, indexed by `SectionId`, resolved at load
  views: [SectionView; SectionId::COUNT],
  /// NodeIdToPhys encoding (header flag `SPARSE_NODE_ID_MAP`)
  node_id_map: NodeIdMapLayout,
  /// Bytes per StringOffsets entry (u32 before v5, u64 since)
  string_offset_size: usize,
  /// Label names by LabelId, decoded at load
  label_names: Vec<Option<Box<str>>>,
  /// Edge type names by ETypeId, decoded at load
  etype_names: Vec<Option<Box<str>>>,
  /// Property key names by PropKeyId, decoded at load
  propkey_names: Vec<Option<Box<str>>>,
}

/// Where a section's bytes live once the snapshot is loaded.
enum SectionView {
  /// Absent or empty section.
  Empty,
  /// Uncompressed section: `mmap[start..end]`.
  Mapped { start: usize, end: usize },
  /// Compressed section, inflated at load.
  Inflated(Box<[u8]>),
}

/// Borrowed or shared section bytes.
#[derive(Clone)]
pub enum SectionBytes<'a> {
  Borrowed(&'a [u8]),
  Shared(Arc<[u8]>),
}

impl AsRef<[u8]> for SectionBytes<'_> {
  fn as_ref(&self) -> &[u8] {
    match self {
      SectionBytes::Borrowed(bytes) => bytes,
      SectionBytes::Shared(bytes) => bytes.as_ref(),
    }
  }
}

/// Options for parsing a snapshot
#[derive(Debug, Clone, Default)]
pub struct ParseSnapshotOptions {
  /// Skip CRC validation (for performance when reading cached/trusted data)
  pub skip_crc_validation: bool,
  /// Optional CRC chunk size for throughput experiments.
  /// `None` or `Some(0)` uses the default whole-buffer CRC path.
  pub crc_chunk_size: Option<usize>,
  /// Optional sink to capture CRC section attribution (bytes + time).
  pub crc_profile_sink: Option<Arc<Mutex<Option<SnapshotCrcProfile>>>>,
}

/// Per-segment CRC timing attribution.
///
/// `section_id == None` means bytes outside section payloads (header/table/alignment gaps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCrcSectionProfile {
  pub section_id: Option<SectionId>,
  pub bytes: usize,
  pub crc_ns: u64,
}

/// Snapshot CRC profile captured while validating footer checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCrcProfile {
  pub total_bytes: usize,
  pub total_ns: u64,
  pub chunk_size: usize,
  pub sections: Vec<SnapshotCrcSectionProfile>,
}

#[derive(Debug, Clone, Copy)]
struct CrcSegment {
  section_id: Option<SectionId>,
  start: usize,
  end: usize,
}

fn normalized_crc_chunk_size(requested: Option<usize>, total_len: usize) -> usize {
  let Some(size) = requested else {
    return total_len.max(1);
  };
  if size == 0 {
    total_len.max(1)
  } else {
    size
  }
}

fn section_segments(
  sections: &[SectionEntry],
  base_offset: usize,
  crc_len: usize,
) -> Vec<CrcSegment> {
  let mut payload_ranges: Vec<(usize, usize, Option<SectionId>)> = Vec::new();

  for (idx, section) in sections.iter().enumerate() {
    if section.length == 0 {
      continue;
    }

    let global_start = section.offset as usize;
    let Some(local_start) = global_start.checked_sub(base_offset) else {
      continue;
    };
    let local_end = local_start.saturating_add(section.length as usize);
    if local_start >= crc_len {
      continue;
    }

    payload_ranges.push((
      local_start,
      local_end.min(crc_len),
      SectionId::from_u32(idx as u32),
    ));
  }

  payload_ranges.sort_by_key(|(start, _, _)| *start);
  let mut segments = Vec::with_capacity(payload_ranges.len().saturating_mul(2).saturating_add(1));
  let mut cursor = 0usize;

  for (start, end, section_id) in payload_ranges {
    if start > cursor {
      segments.push(CrcSegment {
        section_id: None,
        start: cursor,
        end: start,
      });
    }
    if end > start {
      segments.push(CrcSegment {
        section_id,
        start,
        end,
      });
      cursor = end;
    }
  }

  if cursor < crc_len {
    segments.push(CrcSegment {
      section_id: None,
      start: cursor,
      end: crc_len,
    });
  }

  segments
}

fn compute_crc_with_options(
  data: &[u8],
  options: &ParseSnapshotOptions,
  sections: &[SectionEntry],
  base_offset: usize,
) -> (u32, Option<SnapshotCrcProfile>) {
  let chunk_size = normalized_crc_chunk_size(options.crc_chunk_size, data.len());
  if options.crc_profile_sink.is_none() {
    if chunk_size >= data.len().max(1) {
      return (crc32_spread(data), None);
    }
    return (crc32_chunked(data, chunk_size), None);
  }

  let segments = section_segments(sections, base_offset, data.len());
  let mut hasher = Crc32Hasher::new();
  let mut profile_sections = Vec::with_capacity(segments.len());
  let mut total_ns: u64 = 0;

  for segment in segments {
    let bytes = &data[segment.start..segment.end];
    if bytes.is_empty() {
      continue;
    }

    let segment_start = std::time::Instant::now();
    if chunk_size >= bytes.len() {
      hasher.update(bytes);
    } else {
      for chunk in bytes.chunks(chunk_size) {
        hasher.update(chunk);
      }
    }
    let crc_ns = segment_start.elapsed().as_nanos() as u64;
    total_ns = total_ns.saturating_add(crc_ns);
    profile_sections.push(SnapshotCrcSectionProfile {
      section_id: segment.section_id,
      bytes: bytes.len(),
      crc_ns,
    });
  }

  (
    hasher.finalize(),
    Some(SnapshotCrcProfile {
      total_bytes: data.len(),
      total_ns,
      chunk_size,
      sections: profile_sections,
    }),
  )
}

/// Element `index` of a u32 array, or None past its end: one bounds check.
#[inline]
fn u32_at(data: &[u8], index: usize) -> Option<u32> {
  data
    .as_chunks::<4>()
    .0
    .get(index)
    .copied()
    .map(u32::from_le_bytes)
}

/// Element `index` of a u64 array, or None past its end: one bounds check.
#[inline]
fn u64_at(data: &[u8], index: usize) -> Option<u64> {
  data
    .as_chunks::<8>()
    .0
    .get(index)
    .copied()
    .map(u64::from_le_bytes)
}

/// Elements `start..end` of two u32 arrays side by side (a node's CSR run of
/// edge types and endpoints), cut to what both hold.
#[inline]
fn u32_runs<'a>(
  first: &'a [u8],
  second: &'a [u8],
  (start, end): (usize, usize),
) -> (&'a [[u8; 4]], &'a [[u8; 4]]) {
  let (first, second) = (first.as_chunks::<4>().0, second.as_chunks::<4>().0);
  let end = end.min(first.len()).min(second.len());
  let start = start.min(end);
  (&first[start..end], &second[start..end])
}

/// `offsets[index]..offsets[index + 1]` from a u32 offset array, or None when
/// `index + 1` is past its end or the range is inverted.
#[inline]
fn u32_range_at(offsets: &[u8], index: usize) -> Option<(usize, usize)> {
  let start = u32_at(offsets, index)? as usize;
  let end = u32_at(offsets, index.checked_add(1)?)? as usize;
  (start <= end).then_some((start, end))
}

/// First index in `0..len` for which `before_target` is false, given that it
/// is true for a prefix and false after.
#[inline]
fn partition_point(len: usize, before_target: impl Fn(usize) -> bool) -> usize {
  let (mut lo, mut hi) = (0, len);
  while lo < hi {
    let mid = lo + (hi - lo) / 2;
    if before_target(mid) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  lo
}

/// Hash of KeyEntries entry `index`.
#[inline]
fn key_entry_hash(entries: &[u8], index: usize) -> u64 {
  read_u64(entries, index * KEY_INDEX_ENTRY_SIZE)
}

/// Whether a nonzero string ID indexes the string table. ID 0 is the
/// reserved empty string and is always valid.
#[inline]
fn string_id_in_table(string_id: u64, num_strings: usize) -> bool {
  string_id == 0 || string_id < num_strings as u64
}

// ============================================================================
// Load: steps, limits and the fast checks
// ============================================================================

/// Inflated bytes below which a load stays on one thread, and the least
/// each extra thread gets: inflating 256 KiB takes 150-250 us, starting a
/// thread 20-50 us.
const LOAD_WORK_PER_THREAD: usize = 256 * 1024;

/// The content checks scan bytes about this many times faster than zstd
/// inflates them, so an uncompressed section counts for this much less.
const SCAN_WORK_DIVISOR: usize = 8;

/// Bytes the CRC, or a check across sections, scans per extra thread. Both
/// run at several GB/s.
const SCAN_BYTES_PER_THREAD: usize = 1 << 20;

/// A step of loading the sections, in the order a single-threaded load runs
/// them: every section is inflated (in section order) before any content is
/// checked. When several steps fail, load reports the first, so its error
/// does not depend on how the work was split across threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LoadStep {
  /// Inflating compressed section `index`.
  Inflate(usize),
  NodeIdMaps,
  OutOffsets,
  OutDst,
  InOffsets,
  InSrc,
  InOutIndex,
  StringOffsets,
  LabelStringIds,
  EtypeStringIds,
  PropkeyStringIds,
  NodeKeyString,
  KeyEntries,
  /// KeyBuckets, or the hash order of bucketless KeyEntries.
  KeyLookup,
  VectorOffsets,
  NodePropVals,
  NodePropOffsets,
  EdgePropVals,
  EdgePropOffsets,
  NodeLabelOffsets,
  VectorStoreIndex,
}

struct LoadFailure {
  step: LoadStep,
  error: KiteError,
}

impl LoadFailure {
  fn new(step: LoadStep, error: KiteError) -> Self {
    Self { step, error }
  }
}

/// Header counts and declared section sizes the content checks compare
/// against.
struct ContentLimits {
  num_nodes: usize,
  num_edges: usize,
  num_strings: usize,
  has_in_edges: bool,
  has_node_labels: bool,
  has_vector_stores: bool,
  has_key_buckets: bool,
  string_offset_size: usize,
  string_bytes_len: usize,
  key_entry_count: usize,
  /// Vectors in VectorOffsets, if the snapshot has vectors.
  vector_count: Option<usize>,
  vector_data_len: usize,
  vector_store_data_len: usize,
  node_prop_count: usize,
  edge_prop_count: usize,
  node_label_count: usize,
}

#[cfg(test)]
thread_local! {
  /// Makes this thread's validators skip their fast pass, so tests can
  /// compare it with the precise one.
  static PRECISE_CHECKS_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with this thread's validators using their precise pass only.
#[cfg(test)]
fn with_precise_checks_only<R>(f: impl FnOnce() -> R) -> R {
  let previous = PRECISE_CHECKS_ONLY.with(|precise| precise.replace(true));
  let result = f();
  PRECISE_CHECKS_ONLY.with(|precise| precise.set(previous));
  result
}

/// Whether a validator may accept data on its fast pass alone.
#[inline]
fn fast_checks() -> bool {
  #[cfg(test)]
  {
    !PRECISE_CHECKS_ONLY.with(std::cell::Cell::get)
  }
  #[cfg(not(test))]
  {
    true
  }
}

/// CRC-32 of `data`; a large buffer is split into chunks hashed on several
/// threads and combined.
fn crc32_spread(data: &[u8]) -> u32 {
  let threads = parallel::threads_for(data.len(), SCAN_BYTES_PER_THREAD, usize::MAX);
  if threads <= 1 {
    return crc32(data);
  }
  let chunks: Vec<&[u8]> = data.chunks(data.len().div_ceil(threads)).collect();
  let hashers = parallel::run(chunks.len(), threads, |index| {
    let mut hasher = Crc32Hasher::new();
    hasher.update(chunks[index]);
    hasher
  });
  let mut hashers = hashers.into_iter();
  let mut combined = hashers.next().unwrap_or_default();
  for hasher in hashers {
    combined.combine(&hasher);
  }
  combined.finalize()
}

/// Largest value of a little-endian u32 array, or None when it is empty.
/// A trailing partial value is ignored, as the element reads ignore it.
#[inline]
fn u32_max(data: &[u8]) -> Option<u32> {
  let (values, _) = data.as_chunks::<4>();
  let max = values
    .iter()
    .fold(0, |max, value| max.max(u32::from_le_bytes(*value)));
  (!values.is_empty()).then_some(max)
}

/// Whether a u32 offset array is whole, never decreases and ends at most at
/// `end_limit`.
fn u32_offsets_valid(data: &[u8], end_limit: usize) -> bool {
  let (values, rest) = data.as_chunks::<4>();
  let Some(last) = values.last() else {
    return rest.is_empty();
  };
  let decreasing = values
    .iter()
    .zip(&values[1..])
    .fold(false, |decreasing, (previous, value)| {
      decreasing | (u32::from_le_bytes(*value) < u32::from_le_bytes(*previous))
    });
  rest.is_empty() && !decreasing && u32::from_le_bytes(*last) as usize <= end_limit
}

/// Whether a u64 offset array is whole, never decreases and ends at most at
/// `end_limit`.
fn u64_offsets_valid(data: &[u8], end_limit: usize) -> bool {
  let (values, rest) = data.as_chunks::<8>();
  let Some(last) = values.last() else {
    return rest.is_empty();
  };
  let decreasing = values
    .iter()
    .zip(&values[1..])
    .fold(false, |decreasing, (previous, value)| {
      decreasing | (u64::from_le_bytes(*value) < u64::from_le_bytes(*previous))
    });
  rest.is_empty()
    && !decreasing
    && usize::try_from(u64::from_le_bytes(*last)).is_ok_and(|last| last <= end_limit)
}

/// Whether every property value has a known tag, and every string and
/// vector value indexes its table.
fn property_values_valid(data: &[u8], num_strings: usize, vector_count: Option<usize>) -> bool {
  const STRING: u8 = PropValueTag::String as u8;
  const VECTOR: u8 = PropValueTag::VectorF32 as u8;
  // ID 0 is always valid, so every string ID must be below max(num_strings, 1).
  let string_limit = (num_strings as u64).max(1);
  // Without a vector section no vector index is valid.
  let vector_limit = vector_count.map_or(0, |count| count as u64);
  let (values, _) = data.as_chunks::<PROP_VALUE_DISK_SIZE>();
  let invalid = values.iter().fold(false, |invalid, value| {
    let [tag, _, _, _, _, _, _, _, payload @ ..] = *value;
    let payload = u64::from_le_bytes(payload);
    invalid
      | (tag > VECTOR)
      | ((tag == STRING) & (payload >= string_limit))
      | ((tag == VECTOR) & (payload >= vector_limit))
  });
  !invalid
}

/// Whether NodeIdToPhys and PhysToNodeId are inverse bijections between the
/// node IDs and `0..num_nodes`, in ascending node ID order: the k-th mapped
/// ID is at physical node k, and PhysToNodeId maps it back.
fn node_id_maps_valid(
  layout: NodeIdMapLayout,
  map: &[u8],
  phys_to_node: &[u8],
  num_nodes: usize,
  max_node_id: NodeId,
) -> bool {
  let (node_ids, _) = phys_to_node.as_chunks::<8>();
  let node_ids = &node_ids[..num_nodes.min(node_ids.len())];
  match layout {
    NodeIdMapLayout::Dense => {
      let (slots, _) = map.as_chunks::<{ node_map::DENSE_ENTRY_SIZE }>();
      let mut mapped = 0usize;
      for (node_id, slot) in slots.iter().enumerate() {
        let phys = i32::from_le_bytes(*slot);
        if phys == -1 {
          continue;
        }
        let in_order = usize::try_from(phys).is_ok_and(|phys| phys == mapped);
        match node_ids.get(mapped) {
          Some(stored) if in_order && u64::from_le_bytes(*stored) == node_id as NodeId => {
            mapped += 1;
          }
          _ => return false,
        }
      }
      mapped == num_nodes
    }
    NodeIdMapLayout::Sparse => {
      let (entries, _) = map.as_chunks::<{ node_map::SPARSE_ENTRY_SIZE }>();
      let mut previous = None;
      for (index, entry) in entries.iter().enumerate() {
        let [id @ .., p0, p1, p2, p3] = *entry;
        let node_id = u64::from_le_bytes(id);
        let phys = u32::from_le_bytes([p0, p1, p2, p3]);
        let ascending = previous.is_none_or(|previous| previous < node_id);
        match node_ids.get(index) {
          Some(stored)
            if ascending
              && node_id <= max_node_id
              && phys as usize == index
              && u64::from_le_bytes(*stored) == node_id => {}
          _ => return false,
        }
        previous = Some(node_id);
      }
      entries.len() == num_nodes
    }
  }
}

/// Whether every KeyEntries record names a string in the table and a node
/// NodeIdToPhys maps.
fn key_entries_valid(
  entries: &[u8],
  num_strings: usize,
  layout: NodeIdMapLayout,
  map: &[u8],
) -> bool {
  // ID 0 is always valid, so every string ID must be below max(num_strings, 1).
  let string_limit = (num_strings as u64).max(1);
  let (records, _) = entries.as_chunks::<KEY_INDEX_ENTRY_SIZE>();
  let fields = |record: &[u8; KEY_INDEX_ENTRY_SIZE]| {
    let [_, _, _, _, _, _, _, _, s0, s1, s2, s3, _, _, _, _, node_id @ ..] = *record;
    (
      u64::from(u32::from_le_bytes([s0, s1, s2, s3])),
      u64::from_le_bytes(node_id),
    )
  };
  match layout {
    NodeIdMapLayout::Dense => {
      let (slots, _) = map.as_chunks::<{ node_map::DENSE_ENTRY_SIZE }>();
      records.iter().all(|record| {
        let (string_id, node_id) = fields(record);
        string_id < string_limit
          && usize::try_from(node_id)
            .ok()
            .and_then(|node_id| slots.get(node_id))
            .is_some_and(|slot| i32::from_le_bytes(*slot) >= 0)
      })
    }
    NodeIdMapLayout::Sparse => records.iter().all(|record| {
      let (string_id, node_id) = fields(record);
      string_id < string_limit && node_map::sparse_lookup(map, node_id).is_some()
    }),
  }
}

impl SnapshotData {
  fn from_parsed_parts(
    mmap: Arc<Mmap>,
    header: SnapshotHeaderV1,
    sections: Vec<SectionEntry>,
    snapshot_len: usize,
  ) -> Result<Self> {
    let node_id_map = if header.flags.contains(SnapshotFlags::SPARSE_NODE_ID_MAP) {
      NodeIdMapLayout::Sparse
    } else {
      NodeIdMapLayout::Dense
    };
    let string_offset_size = string_offset_size_for_version(header.version);
    let mut snapshot = Self {
      mmap,
      header,
      sections,
      views: std::array::from_fn(|_| SectionView::Empty),
      node_id_map,
      string_offset_size,
      label_names: Vec::new(),
      etype_names: Vec::new(),
      propkey_names: Vec::new(),
    };

    snapshot.validate_section_sizes(snapshot_len)?;
    snapshot.load_sections()?;
    snapshot.label_names = snapshot.decode_names(SectionId::LabelStringIds);
    snapshot.etype_names = snapshot.decode_names(SectionId::EtypeStringIds);
    snapshot.propkey_names = snapshot.decode_names(SectionId::PropkeyStringIds);
    Ok(snapshot)
  }

  /// Load and mmap a snapshot file
  pub fn load(path: impl AsRef<Path>) -> Result<Self> {
    let file = File::open(path.as_ref())?;
    let mmap = map_file(&file)?;
    Self::parse(Arc::new(mmap), &ParseSnapshotOptions::default())
  }

  /// Load with options
  pub fn load_with_options(path: impl AsRef<Path>, options: &ParseSnapshotOptions) -> Result<Self> {
    let file = File::open(path.as_ref())?;
    let mmap = map_file(&file)?;
    Self::parse(Arc::new(mmap), options)
  }

  /// Parse snapshot from mmap buffer
  pub fn parse(mmap: Arc<Mmap>, options: &ParseSnapshotOptions) -> Result<Self> {
    Self::parse_at_offset(mmap, 0, options)
  }

  /// Parse snapshot from mmap buffer at a specific byte offset
  /// Used for single-file format where snapshot is embedded after header+WAL
  pub fn parse_at_offset(
    mmap: Arc<Mmap>,
    offset: usize,
    options: &ParseSnapshotOptions,
  ) -> Result<Self> {
    if offset > mmap.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot offset {offset} exceeds mmap length {}",
        mmap.len()
      )));
    }
    let buffer = &mmap[offset..];

    if buffer.len() < SNAPSHOT_HEADER_SIZE {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot too small: {} bytes",
        buffer.len()
      )));
    }

    // Parse header
    let magic = read_u32(buffer, 0);
    if magic != MAGIC_SNAPSHOT {
      return Err(KiteError::InvalidMagic {
        expected: MAGIC_SNAPSHOT,
        got: magic,
      });
    }

    let version = read_u32(buffer, 4);
    let min_reader_version = read_u32(buffer, 8);

    if MIN_READER_SNAPSHOT < min_reader_version {
      return Err(KiteError::VersionMismatch {
        required: min_reader_version,
        current: MIN_READER_SNAPSHOT,
      });
    }

    let header = SnapshotHeaderV1 {
      magic,
      version,
      min_reader_version,
      flags: SnapshotFlags::from_bits_truncate(read_u32(buffer, 12)),
      generation: read_u64(buffer, 16),
      created_unix_ns: read_u64(buffer, 24),
      num_nodes: read_u64(buffer, 32),
      num_edges: read_u64(buffer, 40),
      max_node_id: read_u64(buffer, 48),
      num_labels: read_u64(buffer, 56),
      num_etypes: read_u64(buffer, 64),
      num_propkeys: read_u64(buffer, 72),
      num_strings: read_u64(buffer, 80),
    };

    let parsed = parse_section_table(buffer, version, offset)?;
    let sections = parsed.sections;
    let aligned_end = parsed
      .max_section_end
      .checked_add(SECTION_ALIGNMENT - 1)
      .map(|value| value & !(SECTION_ALIGNMENT - 1))
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot size alignment overflow".to_string()))?;
    let actual_snapshot_size = aligned_end
      .checked_add(4)
      .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot CRC offset overflow".to_string()))?;

    if actual_snapshot_size > buffer.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Snapshot truncated: expected {actual_snapshot_size} bytes, found {}",
        buffer.len()
      )));
    }

    // Verify footer CRC (optional)
    if !options.skip_crc_validation {
      let footer_crc = read_u32(buffer, actual_snapshot_size - 4);
      let (computed_crc, crc_profile) = compute_crc_with_options(
        &buffer[..actual_snapshot_size - 4],
        options,
        &sections,
        offset,
      );
      if let Some(sink) = options.crc_profile_sink.as_ref() {
        if let Ok(mut guard) = sink.lock() {
          *guard = crc_profile;
        }
      }
      if footer_crc != computed_crc {
        return Err(KiteError::CrcMismatch {
          stored: footer_crc,
          computed: computed_crc,
        });
      }
    }

    Self::from_parsed_parts(mmap, header, sections, actual_snapshot_size)
  }

  fn section_name(id: SectionId) -> &'static str {
    match id {
      SectionId::PhysToNodeId => "PhysToNodeId",
      SectionId::NodeIdToPhys => "NodeIdToPhys",
      SectionId::OutOffsets => "OutOffsets",
      SectionId::OutDst => "OutDst",
      SectionId::OutEtype => "OutEtype",
      SectionId::InOffsets => "InOffsets",
      SectionId::InSrc => "InSrc",
      SectionId::InEtype => "InEtype",
      SectionId::InOutIndex => "InOutIndex",
      SectionId::StringOffsets => "StringOffsets",
      SectionId::StringBytes => "StringBytes",
      SectionId::LabelStringIds => "LabelStringIds",
      SectionId::EtypeStringIds => "EtypeStringIds",
      SectionId::PropkeyStringIds => "PropkeyStringIds",
      SectionId::NodeKeyString => "NodeKeyString",
      SectionId::KeyEntries => "KeyEntries",
      SectionId::KeyBuckets => "KeyBuckets",
      SectionId::NodePropOffsets => "NodePropOffsets",
      SectionId::NodePropKeys => "NodePropKeys",
      SectionId::NodePropVals => "NodePropVals",
      SectionId::EdgePropOffsets => "EdgePropOffsets",
      SectionId::EdgePropKeys => "EdgePropKeys",
      SectionId::EdgePropVals => "EdgePropVals",
      SectionId::NodeLabelOffsets => "NodeLabelOffsets",
      SectionId::NodeLabelIds => "NodeLabelIds",
      SectionId::VectorOffsets => "VectorOffsets",
      SectionId::VectorData => "VectorData",
      SectionId::VectorStoreIndex => "VectorStoreIndex",
      SectionId::VectorStoreData => "VectorStoreData",
    }
  }

  fn invalid_section(section: &str, message: impl Into<String>) -> KiteError {
    KiteError::InvalidSnapshot(format!("{section} section: {}", message.into()))
  }

  fn checked_count(value: u64, section: &str) -> Result<usize> {
    usize::try_from(value)
      .map_err(|_| Self::invalid_section(section, "count does not fit in usize"))
  }

  fn checked_count_plus_one(value: u64, section: &str) -> Result<usize> {
    Self::checked_count(value, section)?
      .checked_add(1)
      .ok_or_else(|| Self::invalid_section(section, "count plus one overflows"))
  }

  fn checked_bytes(count: usize, element_size: usize, section: &str) -> Result<usize> {
    count
      .checked_mul(element_size)
      .ok_or_else(|| Self::invalid_section(section, "array size overflows"))
  }

  // ========================================================================
  // Load: sizes from the section table, then sections, then contents
  // ========================================================================

  /// Bytes section `id` holds once resolved: its declared uncompressed size
  /// when compressed, its on-disk length otherwise, 0 when absent.
  fn declared_len(&self, id: SectionId) -> usize {
    match self.sections.get(id as usize) {
      // parse_section_table checked both sizes fit usize.
      Some(entry) if entry.compression != 0 => entry.uncompressed_size as usize,
      Some(entry) => entry.length as usize,
      None => 0,
    }
  }

  fn expect_len(&self, id: SectionId, expected: usize) -> Result<()> {
    let actual = self.declared_len(id);
    if actual == expected {
      return Ok(());
    }
    let name = Self::section_name(id);
    if actual == 0 {
      return Err(Self::invalid_section(
        name,
        format!("section is missing (expected {expected} bytes)"),
      ));
    }
    Err(Self::invalid_section(
      name,
      format!("expected {expected} bytes, found {actual}"),
    ))
  }

  fn expect_multiple(&self, id: SectionId, element_size: usize) -> Result<usize> {
    let len = self.declared_len(id);
    if !len.is_multiple_of(element_size) {
      return Err(Self::invalid_section(
        Self::section_name(id),
        format!("{len} bytes is not a whole number of {element_size}-byte entries"),
      ));
    }
    Ok(len / element_size)
  }

  fn expect_present(&self, id: SectionId) -> Result<()> {
    if self.declared_len(id) == 0 {
      return Err(Self::invalid_section(
        Self::section_name(id),
        "section is missing",
      ));
    }
    Ok(())
  }

  /// Checks every section's declared size against the header counts, the
  /// other declared sizes and the snapshot size. Reads the section table
  /// only, so it runs before anything is inflated.
  fn validate_section_sizes(&self, snapshot_len: usize) -> Result<()> {
    let mut inflated = 0usize;
    for entry in self.sections.iter().filter(|entry| entry.compression != 0) {
      inflated = inflated.saturating_add(entry.uncompressed_size as usize);
    }
    let budget = inflation_budget(snapshot_len);
    if inflated > budget {
      return Err(KiteError::InvalidSnapshot(format!(
        "compressed sections declare {inflated} bytes, more than the {budget} bytes a \
         {snapshot_len}-byte snapshot may inflate to"
      )));
    }

    let flags = self.header.flags;
    let num_nodes = Self::checked_count(self.header.num_nodes, "node counts")?;
    let num_edges = Self::checked_count(self.header.num_edges, "edge counts")?;
    let node_offsets_len = Self::checked_bytes(
      Self::checked_count_plus_one(self.header.num_nodes, "OutOffsets")?,
      4,
      "OutOffsets",
    )?;
    let edge_array_len = Self::checked_bytes(num_edges, 4, "OutDst")?;

    self.expect_len(
      SectionId::PhysToNodeId,
      Self::checked_bytes(num_nodes, 8, "PhysToNodeId")?,
    )?;
    let node_map_len = match self.node_id_map {
      NodeIdMapLayout::Dense => {
        let slots = usize::try_from(self.header.max_node_id)
          .ok()
          .and_then(|max_node_id| max_node_id.checked_add(1))
          .ok_or_else(|| Self::invalid_section("NodeIdToPhys", "max node ID overflows"))?;
        Self::checked_bytes(slots, node_map::DENSE_ENTRY_SIZE, "NodeIdToPhys")?
      }
      NodeIdMapLayout::Sparse => {
        Self::checked_bytes(num_nodes, node_map::SPARSE_ENTRY_SIZE, "NodeIdToPhys")?
      }
    };
    self.expect_len(SectionId::NodeIdToPhys, node_map_len)?;

    self.expect_len(SectionId::OutOffsets, node_offsets_len)?;
    self.expect_len(SectionId::OutDst, edge_array_len)?;
    self.expect_len(SectionId::OutEtype, edge_array_len)?;
    if flags.contains(SnapshotFlags::HAS_IN_EDGES) {
      self.expect_len(SectionId::InOffsets, node_offsets_len)?;
      self.expect_len(SectionId::InSrc, edge_array_len)?;
      self.expect_len(SectionId::InEtype, edge_array_len)?;
      self.expect_len(SectionId::InOutIndex, edge_array_len)?;
    }

    self.expect_len(
      SectionId::StringOffsets,
      Self::checked_bytes(
        Self::checked_count_plus_one(self.header.num_strings, "StringOffsets")?,
        self.string_offset_size,
        "StringOffsets",
      )?,
    )?;
    for (id, count) in [
      (SectionId::LabelStringIds, self.header.num_labels),
      (SectionId::EtypeStringIds, self.header.num_etypes),
      (SectionId::PropkeyStringIds, self.header.num_propkeys),
    ] {
      let name = Self::section_name(id);
      self.expect_len(
        id,
        Self::checked_bytes(Self::checked_count_plus_one(count, name)?, 4, name)?,
      )?;
    }
    self.expect_len(
      SectionId::NodeKeyString,
      Self::checked_bytes(num_nodes, 4, "NodeKeyString")?,
    )?;

    let key_entry_count = self.expect_multiple(SectionId::KeyEntries, KEY_INDEX_ENTRY_SIZE)?;
    if key_entry_count > num_nodes {
      return Err(Self::invalid_section(
        "KeyEntries",
        format!("entry count {key_entry_count} exceeds node count {num_nodes}"),
      ));
    }
    if flags.contains(SnapshotFlags::HAS_KEY_BUCKETS) {
      self.expect_present(SectionId::KeyBuckets)?;
    }
    let bucket_offsets = self.expect_multiple(SectionId::KeyBuckets, 4)?;
    // The writer uses max(16, 2 * entries) buckets.
    let max_bucket_offsets = key_entry_count.saturating_mul(2).max(16) + 1;
    if bucket_offsets != 0 && !(2..=max_bucket_offsets).contains(&bucket_offsets) {
      return Err(Self::invalid_section(
        "KeyBuckets",
        format!(
          "{bucket_offsets} bucket offsets for {key_entry_count} entries; expected 2 to \
           {max_bucket_offsets}"
        ),
      ));
    }

    self.expect_len(SectionId::NodePropOffsets, node_offsets_len)?;
    let node_prop_count = self.expect_multiple(SectionId::NodePropKeys, 4)?;
    self.expect_len(
      SectionId::NodePropVals,
      Self::checked_bytes(node_prop_count, PROP_VALUE_DISK_SIZE, "NodePropVals")?,
    )?;
    self.expect_len(
      SectionId::EdgePropOffsets,
      Self::checked_bytes(
        num_edges
          .checked_add(1)
          .ok_or_else(|| Self::invalid_section("EdgePropOffsets", "edge count overflows"))?,
        4,
        "EdgePropOffsets",
      )?,
    )?;
    let edge_prop_count = self.expect_multiple(SectionId::EdgePropKeys, 4)?;
    self.expect_len(
      SectionId::EdgePropVals,
      Self::checked_bytes(edge_prop_count, PROP_VALUE_DISK_SIZE, "EdgePropVals")?,
    )?;

    if flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      self.expect_len(SectionId::NodeLabelOffsets, node_offsets_len)?;
      self.expect_multiple(SectionId::NodeLabelIds, 4)?;
    }

    if flags.contains(SnapshotFlags::HAS_VECTORS) {
      self.expect_present(SectionId::VectorOffsets)?;
      self.expect_present(SectionId::VectorData)?;
      let vector_offsets = self.expect_multiple(SectionId::VectorOffsets, 8)?;
      self.expect_multiple(SectionId::VectorData, 4)?;
      // Each vector is the value of one node or edge property.
      let max_vectors = node_prop_count.saturating_add(edge_prop_count);
      if vector_offsets < 2 || vector_offsets - 1 > max_vectors {
        return Err(Self::invalid_section(
          "VectorOffsets",
          format!(
            "{} vectors for {max_vectors} property values",
            vector_offsets.saturating_sub(1)
          ),
        ));
      }
    }

    if flags.contains(SnapshotFlags::HAS_VECTOR_STORES) {
      self.expect_present(SectionId::VectorStoreIndex)?;
      self.expect_present(SectionId::VectorStoreData)?;
    }

    Ok(())
  }

  /// Work units of loading section `index`: a compressed section counts the
  /// bytes it inflates to, an uncompressed one the bytes its checks scan,
  /// which go about `SCAN_WORK_DIVISOR` times faster.
  fn load_work(&self, index: usize) -> usize {
    let entry = &self.sections[index];
    // parse_section_table checked both sizes fit usize.
    if entry.compression != 0 {
      entry.uncompressed_size as usize
    } else {
      entry.length as usize / SCAN_WORK_DIVISOR
    }
  }

  /// Resolves every section (uncompressed ones are ranges of the mmap,
  /// compressed ones are inflated) and checks their contents against the
  /// header counts and each other.
  ///
  /// A large snapshot spreads its sections over several threads, each
  /// checking a section right after inflating it; the two checks that read
  /// two sections run once all are resolved. However the work is split, a
  /// snapshot that fails several checks reports the first in `LoadStep`
  /// order, the order a single thread runs them in.
  fn load_sections(&mut self) -> Result<()> {
    let limits = self.content_limits()?;
    let mut order: Vec<usize> = (0..self.sections.len())
      .filter(|&index| self.sections[index].length != 0)
      .collect();
    // Largest first, so that no long inflation starts last.
    order.sort_by_key(|&index| Reverse(self.load_work(index)));
    let work = order.iter().map(|&index| self.load_work(index)).sum();
    let threads = parallel::threads_for(work, LOAD_WORK_PER_THREAD, order.len());

    let this = &*self;
    let loaded = parallel::run(order.len(), threads, |task| {
      this.load_section(order[task], &limits)
    });
    let mut failures = Vec::new();
    for (&index, (view, failure)) in order.iter().zip(loaded) {
      self.views[index] = view;
      failures.extend(failure);
    }

    // An inflation failure is reported before any check, and the checks
    // across sections need every section.
    if !failures
      .iter()
      .any(|failure| matches!(failure.step, LoadStep::Inflate(_)))
    {
      let scanned = [
        SectionId::NodeIdToPhys,
        SectionId::PhysToNodeId,
        SectionId::KeyEntries,
      ]
      .map(|id| self.declared_len(id))
      .iter()
      .sum();
      let threads = parallel::threads_for(scanned, SCAN_BYTES_PER_THREAD, 2);
      let this = &*self;
      let cross = parallel::run(2, threads, |task| {
        if task == 0 {
          this
            .validate_node_id_maps(limits.num_nodes)
            .map_err(|error| LoadFailure::new(LoadStep::NodeIdMaps, error))
        } else {
          this
            .validate_key_entries(limits.num_strings)
            .map_err(|error| LoadFailure::new(LoadStep::KeyEntries, error))
        }
      });
      failures.extend(cross.into_iter().filter_map(|result| result.err()));
    }

    match failures.into_iter().min_by_key(|failure| failure.step) {
      Some(failure) => Err(failure.error),
      None => Ok(()),
    }
  }

  /// Resolves section `index` and runs the check its bytes alone decide.
  /// A section that fails to inflate stays empty and is not checked.
  fn load_section(
    &self,
    index: usize,
    limits: &ContentLimits,
  ) -> (SectionView, Option<LoadFailure>) {
    let view = match self.resolve_section(index) {
      Ok(view) => view,
      Err(error) => {
        return (
          SectionView::Empty,
          Some(LoadFailure::new(LoadStep::Inflate(index), error)),
        )
      }
    };
    let failure = SectionId::from_u32(index as u32)
      .and_then(|id| Self::check_section(limits, id, self.view_bytes(&view)));
    (view, failure)
  }

  /// Section `index` as a range of the mmap, or inflated if compressed.
  fn resolve_section(&self, index: usize) -> Result<SectionView> {
    let entry = &self.sections[index];
    if entry.length == 0 {
      return Ok(SectionView::Empty);
    }
    // parse_section_table checked the range lies within the mmap.
    let start = entry.offset as usize;
    let end = start + entry.length as usize;
    match CompressionType::from_u32(entry.compression) {
      Some(CompressionType::None) => Ok(SectionView::Mapped { start, end }),
      compression => {
        let name = SectionId::from_u32(index as u32).map_or("unknown", Self::section_name);
        let compression =
          compression.ok_or_else(|| Self::invalid_section(name, "unknown compression type"))?;
        let inflated = decompress_with_size(
          &self.mmap[start..end],
          compression,
          entry.uncompressed_size as usize,
        )
        .map_err(|error| Self::invalid_section(name, format!("cannot decompress: {error}")))?;
        Ok(SectionView::Inflated(inflated.into_boxed_slice()))
      }
    }
  }

  /// Bytes of a resolved section.
  fn view_bytes<'a>(&'a self, view: &'a SectionView) -> &'a [u8] {
    match view {
      SectionView::Empty => &[],
      SectionView::Mapped { start, end } => &self.mmap[*start..*end],
      SectionView::Inflated(bytes) => bytes,
    }
  }

  /// Bytes of section `id`, or an empty slice when it is absent.
  #[inline]
  fn bytes(&self, id: SectionId) -> &[u8] {
    self.section(id).unwrap_or(&[])
  }

  /// What the content checks compare against. It all comes from the header
  /// and the declared section sizes (a resolved section has exactly its
  /// declared size), so each section can be checked as soon as it resolves.
  fn content_limits(&self) -> Result<ContentLimits> {
    let flags = self.header.flags;
    Ok(ContentLimits {
      num_nodes: Self::checked_count(self.header.num_nodes, "node counts")?,
      num_edges: Self::checked_count(self.header.num_edges, "edge counts")?,
      num_strings: Self::checked_count(self.header.num_strings, "StringOffsets")?,
      has_in_edges: flags.contains(SnapshotFlags::HAS_IN_EDGES),
      has_node_labels: flags.contains(SnapshotFlags::HAS_NODE_LABELS),
      has_vector_stores: flags.contains(SnapshotFlags::HAS_VECTOR_STORES),
      has_key_buckets: self.declared_len(SectionId::KeyBuckets) != 0,
      string_offset_size: self.string_offset_size,
      string_bytes_len: self.declared_len(SectionId::StringBytes),
      key_entry_count: self.declared_len(SectionId::KeyEntries) / KEY_INDEX_ENTRY_SIZE,
      // validate_section_sizes checked VectorOffsets has at least two offsets.
      vector_count: flags
        .contains(SnapshotFlags::HAS_VECTORS)
        .then(|| (self.declared_len(SectionId::VectorOffsets) / 8).saturating_sub(1)),
      vector_data_len: self.declared_len(SectionId::VectorData),
      vector_store_data_len: self.declared_len(SectionId::VectorStoreData),
      node_prop_count: self.declared_len(SectionId::NodePropVals) / PROP_VALUE_DISK_SIZE,
      edge_prop_count: self.declared_len(SectionId::EdgePropVals) / PROP_VALUE_DISK_SIZE,
      node_label_count: self.declared_len(SectionId::NodeLabelIds) / 4,
    })
  }

  /// The check that section `id`'s own bytes decide, if it has one.
  fn check_section(limits: &ContentLimits, id: SectionId, data: &[u8]) -> Option<LoadFailure> {
    let (step, result) = match id {
      SectionId::OutOffsets => (
        LoadStep::OutOffsets,
        Self::validate_u32_offsets(data, limits.num_edges, "OutOffsets"),
      ),
      SectionId::OutDst => (
        LoadStep::OutDst,
        Self::validate_u32_values_below(data, limits.num_nodes, "OutDst"),
      ),
      SectionId::InOffsets if limits.has_in_edges => (
        LoadStep::InOffsets,
        Self::validate_u32_offsets(data, limits.num_edges, "InOffsets"),
      ),
      SectionId::InSrc if limits.has_in_edges => (
        LoadStep::InSrc,
        Self::validate_u32_values_below(data, limits.num_nodes, "InSrc"),
      ),
      SectionId::InOutIndex if limits.has_in_edges => (
        LoadStep::InOutIndex,
        Self::validate_u32_values_below(data, limits.num_edges, "InOutIndex"),
      ),
      SectionId::StringOffsets => (
        LoadStep::StringOffsets,
        if limits.string_offset_size == 8 {
          Self::validate_u64_offsets(data, limits.string_bytes_len, "StringOffsets")
        } else {
          Self::validate_u32_offsets(data, limits.string_bytes_len, "StringOffsets")
        },
      ),
      SectionId::LabelStringIds => (
        LoadStep::LabelStringIds,
        Self::validate_string_id_array(data, limits.num_strings, "LabelStringIds"),
      ),
      SectionId::EtypeStringIds => (
        LoadStep::EtypeStringIds,
        Self::validate_string_id_array(data, limits.num_strings, "EtypeStringIds"),
      ),
      SectionId::PropkeyStringIds => (
        LoadStep::PropkeyStringIds,
        Self::validate_string_id_array(data, limits.num_strings, "PropkeyStringIds"),
      ),
      SectionId::NodeKeyString => (
        LoadStep::NodeKeyString,
        Self::validate_string_id_array(data, limits.num_strings, "NodeKeyString"),
      ),
      SectionId::KeyEntries if !limits.has_key_buckets => {
        (LoadStep::KeyLookup, Self::validate_key_order(data))
      }
      SectionId::KeyBuckets => (
        LoadStep::KeyLookup,
        Self::validate_key_buckets(data, limits.key_entry_count),
      ),
      SectionId::VectorOffsets if limits.vector_count.is_some() => (
        LoadStep::VectorOffsets,
        Self::validate_u64_offsets(data, limits.vector_data_len, "VectorOffsets"),
      ),
      SectionId::NodePropVals => (
        LoadStep::NodePropVals,
        Self::validate_property_values(
          data,
          limits.num_strings,
          limits.vector_count,
          "NodePropVals",
        ),
      ),
      SectionId::NodePropOffsets => (
        LoadStep::NodePropOffsets,
        Self::validate_u32_offsets(data, limits.node_prop_count, "NodePropOffsets"),
      ),
      SectionId::EdgePropVals => (
        LoadStep::EdgePropVals,
        Self::validate_property_values(
          data,
          limits.num_strings,
          limits.vector_count,
          "EdgePropVals",
        ),
      ),
      SectionId::EdgePropOffsets => (
        LoadStep::EdgePropOffsets,
        Self::validate_u32_offsets(data, limits.edge_prop_count, "EdgePropOffsets"),
      ),
      SectionId::NodeLabelOffsets if limits.has_node_labels => (
        LoadStep::NodeLabelOffsets,
        Self::validate_u32_offsets(data, limits.node_label_count, "NodeLabelOffsets"),
      ),
      SectionId::VectorStoreIndex if limits.has_vector_stores => (
        LoadStep::VectorStoreIndex,
        Self::validate_vector_store_index(data, limits.vector_store_data_len),
      ),
      _ => return None,
    };
    result.err().map(|error| LoadFailure::new(step, error))
  }

  // Each validator first runs a fast pass that only answers whether the data
  // is valid: branch-free over arrays, so the compiler vectorizes it. Only
  // when that fails does the precise pass run, to name the first bad entry.

  fn validate_u32_offsets(data: &[u8], end_limit: usize, section: &str) -> Result<()> {
    if fast_checks() && u32_offsets_valid(data, end_limit) {
      return Ok(());
    }
    if !data.len().is_multiple_of(4) {
      return Err(Self::invalid_section(
        section,
        "offset array is not a multiple of 4 bytes",
      ));
    }

    let mut previous = 0usize;
    for index in 0..data.len() / 4 {
      let value = read_u32_at(data, index) as usize;
      if value < previous {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} is not monotonic: {value} < {previous}"),
        ));
      }
      if value > end_limit {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} ({value}) exceeds end {end_limit}"),
        ));
      }
      previous = value;
    }
    Ok(())
  }

  fn validate_u64_offsets(data: &[u8], end_limit: usize, section: &str) -> Result<()> {
    if fast_checks() && u64_offsets_valid(data, end_limit) {
      return Ok(());
    }
    if !data.len().is_multiple_of(8) {
      return Err(Self::invalid_section(
        section,
        "offset array is not a multiple of 8 bytes",
      ));
    }

    let mut previous = 0usize;
    for index in 0..data.len() / 8 {
      let value = usize::try_from(read_u64_at(data, index)).map_err(|_| {
        Self::invalid_section(section, format!("offset {index} does not fit in usize"))
      })?;
      if value < previous {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} is not monotonic: {value} < {previous}"),
        ));
      }
      if value > end_limit {
        return Err(Self::invalid_section(
          section,
          format!("offset {index} ({value}) exceeds end {end_limit}"),
        ));
      }
      previous = value;
    }
    Ok(())
  }

  fn validate_string_id_array(data: &[u8], num_strings: usize, section: &str) -> Result<()> {
    // ID 0 is always valid, so every ID must be below max(num_strings, 1).
    if fast_checks() && u32_max(data).is_none_or(|max| u64::from(max) < (num_strings as u64).max(1))
    {
      return Ok(());
    }
    for index in 0..data.len() / 4 {
      let string_id = read_u32_at(data, index);
      if !string_id_in_table(u64::from(string_id), num_strings) {
        return Err(Self::invalid_section(
          section,
          format!("string ID {string_id} at index {index} is outside 0..{num_strings}"),
        ));
      }
    }
    Ok(())
  }

  fn validate_u32_values_below(data: &[u8], limit: usize, section: &str) -> Result<()> {
    if fast_checks() && u32_max(data).is_none_or(|max| (max as usize) < limit) {
      return Ok(());
    }
    for index in 0..data.len() / 4 {
      let value = read_u32_at(data, index) as usize;
      if value >= limit {
        return Err(Self::invalid_section(
          section,
          format!("value {value} at index {index} is outside 0..{limit}"),
        ));
      }
    }
    Ok(())
  }

  fn validate_property_values(
    data: &[u8],
    num_strings: usize,
    vector_count: Option<usize>,
    section: &str,
  ) -> Result<()> {
    if fast_checks() && property_values_valid(data, num_strings, vector_count) {
      return Ok(());
    }
    for index in 0..data.len() / PROP_VALUE_DISK_SIZE {
      let offset = index * PROP_VALUE_DISK_SIZE;
      let tag = data[offset];
      let payload = read_u64(data, offset + 8);
      match PropValueTag::from_u8(tag) {
        Some(PropValueTag::String) => {
          if !string_id_in_table(payload, num_strings) {
            return Err(Self::invalid_section(
              section,
              format!("string ID {payload} at entry {index} is outside 0..{num_strings}"),
            ));
          }
        }
        Some(PropValueTag::VectorF32) => {
          let Some(vector_count) = vector_count else {
            return Err(Self::invalid_section(
              section,
              format!("vector value at entry {index} has no vector section"),
            ));
          };
          if payload >= vector_count as u64 {
            return Err(Self::invalid_section(
              section,
              format!("vector index {payload} at entry {index} is outside 0..{vector_count}"),
            ));
          }
        }
        Some(_) => {}
        None => {
          return Err(Self::invalid_section(
            section,
            format!("invalid property tag {tag} at entry {index}"),
          ));
        }
      }
    }
    Ok(())
  }

  /// NodeIdToPhys and PhysToNodeId must be inverse bijections between the
  /// node IDs and `0..num_nodes`. Every mapped `(node_id, phys)` must have
  /// `PhysToNodeId[phys] == node_id`; distinct IDs then map to distinct
  /// physical nodes, and exactly `num_nodes` mapped IDs cover them all.
  /// Physical order must also be node ID order (the writer sorts nodes by
  /// ID): reads that seek, like pages and edge slices, binary-search physical
  /// indexes by node ID. Both maps are walked in ascending node ID, so that
  /// holds when the k-th mapped ID is at physical node k.
  fn validate_node_id_maps(&self, num_nodes: usize) -> Result<()> {
    const SECTION: &str = "NodeIdToPhys";
    let map = self.bytes(SectionId::NodeIdToPhys);
    let phys_to_node = self.bytes(SectionId::PhysToNodeId);
    if fast_checks()
      && node_id_maps_valid(
        self.node_id_map,
        map,
        phys_to_node,
        num_nodes,
        self.header.max_node_id,
      )
    {
      return Ok(());
    }
    // `rank`: how many node IDs below `node_id` are mapped.
    let check_pair = |node_id: NodeId, phys: usize, rank: usize| -> Result<()> {
      if phys >= num_nodes {
        return Err(Self::invalid_section(
          SECTION,
          format!("physical node {phys} for node ID {node_id} is outside the node count"),
        ));
      }
      let stored = read_u64_at(phys_to_node, phys);
      if stored != node_id {
        return Err(Self::invalid_section(
          "PhysToNodeId",
          format!(
            "physical node {phys} has node ID {stored}, but NodeIdToPhys maps node ID \
             {node_id} to it"
          ),
        ));
      }
      if phys != rank {
        return Err(Self::invalid_section(
          "PhysToNodeId",
          format!(
            "node ID {node_id} is at physical node {phys}, out of order: physical nodes must \
             be in ascending node ID order (expected physical node {rank})"
          ),
        ));
      }
      Ok(())
    };

    let mapped = match self.node_id_map {
      NodeIdMapLayout::Dense => {
        let mut mapped = 0usize;
        for index in 0..map.len() / node_map::DENSE_ENTRY_SIZE {
          let phys = read_i32_at(map, index);
          if phys == -1 {
            continue;
          }
          let phys = usize::try_from(phys).map_err(|_| {
            Self::invalid_section(
              SECTION,
              format!("physical node {phys} at node ID {index} is negative"),
            )
          })?;
          check_pair(index as NodeId, phys, mapped)?;
          mapped += 1;
        }
        mapped
      }
      NodeIdMapLayout::Sparse => {
        let count = node_map::sparse_len(map);
        let mut previous: Option<NodeId> = None;
        for index in 0..count {
          let (node_id, phys) = node_map::sparse_entry(map, index);
          if previous.is_some_and(|previous| previous >= node_id) {
            return Err(Self::invalid_section(
              SECTION,
              format!("node ID {node_id} at entry {index} is not strictly ascending"),
            ));
          }
          if node_id > self.header.max_node_id {
            return Err(Self::invalid_section(
              SECTION,
              format!(
                "node ID {node_id} at entry {index} exceeds max node ID {}",
                self.header.max_node_id
              ),
            ));
          }
          check_pair(node_id, phys as usize, index)?;
          previous = Some(node_id);
        }
        count
      }
    };
    if mapped != num_nodes {
      return Err(Self::invalid_section(
        SECTION,
        format!("maps {mapped} node IDs for {num_nodes} nodes"),
      ));
    }
    Ok(())
  }

  /// KeyEntries must reference strings and present nodes.
  fn validate_key_entries(&self, num_strings: usize) -> Result<()> {
    let entries = self.bytes(SectionId::KeyEntries);
    if fast_checks()
      && key_entries_valid(
        entries,
        num_strings,
        self.node_id_map,
        self.bytes(SectionId::NodeIdToPhys),
      )
    {
      return Ok(());
    }
    for index in 0..entries.len() / KEY_INDEX_ENTRY_SIZE {
      let entry_offset = index * KEY_INDEX_ENTRY_SIZE;
      let string_id = read_u32(entries, entry_offset + 8);
      if !string_id_in_table(u64::from(string_id), num_strings) {
        return Err(Self::invalid_section(
          "KeyEntries",
          format!("string ID {string_id} at entry {index} is outside 0..{num_strings}"),
        ));
      }
      let node_id = read_u64(entries, entry_offset + 16);
      if self.phys_node(node_id).is_none() {
        return Err(Self::invalid_section(
          "KeyEntries",
          format!("node ID {node_id} at entry {index} is not present"),
        ));
      }
    }
    Ok(())
  }

  /// lookup_by_key finds entries through KeyBuckets: its offsets must
  /// partition all `entry_count` entries.
  fn validate_key_buckets(buckets: &[u8], entry_count: usize) -> Result<()> {
    Self::validate_u32_offsets(buckets, entry_count, "KeyBuckets")?;
    // validate_section_sizes checked there are at least two offsets.
    if read_u32_at(buckets, 0) != 0
      || read_u32_at(buckets, buckets.len() / 4 - 1) as usize != entry_count
    {
      return Err(Self::invalid_section(
        "KeyBuckets",
        "bucket offsets do not cover all key entries",
      ));
    }
    Ok(())
  }

  /// Without KeyBuckets, lookup_by_key binary-searches entries by hash.
  fn validate_key_order(entries: &[u8]) -> Result<()> {
    let entry_count = entries.len() / KEY_INDEX_ENTRY_SIZE;
    if let Some(index) = (1..entry_count)
      .find(|&index| key_entry_hash(entries, index - 1) > key_entry_hash(entries, index))
    {
      return Err(Self::invalid_section(
        "KeyEntries",
        format!(
          "entry {index} is out of hash order, and there is no KeyBuckets section to \
           find entries by bucket"
        ),
      ));
    }
    Ok(())
  }

  fn validate_vector_store_index(index: &[u8], data_len: usize) -> Result<()> {
    if index.len() < 4 {
      return Err(Self::invalid_section(
        "VectorStoreIndex",
        "index is smaller than its count",
      ));
    }
    let count = read_u32(index, 0) as usize;
    let entries_len = Self::checked_bytes(count, 20, "VectorStoreIndex")?;
    let expected_len = entries_len
      .checked_add(4)
      .ok_or_else(|| Self::invalid_section("VectorStoreIndex", "index size overflows"))?;
    if index.len() != expected_len {
      return Err(Self::invalid_section(
        "VectorStoreIndex",
        format!(
          "count {count} requires {expected_len} bytes, found {}",
          index.len()
        ),
      ));
    }
    for entry in 0..count {
      let entry_offset = 4 + entry * 20;
      let payload_offset = usize::try_from(read_u64(index, entry_offset + 4)).map_err(|_| {
        Self::invalid_section(
          "VectorStoreIndex",
          format!("entry {entry} offset overflows"),
        )
      })?;
      let payload_len = usize::try_from(read_u64(index, entry_offset + 12)).map_err(|_| {
        Self::invalid_section(
          "VectorStoreIndex",
          format!("entry {entry} length overflows"),
        )
      })?;
      let payload_end = payload_offset.checked_add(payload_len).ok_or_else(|| {
        Self::invalid_section("VectorStoreIndex", format!("entry {entry} range overflows"))
      })?;
      if payload_end > data_len {
        return Err(Self::invalid_section(
          "VectorStoreIndex",
          format!("entry {entry} exceeds VectorStoreData"),
        ));
      }
    }
    Ok(())
  }

  /// Names for a LabelStringIds/EtypeStringIds/PropkeyStringIds table,
  /// indexed by schema ID. String ID 0 (and invalid UTF-8) has no name.
  fn decode_names(&self, id: SectionId) -> Vec<Option<Box<str>>> {
    let string_ids = self.bytes(id);
    (0..string_ids.len() / 4)
      .map(|index| match read_u32_at(string_ids, index) {
        0 => None,
        string_id => self.string_str(string_id).map(Box::from),
      })
      .collect()
  }

  // ========================================================================
  // Section access
  // ========================================================================

  /// Decompressed bytes of section `id`, or None if it is absent or empty.
  #[inline]
  pub(crate) fn section(&self, id: SectionId) -> Option<&[u8]> {
    match &self.views[id as usize] {
      SectionView::Empty => None,
      SectionView::Mapped { start, end } => Some(&self.mmap[*start..*end]),
      SectionView::Inflated(bytes) => Some(bytes),
    }
  }

  /// Get decompressed section bytes
  pub fn section_bytes(&self, id: SectionId) -> Option<Vec<u8>> {
    self.section(id).map(<[u8]>::to_vec)
  }

  /// Get section bytes as a slice of the mmap.
  /// Returns None if the section doesn't exist or is compressed.
  pub fn section_slice(&self, id: SectionId) -> Option<&[u8]> {
    match self.views[id as usize] {
      SectionView::Mapped { .. } => self.section(id),
      SectionView::Empty | SectionView::Inflated(_) => None,
    }
  }

  /// Get section data as a slice, decompressing if needed.
  pub fn section_data(&self, id: SectionId) -> Option<Cow<'_, [u8]>> {
    self.section(id).map(Cow::Borrowed)
  }

  /// Get section data as a borrowed slice or shared buffer.
  pub fn section_data_shared(&self, id: SectionId) -> Option<SectionBytes<'_>> {
    self.section(id).map(SectionBytes::Borrowed)
  }

  // ========================================================================
  // Node accessors
  // ========================================================================

  /// Get NodeID for a physical node index
  #[inline]
  pub fn node_id(&self, phys: PhysNode) -> Option<NodeId> {
    u64_at(self.section(SectionId::PhysToNodeId)?, phys as usize)
  }

  /// Get physical node index for a NodeID, or None if not present
  #[inline]
  pub fn phys_node(&self, node_id: NodeId) -> Option<PhysNode> {
    let map = self.section(SectionId::NodeIdToPhys)?;
    match self.node_id_map {
      NodeIdMapLayout::Dense => node_map::dense_lookup(map, node_id),
      NodeIdMapLayout::Sparse => node_map::sparse_lookup(map, node_id),
    }
  }

  /// NodeIdToPhys layout of this snapshot.
  #[inline]
  pub fn node_id_map_layout(&self) -> NodeIdMapLayout {
    self.node_id_map
  }

  /// Bytes per StringOffsets entry: 4 before v5, 8 since.
  #[inline]
  pub fn string_offset_size(&self) -> usize {
    self.string_offset_size
  }

  /// Check if a NodeID exists in the snapshot
  #[inline]
  pub fn has_node(&self, node_id: NodeId) -> bool {
    self.phys_node(node_id).is_some()
  }

  /// Physical index of the first node with an ID greater than `node_id`
  /// (`num_nodes` if there is none). Physical order is ID order.
  pub fn phys_after(&self, node_id: NodeId) -> PhysNode {
    let Some(ids) = self.section(SectionId::PhysToNodeId) else {
      return 0;
    };
    let count = (ids.len() / 8).min(self.header.num_nodes as usize);
    partition_point(count, |phys| read_u64_at(ids, phys) <= node_id) as PhysNode
  }

  /// Get the number of nodes in the snapshot
  #[inline]
  pub fn num_nodes(&self) -> u64 {
    self.header.num_nodes
  }

  /// Get the number of edges in the snapshot
  #[inline]
  pub fn num_edges(&self) -> u64 {
    self.header.num_edges
  }

  /// Get max node ID in the snapshot
  #[inline]
  pub fn max_node_id(&self) -> u64 {
    self.header.max_node_id
  }

  // ========================================================================
  // String table accessors
  // ========================================================================

  /// Get string by StringID
  pub fn string(&self, string_id: StringId) -> Option<String> {
    self.string_str(string_id).map(str::to_owned)
  }

  /// String `string_id`, borrowed, if it is valid UTF-8.
  fn string_str(&self, string_id: StringId) -> Option<&str> {
    std::str::from_utf8(self.string_bytes(string_id)?).ok()
  }

  /// Raw bytes of string `string_id`.
  pub(crate) fn string_bytes(&self, string_id: StringId) -> Option<&[u8]> {
    if string_id == 0 {
      return Some(&[]);
    }
    let offsets = self.section(SectionId::StringOffsets)?;
    let (start, end) = self.string_range(offsets, string_id as usize)?;
    self.bytes(SectionId::StringBytes).get(start..end)
  }

  /// Byte range of string `index` in StringBytes.
  #[inline]
  fn string_range(&self, offsets: &[u8], index: usize) -> Option<(usize, usize)> {
    if self.string_offset_size != 8 {
      return u32_range_at(offsets, index);
    }
    let start = usize::try_from(u64_at(offsets, index)?).ok()?;
    let end = usize::try_from(u64_at(offsets, index.checked_add(1)?)?).ok()?;
    (start <= end).then_some((start, end))
  }

  // ========================================================================
  // Edge accessors
  // ========================================================================

  /// Get out-edge offset range for a physical node
  #[inline]
  fn out_edge_range(&self, phys: PhysNode) -> Option<(usize, usize)> {
    u32_range_at(self.section(SectionId::OutOffsets)?, phys as usize)
  }

  /// Get out-degree for a physical node
  pub fn out_degree(&self, phys: PhysNode) -> Option<usize> {
    let (start, end) = self.out_edge_range(phys)?;
    Some(end - start)
  }

  /// Check if an edge exists in the snapshot (binary search)
  pub fn has_edge(&self, src_phys: PhysNode, etype: ETypeId, dst_phys: PhysNode) -> bool {
    self.find_edge_index(src_phys, etype, dst_phys).is_some()
  }

  /// Find edge index for a specific edge (returns None if not found)
  pub fn find_edge_index(
    &self,
    src_phys: PhysNode,
    etype: ETypeId,
    dst_phys: PhysNode,
  ) -> Option<usize> {
    let (start, end) = self.out_edge_range(src_phys)?;
    let out_etype = self.section(SectionId::OutEtype)?;
    let out_dst = self.section(SectionId::OutDst)?;
    let end = end.min(out_etype.len() / 4).min(out_dst.len() / 4);
    if start >= end {
      return None;
    }

    // Edges are sorted by (etype, dst) within the node's range.
    let target = (etype, dst_phys);
    let index = start
      + partition_point(end - start, |offset| {
        let index = start + offset;
        (read_u32_at(out_etype, index), read_u32_at(out_dst, index)) < target
      });
    (index < end
      && read_u32_at(out_etype, index) == etype
      && read_u32_at(out_dst, index) == dst_phys)
      .then_some(index)
  }

  /// Iterate out-edges for a physical node
  pub fn iter_out_edges(&self, phys: PhysNode) -> OutEdgeIter<'_> {
    OutEdgeIter::new(self, phys)
  }

  /// Get in-edge offset range for a physical node
  #[inline]
  fn in_edge_range(&self, phys: PhysNode) -> Option<(usize, usize)> {
    if !self.header.flags.contains(SnapshotFlags::HAS_IN_EDGES) {
      return None;
    }
    u32_range_at(self.section(SectionId::InOffsets)?, phys as usize)
  }

  /// Get in-degree for a physical node
  pub fn in_degree(&self, phys: PhysNode) -> Option<usize> {
    let (start, end) = self.in_edge_range(phys)?;
    Some(end - start)
  }

  /// Iterate in-edges for a physical node
  pub fn iter_in_edges(&self, phys: PhysNode) -> InEdgeIter<'_> {
    InEdgeIter::new(self, phys)
  }

  /// A node's out-edges as `(etype, dst)` keys, seekable.
  pub fn out_edge_keys(&self, phys: PhysNode) -> EdgeKeyIter<'_> {
    EdgeKeyIter::new(
      self,
      self.out_edge_range(phys),
      SectionId::OutEtype,
      SectionId::OutDst,
    )
  }

  /// A node's in-edges as `(etype, src)` keys, seekable.
  pub fn in_edge_keys(&self, phys: PhysNode) -> EdgeKeyIter<'_> {
    EdgeKeyIter::new(
      self,
      self.in_edge_range(phys),
      SectionId::InEtype,
      SectionId::InSrc,
    )
  }

  // ========================================================================
  // Key index lookup
  // ========================================================================

  /// Look up a node by key in the snapshot
  pub fn lookup_by_key(&self, key: &str) -> Option<NodeId> {
    let entries = self.section(SectionId::KeyEntries)?;
    let num_entries = entries.len() / KEY_INDEX_ENTRY_SIZE;
    if num_entries == 0 {
      return None;
    }
    let hash64 = xxhash64_string(key);

    let (lo, hi) = match self.section(SectionId::KeyBuckets) {
      Some(buckets) => {
        // Load checked there are at least two offsets.
        let num_buckets = (buckets.len() / 4 - 1) as u64;
        let bucket = (hash64 % num_buckets) as usize;
        (
          read_u32_at(buckets, bucket) as usize,
          read_u32_at(buckets, bucket + 1) as usize,
        )
      }
      // Load checked that bucketless entries are sorted by hash.
      None => (
        partition_point(num_entries, |index| key_entry_hash(entries, index) < hash64),
        partition_point(num_entries, |index| {
          key_entry_hash(entries, index) <= hash64
        }),
      ),
    };

    // Check every entry in range with a matching hash (collisions).
    (lo..hi.min(num_entries)).find_map(|index| {
      let offset = index * KEY_INDEX_ENTRY_SIZE;
      if read_u64(entries, offset) != hash64 {
        return None;
      }
      let string_id = read_u32(entries, offset + 8);
      (self.string_bytes(string_id)? == key.as_bytes()).then(|| read_u64(entries, offset + 16))
    })
  }

  /// Get the key for a node, if any
  pub fn node_key(&self, phys: PhysNode) -> Option<String> {
    self.node_key_str(phys).map(str::to_owned)
  }

  /// The key of a node, borrowed, if it has one.
  pub fn node_key_str(&self, phys: PhysNode) -> Option<&str> {
    let string_id = u32_at(self.section(SectionId::NodeKeyString)?, phys as usize)?;
    if string_id == 0 {
      return None;
    }
    self.string_str(string_id)
  }

  // ========================================================================
  // Label access
  // ========================================================================

  /// Get all labels for a node
  pub fn node_labels(&self, phys: PhysNode) -> Option<Vec<LabelId>> {
    if !self.header.flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      return None;
    }

    let offsets = self.section(SectionId::NodeLabelOffsets)?;
    let labels = self.bytes(SectionId::NodeLabelIds);
    let (start, end) = u32_range_at(offsets, phys as usize)?;
    let end = end.min(labels.len() / 4);

    Some(
      (start..end)
        .map(|i| read_u32_at(labels, i) as LabelId)
        .collect(),
    )
  }

  /// Whether a node has label `label_id`, without listing its labels.
  pub fn node_has_label(&self, phys: PhysNode, label_id: LabelId) -> bool {
    if !self.header.flags.contains(SnapshotFlags::HAS_NODE_LABELS) {
      return false;
    }
    let Some(offsets) = self.section(SectionId::NodeLabelOffsets) else {
      return false;
    };
    let labels = self.bytes(SectionId::NodeLabelIds);
    let Some((start, end)) = u32_range_at(offsets, phys as usize) else {
      return false;
    };
    (start..end.min(labels.len() / 4)).any(|i| read_u32_at(labels, i) == label_id)
  }

  // ========================================================================
  // Property access
  // ========================================================================

  /// Property key and value arrays and the range of `index` in them.
  #[inline]
  fn prop_range(
    &self,
    offsets: SectionId,
    keys: SectionId,
    vals: SectionId,
    index: usize,
  ) -> Option<(&[u8], &[u8], usize, usize)> {
    if !self.header.flags.contains(SnapshotFlags::HAS_PROPERTIES) {
      return None;
    }
    let (start, end) = u32_range_at(self.section(offsets)?, index)?;
    let keys = self.bytes(keys);
    Some((keys, self.bytes(vals), start, end.min(keys.len() / 4)))
  }

  fn collect_props(
    &self,
    offsets: SectionId,
    keys: SectionId,
    vals: SectionId,
    index: usize,
  ) -> Option<HashMap<PropKeyId, PropValue>> {
    let (keys, vals, start, end) = self.prop_range(offsets, keys, vals, index)?;
    let mut props = HashMap::with_capacity(end.saturating_sub(start));
    for i in start..end {
      if let Some(value) = self.decode_prop_value(vals, i) {
        props.insert(read_u32_at(keys, i), value);
      }
    }
    Some(props)
  }

  /// Get all properties for a node
  pub fn node_props(&self, phys: PhysNode) -> Option<HashMap<PropKeyId, PropValue>> {
    self.collect_props(
      SectionId::NodePropOffsets,
      SectionId::NodePropKeys,
      SectionId::NodePropVals,
      phys as usize,
    )
  }

  /// Get a specific property for a node
  pub fn node_prop(&self, phys: PhysNode, prop_key_id: PropKeyId) -> Option<PropValue> {
    let (keys, vals, start, end) = self.prop_range(
      SectionId::NodePropOffsets,
      SectionId::NodePropKeys,
      SectionId::NodePropVals,
      phys as usize,
    )?;
    let index = (start..end).find(|&i| read_u32_at(keys, i) == prop_key_id)?;
    self.decode_prop_value(vals, index)
  }

  /// Get all properties for an edge by edge index
  pub fn edge_props(&self, edge_idx: usize) -> Option<HashMap<PropKeyId, PropValue>> {
    self.collect_props(
      SectionId::EdgePropOffsets,
      SectionId::EdgePropKeys,
      SectionId::EdgePropVals,
      edge_idx,
    )
  }

  /// Decode property value `index` from disk format
  fn decode_prop_value(&self, vals: &[u8], index: usize) -> Option<PropValue> {
    if index >= vals.len() / PROP_VALUE_DISK_SIZE {
      return None;
    }

    let offset = index * PROP_VALUE_DISK_SIZE;
    let tag = vals[offset];
    let payload = read_u64(vals, offset + 8);

    match PropValueTag::from_u8(tag)? {
      PropValueTag::Null => Some(PropValue::Null),
      PropValueTag::Bool => Some(PropValue::Bool(payload != 0)),
      PropValueTag::I64 => Some(PropValue::I64(payload as i64)),
      PropValueTag::F64 => Some(PropValue::F64(f64::from_bits(payload))),
      PropValueTag::String => {
        let s = self.string(StringId::try_from(payload).ok()?)?;
        Some(PropValue::String(s))
      }
      PropValueTag::VectorF32 => {
        if !self.header.flags.contains(SnapshotFlags::HAS_VECTORS) {
          return None;
        }

        let offsets = self.section(SectionId::VectorOffsets)?;
        let data = self.bytes(SectionId::VectorData);

        let idx = usize::try_from(payload).ok()?;
        let start = usize::try_from(u64_at(offsets, idx)?).ok()?;
        let end = usize::try_from(u64_at(offsets, idx.checked_add(1)?)?).ok()?;
        let bytes = data.get(start..end)?;
        if bytes.len() % 4 != 0 {
          return None;
        }

        let vec = bytes
          .as_chunks::<4>()
          .0
          .iter()
          .map(|chunk| f32::from_le_bytes(*chunk))
          .collect();
        Some(PropValue::VectorF32(vec))
      }
    }
  }
}

// ============================================================================
// Edge Iterators
// ============================================================================

/// Iterator over out-edges
pub struct OutEdgeIter<'a> {
  /// The node's run of OutEtype and OutDst
  etypes: std::slice::Iter<'a, [u8; 4]>,
  dsts: std::slice::Iter<'a, [u8; 4]>,
}

impl<'a> OutEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let (etypes, dsts) = u32_runs(
      snapshot.bytes(SectionId::OutEtype),
      snapshot.bytes(SectionId::OutDst),
      snapshot.out_edge_range(phys).unwrap_or((0, 0)),
    );
    Self {
      etypes: etypes.iter(),
      dsts: dsts.iter(),
    }
  }
}

impl<'a> Iterator for OutEdgeIter<'a> {
  type Item = (PhysNode, ETypeId); // (dst, etype)

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    let dst = u32::from_le_bytes(*self.dsts.next()?);
    let etype = u32::from_le_bytes(*self.etypes.next()?);
    Some((dst, etype))
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    self.dsts.size_hint()
  }
}

impl<'a> ExactSizeIterator for OutEdgeIter<'a> {}

/// Iterator over in-edges
pub struct InEdgeIter<'a> {
  /// The node's run of InEtype and InSrc, and the index of its first edge
  etypes: std::slice::Iter<'a, [u8; 4]>,
  srcs: std::slice::Iter<'a, [u8; 4]>,
  in_out_index: &'a [u8],
  current: usize,
}

impl<'a> InEdgeIter<'a> {
  fn new(snapshot: &'a SnapshotData, phys: PhysNode) -> Self {
    let range = snapshot.in_edge_range(phys).unwrap_or((0, 0));
    let (etypes, srcs) = u32_runs(
      snapshot.bytes(SectionId::InEtype),
      snapshot.bytes(SectionId::InSrc),
      range,
    );
    Self {
      etypes: etypes.iter(),
      srcs: srcs.iter(),
      in_out_index: snapshot.bytes(SectionId::InOutIndex),
      current: range.0,
    }
  }
}

impl<'a> Iterator for InEdgeIter<'a> {
  type Item = (PhysNode, ETypeId, u32); // (src, etype, out_index)

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    let src = u32::from_le_bytes(*self.srcs.next()?);
    let etype = u32::from_le_bytes(*self.etypes.next()?);
    let out_index = u32_at(self.in_out_index, self.current).unwrap_or(0);
    self.current += 1;
    Some((src, etype, out_index))
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    self.srcs.size_hint()
  }
}

impl<'a> ExactSizeIterator for InEdgeIter<'a> {}

/// A node's out-edges as `(etype, dst)` or in-edges as `(etype, src)`, with
/// the other endpoint's node ID, in that key order (the order the snapshot
/// stores them in, by physical index, which is ID order), seekable by key. The
/// default holds no edges.
#[derive(Default)]
pub struct EdgeKeyIter<'a> {
  /// The remaining run of edge types and other endpoints' physical indexes
  etypes: &'a [[u8; 4]],
  others: &'a [[u8; 4]],
  /// PhysToNodeId
  node_ids: &'a [[u8; 8]],
}

impl<'a> EdgeKeyIter<'a> {
  fn new(
    snapshot: &'a SnapshotData,
    range: Option<(usize, usize)>,
    etypes: SectionId,
    others: SectionId,
  ) -> Self {
    let (etypes, others) = u32_runs(
      snapshot.bytes(etypes),
      snapshot.bytes(others),
      range.unwrap_or((0, 0)),
    );
    Self {
      etypes,
      others,
      node_ids: snapshot.bytes(SectionId::PhysToNodeId).as_chunks::<8>().0,
    }
  }

  fn key(&self, index: usize) -> (ETypeId, PhysNode) {
    (
      u32::from_le_bytes(self.etypes[index]),
      u32::from_le_bytes(self.others[index]),
    )
  }

  /// Skip to the first remaining edge at or after `(etype, other)`, the other
  /// endpoint given as a physical index (see [`SnapshotData::phys_after`]): a
  /// binary search.
  pub fn seek(&mut self, from: (ETypeId, PhysNode)) {
    let skip = partition_point(self.etypes.len(), |index| self.key(index) < from);
    self.etypes = &self.etypes[skip..];
    self.others = &self.others[skip..];
  }

  /// Stop after the edges of type `etype`: a binary search.
  pub fn end_after_etype(&mut self, etype: ETypeId) {
    let keep = partition_point(self.etypes.len(), |index| {
      u32::from_le_bytes(self.etypes[index]) <= etype
    });
    self.etypes = &self.etypes[..keep];
    self.others = &self.others[..keep];
  }

  /// The number of edges left (some of which `next` may skip: those whose
  /// endpoint is past PhysToNodeId).
  pub fn remaining(&self) -> usize {
    self.etypes.len()
  }

  /// Append the remaining keys to `out` in order, as `item` maps them, each key
  /// once (a snapshot may hold an edge twice), until `out` holds `limit` items:
  /// one pass over the snapshot's arrays, with room made first.
  pub fn append_keys<T>(
    &mut self,
    out: &mut Vec<T>,
    limit: usize,
    item: impl Fn(ETypeId, NodeId) -> T,
  ) {
    let room = limit.saturating_sub(out.len());
    // With room for every key, no count to check.
    let bounded = room < self.etypes.len();
    out.reserve(room.min(self.etypes.len()));
    let node_ids = self.node_ids;
    let mut last = None;
    let mut visited = 0;
    for (etype, other) in self.etypes.iter().zip(self.others) {
      if bounded && out.len() >= limit {
        break;
      }
      visited += 1;
      if last == Some((*etype, *other)) {
        continue;
      }
      last = Some((*etype, *other));
      if let Some(node_id) = node_ids.get(u32::from_le_bytes(*other) as usize) {
        out.push(item(
          u32::from_le_bytes(*etype),
          u64::from_le_bytes(*node_id),
        ));
      }
    }
    self.etypes = &self.etypes[visited..];
    self.others = &self.others[visited..];
  }

  /// Visit the remaining keys in order until `f` breaks, in one pass over the
  /// snapshot's arrays: what reads that take many edges use instead of `next`.
  #[inline]
  pub fn try_for_each_key(
    &mut self,
    mut f: impl FnMut(ETypeId, NodeId) -> std::ops::ControlFlow<()>,
  ) -> std::ops::ControlFlow<()> {
    let node_ids = self.node_ids;
    let mut visited = 0;
    let result = self
      .etypes
      .iter()
      .zip(self.others)
      .try_for_each(|(etype, other)| {
        visited += 1;
        match node_ids.get(u32::from_le_bytes(*other) as usize) {
          Some(node_id) => f(u32::from_le_bytes(*etype), u64::from_le_bytes(*node_id)),
          None => std::ops::ControlFlow::Continue(()),
        }
      });
    self.etypes = &self.etypes[visited..];
    self.others = &self.others[visited..];
    result
  }
}

impl Iterator for EdgeKeyIter<'_> {
  /// `(etype, other endpoint's node ID)`
  type Item = (ETypeId, NodeId);

  #[inline]
  fn next(&mut self) -> Option<Self::Item> {
    while let (Some((etype, etypes)), Some((other, others))) =
      (self.etypes.split_first(), self.others.split_first())
    {
      self.etypes = etypes;
      self.others = others;
      if let Some(node_id) = self.node_ids.get(u32::from_le_bytes(*other) as usize) {
        return Some((u32::from_le_bytes(*etype), u64::from_le_bytes(*node_id)));
      }
    }
    None
  }

  fn size_hint(&self) -> (usize, Option<usize>) {
    (0, Some(self.etypes.len()))
  }
}

// ============================================================================
// Extended SnapshotData methods for compaction
// ============================================================================

/// Out-edge info for compaction
pub struct OutEdgeInfo {
  pub dst: PhysNode,
  pub etype: ETypeId,
}

impl SnapshotData {
  /// Get label name by LabelID
  pub fn label_name(&self, label_id: LabelId) -> Option<&str> {
    self.label_names.get(label_id as usize)?.as_deref()
  }

  /// Get etype name by ETypeID
  pub fn etype_name(&self, etype_id: ETypeId) -> Option<&str> {
    self.etype_names.get(etype_id as usize)?.as_deref()
  }

  /// Get propkey name by PropKeyID
  pub fn propkey_name(&self, propkey_id: PropKeyId) -> Option<&str> {
    self.propkey_names.get(propkey_id as usize)?.as_deref()
  }

  /// Get out-edges as a Vec for compaction purposes
  pub fn out_edges(&self, phys: PhysNode) -> Vec<OutEdgeInfo> {
    self
      .iter_out_edges(phys)
      .map(|(dst, etype)| OutEdgeInfo { dst, etype })
      .collect()
  }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::snapshot::writer::{build_snapshot_to_memory, NodeData, SnapshotBuildInput};
  use crate::types::PropValue;
  use crate::util::crc::crc32;
  use crate::util::mmap::map_file;
  use std::collections::HashMap;
  use std::fs::{self, File};
  use std::sync::{Arc, Mutex};
  use tempfile::tempdir;

  fn corruption_test_snapshot() -> Vec<u8> {
    let mut node_props = HashMap::new();
    node_props.insert(1, PropValue::String("node-value".to_string()));

    build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![
        NodeData {
          node_id: 1,
          key: Some("alpha".to_string()),
          labels: vec![1],
          props: node_props,
        },
        NodeData {
          node_id: 2,
          key: Some("beta".to_string()),
          labels: vec![1],
          props: HashMap::new(),
        },
      ],
      edges: vec![crate::core::snapshot::writer::EdgeData {
        src: 1,
        etype: 1,
        dst: 2,
        props: HashMap::new(),
      }],
      labels: HashMap::from([(1, "person".to_string())]),
      etypes: HashMap::from([(1, "knows".to_string())]),
      propkeys: HashMap::from([(1, "value".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("snapshot build")
  }

  fn section_payload_offset(bytes: &[u8], id: SectionId) -> usize {
    let table_offset = SNAPSHOT_HEADER_SIZE + id as usize * SECTION_ENTRY_SIZE;
    read_u64(bytes, table_offset) as usize
  }

  fn load_corrupt_snapshot(bytes: &[u8], skip_crc_validation: bool) -> Result<()> {
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("corrupt-snapshot.gds");
    fs::write(&path, bytes).expect("write snapshot");
    SnapshotData::load_with_options(
      &path,
      &ParseSnapshotOptions {
        skip_crc_validation,
        ..ParseSnapshotOptions::default()
      },
    )
    .map(|_| ())
  }

  #[test]
  fn test_parse_snapshot_options_default() {
    let opts = ParseSnapshotOptions::default();
    assert!(!opts.skip_crc_validation);
    assert!(opts.crc_chunk_size.is_none());
    assert!(opts.crc_profile_sink.is_none());
  }

  #[test]
  fn test_parse_collects_crc_section_profile() {
    let mut props = HashMap::new();
    props.insert(1, PropValue::VectorF32(vec![0.1, 0.2, 0.3, 0.4]));

    let bytes = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 1,
        key: Some("n1".to_string()),
        labels: Vec::new(),
        props,
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::from([(1, "embedding".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("snapshot build");

    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("profile-snapshot.gds");
    fs::write(&path, &bytes).expect("write snapshot");

    let profile_sink = Arc::new(Mutex::new(None));
    let options = ParseSnapshotOptions {
      skip_crc_validation: false,
      crc_chunk_size: Some(128),
      crc_profile_sink: Some(Arc::clone(&profile_sink)),
    };

    let _snapshot = SnapshotData::load_with_options(&path, &options).expect("snapshot parse");

    let profile = profile_sink
      .lock()
      .expect("profile lock")
      .clone()
      .expect("profile");
    assert_eq!(profile.chunk_size, 128);
    assert!(profile.total_bytes > 0);
    assert!(profile.total_ns > 0);
    assert!(!profile.sections.is_empty());
    assert_eq!(
      profile
        .sections
        .iter()
        .map(|entry| entry.bytes)
        .sum::<usize>(),
      profile.total_bytes
    );
    assert!(profile
      .sections
      .iter()
      .any(|entry| entry.section_id == Some(SectionId::VectorData) && entry.bytes > 0));
  }

  #[test]
  fn test_parse_at_offset_rejects_offset_past_mmap_without_panicking() {
    let bytes = corruption_test_snapshot();
    let dir = tempdir().expect("temp dir");
    let path = dir.path().join("offset-snapshot.gds");
    fs::write(&path, &bytes).expect("write snapshot");
    let file = File::open(&path).expect("open snapshot");
    let mmap = map_file(&file).expect("map snapshot");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      SnapshotData::parse_at_offset(
        Arc::new(mmap),
        bytes.len().saturating_add(1),
        &ParseSnapshotOptions {
          skip_crc_validation: true,
          ..ParseSnapshotOptions::default()
        },
      )
    }));

    assert!(result.is_ok(), "offset parsing panicked");
    assert!(matches!(
      result.expect("panic checked"),
      Err(KiteError::InvalidSnapshot(_))
    ));
  }

  #[test]
  #[allow(clippy::type_complexity)]
  fn test_snapshot_corruption_matrix_returns_errors_without_panicking() {
    let valid = corruption_test_snapshot();
    let section_table_end = SNAPSHOT_HEADER_SIZE + SectionId::COUNT * SECTION_ENTRY_SIZE;
    let mut truncation_boundaries = vec![SNAPSHOT_HEADER_SIZE, section_table_end];
    for id in [
      SectionId::PhysToNodeId,
      SectionId::OutOffsets,
      SectionId::StringOffsets,
      SectionId::KeyEntries,
      SectionId::KeyBuckets,
    ] {
      truncation_boundaries.push(section_payload_offset(&valid, id));
    }

    for end in truncation_boundaries {
      let truncated = &valid[..end.min(valid.len().saturating_sub(1))];
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_corrupt_snapshot(truncated, true)
      }));
      assert!(result.is_ok(), "truncation at {end} panicked");
      assert!(
        result.expect("panic checked").is_err(),
        "truncation at {end} accepted"
      );
    }

    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 4] = [
      (
        "string offsets descending",
        Box::new(|bytes| {
          let offset = section_payload_offset(bytes, SectionId::StringOffsets);
          write_u32(bytes, offset + 4, 1);
          write_u32(bytes, offset + 8, 0);
        }),
      ),
      (
        "out csr offsets descending",
        Box::new(|bytes| {
          let offset = section_payload_offset(bytes, SectionId::OutOffsets);
          write_u32(bytes, offset + 4, 2);
          write_u32(bytes, offset + 8, 1);
        }),
      ),
      (
        "key bucket outside entries",
        Box::new(|bytes| {
          let entry_table =
            SNAPSHOT_HEADER_SIZE + SectionId::KeyEntries as usize * SECTION_ENTRY_SIZE;
          let entry_count = read_u64(bytes, entry_table + 8) as usize / KEY_INDEX_ENTRY_SIZE;
          let offset = section_payload_offset(bytes, SectionId::KeyBuckets);
          write_u32(bytes, offset, entry_count as u32 + 1);
        }),
      ),
      (
        "header node count mismatch",
        Box::new(|bytes| write_u64(bytes, 32, 3)),
      ),
    ];

    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      let footer_crc = crc32(&corrupted[..corrupted.len() - 4]);
      let footer_offset = corrupted.len() - 4;
      write_u32(&mut corrupted, footer_offset, footer_crc);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_corrupt_snapshot(&corrupted, false)
      }));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }

    for (name, field_offset, values) in [
      ("header node count", 32usize, [0u64, 1, u64::MAX].as_slice()),
      ("header edge count", 40usize, [0u64, u64::MAX].as_slice()),
      (
        "header max node ID",
        48usize,
        [0u64, 1, u64::MAX].as_slice(),
      ),
      (
        "header string count",
        80usize,
        [0u64, 1, u64::MAX].as_slice(),
      ),
    ] {
      for &value in values {
        let mut corrupted = valid.clone();
        write_u64(&mut corrupted, field_offset, value);
        let footer_crc = crc32(&corrupted[..corrupted.len() - 4]);
        let footer_offset = corrupted.len() - 4;
        write_u32(&mut corrupted, footer_offset, footer_crc);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          load_corrupt_snapshot(&corrupted, false)
        }));
        assert!(result.is_ok(), "{name}={value} panicked");
        assert!(
          result.expect("panic checked").is_err(),
          "{name}={value} accepted"
        );
      }
    }
  }

  fn rewrite_crc(bytes: &mut [u8]) {
    let footer_offset = bytes.len() - 4;
    let footer_crc = crc32(&bytes[..footer_offset]);
    write_u32(bytes, footer_offset, footer_crc);
  }

  /// Nodes 1, 2^40 and u64::MAX - 1: far too spread out for the dense map.
  fn sparse_test_snapshot() -> Vec<u8> {
    let node = |node_id| NodeData {
      node_id,
      key: None,
      labels: Vec::new(),
      props: HashMap::new(),
    };
    build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![node(1), node(1 << 40), node(u64::MAX - 1)],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::new(),
      vector_stores: None,
      compression: None,
    })
    .expect("snapshot build")
  }

  #[test]
  fn test_sparse_node_map_corruption_returns_errors_without_panicking() {
    let valid = sparse_test_snapshot();
    load_corrupt_snapshot(&valid, false).expect("valid sparse snapshot loads");
    let map = section_payload_offset(&valid, SectionId::NodeIdToPhys);
    let entry = |index: usize| map + index * node_map::SPARSE_ENTRY_SIZE;

    #[allow(clippy::type_complexity)]
    let mutations: [(&str, Box<dyn Fn(&mut Vec<u8>)>); 4] = [
      (
        "entries out of order",
        Box::new(move |bytes| write_u64(bytes, entry(0), 1 << 41)),
      ),
      (
        "duplicate node ID",
        Box::new(move |bytes| write_u64(bytes, entry(1), 1)),
      ),
      (
        "node ID above max",
        Box::new(move |bytes| write_u64(bytes, entry(2), u64::MAX)),
      ),
      (
        "phys outside node count",
        Box::new(move |bytes| write_u32(bytes, entry(1) + 8, 3)),
      ),
    ];
    for (name, mutate) in mutations {
      let mut corrupted = valid.clone();
      mutate(&mut corrupted);
      rewrite_crc(&mut corrupted);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        load_corrupt_snapshot(&corrupted, false)
      }));
      assert!(result.is_ok(), "{name} panicked");
      assert!(result.expect("panic checked").is_err(), "{name} accepted");
    }
  }

  /// v5 stores uncompressed_size as a u64 at +20; its high half must count.
  #[test]
  fn test_compressed_section_size_uses_all_64_bits() {
    let mut node_props = HashMap::new();
    node_props.insert(1, PropValue::String("x".repeat(4096)));
    let mut bytes = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![NodeData {
        node_id: 1,
        key: None,
        labels: Vec::new(),
        props: node_props,
      }],
      edges: Vec::new(),
      labels: HashMap::new(),
      etypes: HashMap::new(),
      propkeys: HashMap::from([(1, "value".to_string())]),
      vector_stores: None,
      compression: Some(crate::util::compression::CompressionOptions {
        enabled: true,
        min_size: 1,
        ..Default::default()
      }),
    })
    .expect("snapshot build");
    load_corrupt_snapshot(&bytes, false).expect("valid compressed snapshot loads");

    let entry = SNAPSHOT_HEADER_SIZE + SectionId::StringBytes as usize * SECTION_ENTRY_SIZE;
    assert_ne!(read_u32(&bytes, entry + 16), 0, "StringBytes is compressed");
    let declared = read_u64(&bytes, entry + 20);
    write_u64(&mut bytes, entry + 20, declared + (1 << 32));
    rewrite_crc(&mut bytes);
    assert!(load_corrupt_snapshot(&bytes, false).is_err());
  }
}

/// Audit S3: every accessor that indexes by a caller-provided ID must return
/// a miss for out-of-range IDs. Unchecked `idx * N + M` bounds checks wrap for
/// IDs >= 2^62 (u64 node IDs, usize edge indices) and panic with overflow
/// checks or alias low entries without them.
#[cfg(test)]
mod audit_tests {
  use super::*;
  use crate::core::snapshot::writer::{
    build_snapshot_to_memory, EdgeData, NodeData, SnapshotBuildInput,
  };
  use std::fmt::Debug;
  use std::io::Write;
  use std::panic::{catch_unwind, AssertUnwindSafe};
  use tempfile::NamedTempFile;

  const KNOWS: ETypeId = 1;
  const NAME: PropKeyId = 1;
  const WEIGHT: PropKeyId = 2;

  /// Nodes 1 and 2 with keys, labels and props; edge 1 -> 2 with a prop.
  /// NodeIdToPhys is [-1, 0, 1], so IDs 2^62 + 1 and 2^62 + 2 alias nodes 1
  /// and 2 once `id * 4` wraps.
  fn small_snapshot() -> (NamedTempFile, SnapshotData) {
    let node = |node_id: NodeId, key: &str| NodeData {
      node_id,
      key: Some(key.to_string()),
      labels: vec![1],
      props: HashMap::from([(NAME, PropValue::String(key.to_string()))]),
    };
    let buffer = build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: vec![node(1, "alpha"), node(2, "beta")],
      edges: vec![EdgeData {
        src: 1,
        etype: KNOWS,
        dst: 2,
        props: HashMap::from([(WEIGHT, PropValue::F64(0.5))]),
      }],
      labels: HashMap::from([(1, "Thing".to_string())]),
      etypes: HashMap::from([(KNOWS, "KNOWS".to_string())]),
      propkeys: HashMap::from([(NAME, "name".to_string()), (WEIGHT, "weight".to_string())]),
      vector_stores: None,
      compression: None,
    })
    .expect("build snapshot");
    let mut file = NamedTempFile::new().expect("temp file");
    file.write_all(&buffer).expect("write snapshot");
    file.flush().expect("flush snapshot");
    let snapshot = SnapshotData::load(file.path()).expect("load snapshot");
    (file, snapshot)
  }

  fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
      (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
      message.clone()
    } else {
      "non-string panic".to_string()
    }
  }

  fn probe<T: PartialEq + Debug>(
    failures: &mut Vec<String>,
    what: String,
    expected: T,
    f: impl FnOnce() -> T,
  ) {
    match catch_unwind(AssertUnwindSafe(f)) {
      Ok(actual) if actual == expected => {}
      Ok(actual) => failures.push(format!("{what} = {actual:?}, expected {expected:?}")),
      Err(payload) => failures.push(format!("{what} panicked: {}", panic_message(&*payload))),
    }
  }

  fn assert_no_failures(failures: Vec<String>) {
    assert!(
      failures.is_empty(),
      "out-of-range lookups must miss cleanly:\n{}",
      failures.join("\n")
    );
  }

  #[test]
  fn s3_phys_node_rejects_node_ids_past_the_mapping() {
    let (_file, snapshot) = small_snapshot();
    let mut failures = Vec::new();
    for id in [
      3,
      1 << 32,
      1 << 62,
      (1 << 62) + 1,
      (1 << 62) + 2,
      (1 << 63) + 1,
      u64::MAX - 1,
      u64::MAX,
    ] {
      probe(&mut failures, format!("phys_node({id})"), None, || {
        snapshot.phys_node(id)
      });
      probe(&mut failures, format!("has_node({id})"), false, || {
        snapshot.has_node(id)
      });
    }
    assert_no_failures(failures);
  }

  /// `edge_props` takes a raw usize. The u32-indexed accessors cannot overflow
  /// usize on 64-bit targets, but would on wasm32; they are probed here so the
  /// fix covers them all.
  #[test]
  fn s3_index_accessors_reject_indices_past_their_sections() {
    let (_file, snapshot) = small_snapshot();
    let mut failures = Vec::new();

    for edge_idx in [1, 1 << 32, 1 << 62, (1 << 62) + 1, usize::MAX] {
      probe(
        &mut failures,
        format!("edge_props({edge_idx})"),
        None,
        || snapshot.edge_props(edge_idx),
      );
    }

    for phys in [2, u32::MAX] {
      probe(&mut failures, format!("node_id({phys})"), None, || {
        snapshot.node_id(phys)
      });
      probe(&mut failures, format!("node_key({phys})"), None, || {
        snapshot.node_key(phys)
      });
      probe(&mut failures, format!("node_labels({phys})"), None, || {
        snapshot.node_labels(phys)
      });
      probe(&mut failures, format!("node_props({phys})"), None, || {
        snapshot.node_props(phys)
      });
      probe(&mut failures, format!("node_prop({phys})"), None, || {
        snapshot.node_prop(phys, NAME)
      });
      probe(&mut failures, format!("out_degree({phys})"), None, || {
        snapshot.out_degree(phys)
      });
      probe(&mut failures, format!("in_degree({phys})"), None, || {
        snapshot.in_degree(phys)
      });
      probe(&mut failures, format!("has_edge({phys}, 1)"), false, || {
        snapshot.has_edge(phys, KNOWS, 1)
      });
      probe(&mut failures, format!("has_edge(0, {phys})"), false, || {
        snapshot.has_edge(0, KNOWS, phys)
      });
      probe(
        &mut failures,
        format!("find_edge_index({phys}, 1)"),
        None,
        || snapshot.find_edge_index(phys, KNOWS, 1),
      );
      probe(&mut failures, format!("iter_out_edges({phys})"), 0, || {
        snapshot.iter_out_edges(phys).count()
      });
      probe(&mut failures, format!("iter_in_edges({phys})"), 0, || {
        snapshot.iter_in_edges(phys).count()
      });
    }

    for id in [u32::MAX - 1, u32::MAX] {
      probe(&mut failures, format!("string({id})"), None, || {
        snapshot.string(id)
      });
      probe(&mut failures, format!("label_name({id})"), None, || {
        snapshot.label_name(id).map(str::to_string)
      });
      probe(&mut failures, format!("etype_name({id})"), None, || {
        snapshot.etype_name(id).map(str::to_string)
      });
      probe(&mut failures, format!("propkey_name({id})"), None, || {
        snapshot.propkey_name(id).map(str::to_string)
      });
    }

    assert_no_failures(failures);
  }
}

#[cfg(test)]
#[path = "b4_open_perf_tests.rs"]
mod b4_open_perf_tests;
