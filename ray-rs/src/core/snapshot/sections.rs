//! Snapshot section parsing
//!
//! Section definitions and parsing helpers

use crate::constants::SECTION_ALIGNMENT;
use crate::error::{KiteError, Result};
use crate::types::{SectionEntry, SectionId, SECTION_ENTRY_SIZE, SNAPSHOT_HEADER_SIZE};
use crate::util::binary::{align_up, read_u32, read_u64};
use crate::util::compression::CompressionType;

/// Parsed section table metadata
#[derive(Debug, Clone)]
pub struct ParsedSections {
  pub sections: Vec<SectionEntry>,
  pub max_section_end: usize,
}

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

/// Parse and validate the snapshot section table.
///
/// `buffer` is the snapshot slice starting at the header.
/// `base_offset` is the absolute file offset of the snapshot start (0 for standalone snapshots).
pub fn parse_section_table(
  buffer: &[u8],
  section_count: usize,
  base_offset: usize,
) -> Result<ParsedSections> {
  let section_table_size = section_count * SECTION_ENTRY_SIZE;
  let table_end = SNAPSHOT_HEADER_SIZE + section_table_size;

  if buffer.len() < table_end {
    return Err(KiteError::InvalidSnapshot(format!(
      "Snapshot too small for section table: {} bytes",
      buffer.len()
    )));
  }

  let data_start = align_up(table_end, SECTION_ALIGNMENT);
  let mut sections = Vec::with_capacity(section_count);
  let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
  let mut max_section_end = table_end;

  let mut offset = SNAPSHOT_HEADER_SIZE;
  for idx in 0..section_count {
    let section_offset = read_u64(buffer, offset) as usize;
    let section_length = read_u64(buffer, offset + 8) as usize;
    let compression = read_u32(buffer, offset + 16);
    let uncompressed_size = read_u32(buffer, offset + 20);
    offset += SECTION_ENTRY_SIZE;

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
      if uncompressed_size != 0 && uncompressed_size != section_length as u32 {
        return Err(KiteError::InvalidSnapshot(format!(
          "Section {idx} uncompressed_size {uncompressed_size} invalid for uncompressed data"
        )));
      }
    } else if uncompressed_size == 0 {
      return Err(KiteError::InvalidSnapshot(format!(
        "Section {idx} is compressed but uncompressed_size is 0"
      )));
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

    sections.push(SectionEntry {
      offset: (section_offset + base_offset) as u64,
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
  use crate::util::binary::{read_u32, write_u64};
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
    let parsed = parse_section_table(&buffer, section_count, 0).expect("expected value");
    assert_eq!(parsed.sections.len(), section_count);
    assert!(parsed.max_section_end >= SNAPSHOT_HEADER_SIZE);
  }

  #[test]
  fn test_parse_section_table_rejects_unaligned_offset() {
    let mut buffer = build_empty_snapshot();
    let version = read_u32(&buffer, 4);
    let section_count = section_count_for_version(version);
    let parsed = parse_section_table(&buffer, section_count, 0).expect("expected value");
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

    let err = parse_section_table(&buffer, section_count, 0).unwrap_err();
    let message = format!("{err:?}");
    assert!(message.contains("aligned"));
    assert!(section.length > 0);
  }
}
