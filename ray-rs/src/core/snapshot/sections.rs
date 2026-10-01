//! Snapshot section parsing
//!
//! Section definitions and parsing helpers

use crate::constants::SECTION_ALIGNMENT;
use crate::error::{KiteError, Result};
use crate::types::{
  SectionEntry, SectionId, SECTION_ENTRY_SIZE, SECTION_ENTRY_SIZE_V4, SNAPSHOT_HEADER_SIZE,
};
use crate::util::binary::{read_u32, read_u64};
use crate::util::compression::{CompressionType, MAX_DECOMPRESSED_BYTES};

/// Parsed section table metadata
#[derive(Debug, Clone)]
pub struct ParsedSections {
  pub sections: Vec<SectionEntry>,
  pub max_section_end: usize,
}

/// First version with u64 section sizes and u64 string offsets.
const FIRST_WIDE_VERSION: u32 = 5;

/// Resolve section table size for a snapshot version.
pub fn section_count_for_version(version: u32) -> usize {
  if version >= 4 {
    SectionId::COUNT
  } else if version >= 3 {
    SectionId::COUNT_V3
  } else if version >= 2 {
    SectionId::COUNT_V2
  } else {
    SectionId::COUNT_V1
  }
}

/// Bytes per section table entry for a snapshot version.
pub fn section_entry_size_for_version(version: u32) -> usize {
  if version >= FIRST_WIDE_VERSION {
    SECTION_ENTRY_SIZE
  } else {
    SECTION_ENTRY_SIZE_V4
  }
}

/// Bytes per StringOffsets entry for a snapshot version (u32 before v5).
pub fn string_offset_size_for_version(version: u32) -> usize {
  if version >= FIRST_WIDE_VERSION {
    8
  } else {
    4
  }
}

/// Compressed sections are inflated into memory, so their declared size is
/// capped. Uncompressed sections are borrowed from the mmap and only need to
/// match their on-disk length.
fn validate_declared_uncompressed_size(section_index: usize, size: u64) -> Result<()> {
  let size = usize::try_from(size).map_err(|_| {
    KiteError::InvalidSnapshot(format!(
      "Section {section_index} declared uncompressed size does not fit in usize"
    ))
  })?;
  if size > MAX_DECOMPRESSED_BYTES {
    return Err(KiteError::InvalidSnapshot(format!(
      "Section {section_index} declared uncompressed size {size} exceeds limit {MAX_DECOMPRESSED_BYTES}"
    )));
  }
  Ok(())
}

/// Parse and validate the snapshot section table.
///
/// `buffer` is the snapshot slice starting at the header.
/// `version` is the snapshot format version from the header.
/// `base_offset` is the absolute file offset of the snapshot start (0 for standalone snapshots).
pub fn parse_section_table(
  buffer: &[u8],
  version: u32,
  base_offset: usize,
) -> Result<ParsedSections> {
  let section_count = section_count_for_version(version);
  let entry_size = section_entry_size_for_version(version);
  let wide_sizes = version >= FIRST_WIDE_VERSION;
  let section_table_size = section_count.checked_mul(entry_size).ok_or_else(|| {
    KiteError::InvalidSnapshot("Snapshot section table size overflow".to_string())
  })?;
  let table_end = SNAPSHOT_HEADER_SIZE
    .checked_add(section_table_size)
    .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot section table end overflow".to_string()))?;

  if buffer.len() < table_end {
    return Err(KiteError::InvalidSnapshot(format!(
      "Snapshot too small for section table: {} bytes",
      buffer.len()
    )));
  }

  let data_start = table_end
    .checked_add(SECTION_ALIGNMENT - 1)
    .map(|value| value & !(SECTION_ALIGNMENT - 1))
    .ok_or_else(|| KiteError::InvalidSnapshot("Snapshot data start overflow".to_string()))?;
  let mut sections = Vec::with_capacity(section_count);
  let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
  let mut max_section_end = table_end;

  let mut offset = SNAPSHOT_HEADER_SIZE;
  for idx in 0..section_count {
    let section_offset = usize::try_from(read_u64(buffer, offset)).map_err(|_| {
      KiteError::InvalidSnapshot(format!("Section {idx} offset does not fit in usize"))
    })?;
    let section_length = usize::try_from(read_u64(buffer, offset + 8)).map_err(|_| {
      KiteError::InvalidSnapshot(format!("Section {idx} length does not fit in usize"))
    })?;
    let compression = read_u32(buffer, offset + 16);
    let uncompressed_size = if wide_sizes {
      read_u64(buffer, offset + 20)
    } else {
      u64::from(read_u32(buffer, offset + 20))
    };
    offset += entry_size;

    if section_length == 0 {
      if compression != 0 || uncompressed_size != 0 {
        return Err(KiteError::InvalidSnapshot(format!(
          "Section {idx} has length 0 but non-zero metadata",
        )));
      }

      sections.push(SectionEntry {
        offset: 0,
        length: 0,
        compression,
        uncompressed_size,
      });
      continue;
    }

    if section_offset == 0 {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} has data but offset is 0"
      )));
    }

    if section_offset < data_start {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} offset {section_offset} overlaps header/table"
      )));
    }

    if section_offset % SECTION_ALIGNMENT != 0 {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} offset {section_offset} is not {SECTION_ALIGNMENT}-byte aligned"
      )));
    }

    let compression_type = CompressionType::from_u32(compression).ok_or_else(|| {
      KiteError::InvalidSnapshot(format!(
        "Section {idx} has invalid compression type {compression}"
      ))
    })?;

    if compression_type == CompressionType::None {
      if uncompressed_size != 0 && usize::try_from(uncompressed_size).ok() != Some(section_length) {
        return Err(KiteError::InvalidSnapshot(format!(
          "Section {idx} uncompressed_size {uncompressed_size} invalid for uncompressed data"
        )));
      }
    } else if uncompressed_size == 0 {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} is compressed but uncompressed_size is 0"
      )));
    } else {
      validate_declared_uncompressed_size(idx, uncompressed_size)?;
    }

    let section_end = section_offset
      .checked_add(section_length)
      .ok_or_else(|| KiteError::InvalidSnapshot(format!("Section {idx} size overflow")))?;

    if section_end > buffer.len() {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} exceeds snapshot size: {section_end} > {}",
        buffer.len()
      )));
    }

    if section_end > max_section_end {
      max_section_end = section_end;
    }

    ranges.push((section_offset, section_end, idx));

    let absolute_offset = section_offset.checked_add(base_offset).ok_or_else(|| {
      KiteError::InvalidSnapshot(format!("Section {idx} absolute offset overflow"))
    })?;
    absolute_offset
      .checked_add(section_length)
      .ok_or_else(|| KiteError::InvalidSnapshot(format!("Section {idx} absolute end overflow")))?;

    sections.push(SectionEntry {
      offset: u64::try_from(absolute_offset).map_err(|_| {
        KiteError::InvalidSnapshot(format!("Section {idx} absolute offset overflow"))
      })?,
      length: section_length as u64,
      compression,
      uncompressed_size,
    });
  }

  ranges.sort_by_key(|(start, _, _)| *start);
  let mut prev_end = None;
  for (start, end, idx) in ranges {
    if let Some(prev_end) = prev_end {
      if start < prev_end {
        return Err(KiteError::InvalidSnapshot(format!(
          "Section {idx} overlaps previous section ({start} < {prev_end})"
        )));
      }
    }
    prev_end = Some(end);
  }

  Ok(ParsedSections {
    sections,
    max_section_end,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::snapshot::writer::{build_snapshot_to_memory, SnapshotBuildInput};
  use crate::util::binary::{align_up, read_u32, write_u64};
  use std::collections::HashMap;

  fn build_empty_snapshot() -> Vec<u8> {
    build_snapshot_to_memory(SnapshotBuildInput {
      generation: 1,
      nodes: Vec::new(),
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
  fn test_parse_section_table_ok() {
    let buffer = build_empty_snapshot();
    let version = read_u32(&buffer, 4);
    let section_count = section_count_for_version(version);
    let parsed = parse_section_table(&buffer, version, 0).expect("expected value");
    assert_eq!(parsed.sections.len(), section_count);
    assert!(parsed.max_section_end >= SNAPSHOT_HEADER_SIZE);
  }

  #[test]
  fn test_parse_section_table_rejects_unaligned_offset() {
    let mut buffer = build_empty_snapshot();
    let version = read_u32(&buffer, 4);
    let section_count = section_count_for_version(version);
    let parsed = parse_section_table(&buffer, version, 0).expect("expected value");
    let (idx, section) = parsed
      .sections
      .iter()
      .enumerate()
      .find(|(_, entry)| entry.length > 0)
      .expect("section with data");

    let table_offset = SNAPSHOT_HEADER_SIZE + idx * SECTION_ENTRY_SIZE;
    let data_start = align_up(
      SNAPSHOT_HEADER_SIZE + section_count * SECTION_ENTRY_SIZE,
      SECTION_ALIGNMENT,
    );

    write_u64(&mut buffer, table_offset, (data_start + 1) as u64);

    let err = parse_section_table(&buffer, version, 0).unwrap_err();
    let message = format!("{err:?}");
    assert!(message.contains("aligned"));
    assert!(section.length > 0);
  }

  #[test]
  fn test_declared_size_policy_allows_large_64_bit_sections() {
    let large_size = (256 * 1024 * 1024 + 1) as u64;
    let result = validate_declared_uncompressed_size(0, large_size);

    if cfg!(target_pointer_width = "64") {
      assert!(result.is_ok());
    } else {
      assert!(result.is_err());
    }
  }
}
