//! Snapshot integrity checks.
//!
//! Ported from src/check/checker.ts

use std::borrow::Cow;

use crate::core::snapshot::reader::SnapshotData;
use crate::types::{CheckResult, SectionId, KEY_INDEX_ENTRY_SIZE};
use crate::util::binary::{read_i32_at, read_u32, read_u32_at, read_u64, read_u64_at};

/// Check all snapshot invariants
pub fn check_snapshot(snapshot: &SnapshotData) -> CheckResult {
  let mut errors = Vec::new();
  let mut warnings = Vec::new();

  let num_nodes = match usize::try_from(snapshot.header.num_nodes) {
    Ok(v) => v,
    Err(_) => {
      errors.push("num_nodes overflow".to_string());
      return CheckResult {
        valid: false,
        errors,
        warnings,
      };
    }
  };
  let num_edges = match usize::try_from(snapshot.header.num_edges) {
    Ok(v) => v,
    Err(_) => {
      errors.push("num_edges overflow".to_string());
      return CheckResult {
        valid: false,
        errors,
        warnings,
      };
    }
  };
  let max_node_id = snapshot.header.max_node_id;

  let out_offsets = section_data(snapshot, SectionId::OutOffsets);
  let out_dst = section_data(snapshot, SectionId::OutDst);
  let out_etype = section_data(snapshot, SectionId::OutEtype);
  let in_offsets = section_data(snapshot, SectionId::InOffsets);
  let in_src = section_data(snapshot, SectionId::InSrc);
  let in_etype = section_data(snapshot, SectionId::InEtype);
  let in_out_index = section_data(snapshot, SectionId::InOutIndex);
  let phys_to_nodeid = section_data(snapshot, SectionId::PhysToNodeId);
  let nodeid_to_phys = section_data(snapshot, SectionId::NodeIdToPhys);
  let key_entries = section_data(snapshot, SectionId::KeyEntries);
  let key_buckets = section_data(snapshot, SectionId::KeyBuckets);
  let string_offsets = section_data(snapshot, SectionId::StringOffsets);
  let string_bytes = section_data(snapshot, SectionId::StringBytes);

  check_csr_offsets(
    "out_offsets",
    out_offsets.as_deref(),
    num_nodes,
    num_edges,
    true,
    &mut errors,
  );
  check_csr_offsets(
    "in_offsets",
    in_offsets.as_deref(),
    num_nodes,
    num_edges,
    false,
    &mut errors,
  );

  check_edge_references(
    "out_dst",
    out_dst.as_deref(),
    num_edges,
    num_nodes,
    &mut errors,
  );
  check_edge_references(
    "in_src",
    in_src.as_deref(),
    num_edges,
    num_nodes,
    &mut errors,
  );

  check_mapping_bijection(
    phys_to_nodeid.as_deref(),
    nodeid_to_phys.as_deref(),
    num_nodes,
    max_node_id,
    &mut errors,
  );

  check_out_edge_sorting(
    out_offsets.as_deref(),
    out_etype.as_deref(),
    out_dst.as_deref(),
    num_nodes,
    num_edges,
    &mut errors,
    &mut warnings,
  );

  let reciprocity_sections = EdgeReciprocitySections {
    out_offsets: out_offsets.as_deref(),
    out_etype: out_etype.as_deref(),
    out_dst: out_dst.as_deref(),
    in_offsets: in_offsets.as_deref(),
    in_src: in_src.as_deref(),
    in_etype: in_etype.as_deref(),
    in_out_index: in_out_index.as_deref(),
  };
  check_edge_reciprocity(&reciprocity_sections, num_nodes, num_edges, &mut errors);

  let key_entries = match key_entries {
    Some(entries) => entries,
    None => {
      return CheckResult {
        valid: errors.is_empty(),
        errors,
        warnings,
      };
    }
  };

  check_key_index_ordering(key_entries.as_ref(), key_buckets.as_deref(), &mut errors);

  let num_strings = match usize::try_from(snapshot.header.num_strings) {
    Ok(v) => v,
    Err(_) => {
      errors.push("num_strings overflow".to_string());
      return CheckResult {
        valid: false,
        errors,
        warnings,
      };
    }
  };

  check_string_table_bounds(
    string_offsets.as_deref(),
    string_bytes.as_deref(),
    num_strings,
    &mut errors,
  );

  CheckResult {
    valid: errors.is_empty(),
    errors,
    warnings,
  }
}

#[inline]
fn check_csr_offsets(
  name: &str,
  offsets: Option<&[u8]>,
  num_nodes: usize,
  num_edges: usize,
  required: bool,
  errors: &mut Vec<String>,
) {
  let Some(offsets) = offsets else {
    if required {
      errors.push(format!("{name} section missing"));
    }
    return;
  };

  if offsets.len() < (num_nodes + 1) * 4 {
    errors.push(format!("{name} section is too small"));
    return;
  }

  let mut prev = 0u32;
  for i in 0..=num_nodes {
    let current = read_u32_at(offsets, i);
    if current < prev {
      errors.push(format!(
        "{name} not monotonic at index {i}: {prev} -> {current}"
      ));
      break;
    }
    prev = current;
  }

  let last_offset = read_u32_at(offsets, num_nodes);
  if last_offset as usize != num_edges {
    errors.push(format!(
      "{name} final value {last_offset} != numEdges {num_edges}"
    ));
  }
}

#[inline]
fn check_edge_references(
  name: &str,
  data: Option<&[u8]>,
  num_edges: usize,
  num_nodes: usize,
  errors: &mut Vec<String>,
) {
  let Some(data) = data else {
    return;
  };

  if data.len() < num_edges * 4 {
    errors.push(format!("{name} section is too small"));
    return;
  }

  for i in 0..num_edges {
    let value = read_u32_at(data, i);
    if value as usize >= num_nodes {
      errors.push(format!(
        "{name}[{i}] = {value} out of range [0, {num_nodes})"
      ));
    }
  }
}

#[inline]
fn check_mapping_bijection(
  phys_to_nodeid: Option<&[u8]>,
  nodeid_to_phys: Option<&[u8]>,
  num_nodes: usize,
  max_node_id: u64,
  errors: &mut Vec<String>,
) {
  let (Some(phys_to_nodeid), Some(nodeid_to_phys)) = (phys_to_nodeid, nodeid_to_phys) else {
    errors.push("node/phys mapping sections missing".to_string());
    return;
  };

  if phys_to_nodeid.len() < num_nodes * 8 {
    errors.push("phys_to_nodeid section is too small".to_string());
  }

  let phys_limit = std::cmp::min(num_nodes, phys_to_nodeid.len() / 8);
  for phys in 0..phys_limit {
    let node_id = read_u64_at(phys_to_nodeid, phys);
    if node_id > max_node_id {
      errors.push(format!(
        "phys_to_nodeid[{phys}] = {node_id} > maxNodeId {max_node_id}"
      ));
      continue;
    }

    let node_id_idx = node_id as usize;
    if node_id_idx * 4 + 4 > nodeid_to_phys.len() {
      errors.push(format!("nodeid_to_phys out of range for nodeId {node_id}"));
      continue;
    }
    let back_phys = read_i32_at(nodeid_to_phys, node_id_idx);
    if back_phys != phys as i32 {
      errors.push(format!(
        "Mapping mismatch: phys {phys} -> nodeId {node_id} -> phys {back_phys}"
      ));
    }
  }

  let mapping_size = nodeid_to_phys.len() / 4;
  for node_id in 0..mapping_size {
    let phys = read_i32_at(nodeid_to_phys, node_id);
    if phys == -1 {
      continue;
    }

    if phys < 0 || phys as usize >= num_nodes {
      errors.push(format!("nodeid_to_phys[{node_id}] = {phys} out of range"));
      continue;
    }

    let back_node_id = read_u64_at(phys_to_nodeid, phys as usize);
    if back_node_id != node_id as u64 {
      errors.push(format!(
        "Mapping mismatch: nodeId {node_id} -> phys {phys} -> nodeId {back_node_id}"
      ));
    }
  }
}

#[inline]
fn check_out_edge_sorting(
  out_offsets: Option<&[u8]>,
  out_etype: Option<&[u8]>,
  out_dst: Option<&[u8]>,
  num_nodes: usize,
  num_edges: usize,
  errors: &mut Vec<String>,
  warnings: &mut Vec<String>,
) {
  let (Some(out_offsets), Some(out_etype), Some(out_dst)) = (out_offsets, out_etype, out_dst)
  else {
    return;
  };

  if out_etype.len() < num_edges * 4 || out_dst.len() < num_edges * 4 {
    errors.push("out_etype/out_dst sections are too small".to_string());
    return;
  }

  for phys in 0..num_nodes {
    let start = read_u32_at(out_offsets, phys) as usize;
    let end = read_u32_at(out_offsets, phys + 1) as usize;

    for i in (start + 1)..end {
      let prev_etype = read_u32_at(out_etype, i - 1);
      let prev_dst = read_u32_at(out_dst, i - 1);
      let curr_etype = read_u32_at(out_etype, i);
      let curr_dst = read_u32_at(out_dst, i);

      let cmp = if prev_etype < curr_etype {
        -1
      } else if prev_etype > curr_etype {
        1
      } else if prev_dst < curr_dst {
        -1
      } else if prev_dst > curr_dst {
        1
      } else {
        0
      };

      if cmp > 0 {
        errors.push(format!(
          "Out-edges not sorted for phys {phys} at index {i}: ({prev_etype},{prev_dst}) > ({curr_etype},{curr_dst})"
        ));
        break;
      }
      if cmp == 0 {
        warnings.push(format!(
          "Duplicate out-edge for phys {phys}: ({curr_etype},{curr_dst})"
        ));
      }
    }
  }
}

struct EdgeReciprocitySections<'a> {
  out_offsets: Option<&'a [u8]>,
  out_etype: Option<&'a [u8]>,
  out_dst: Option<&'a [u8]>,
  in_offsets: Option<&'a [u8]>,
  in_src: Option<&'a [u8]>,
  in_etype: Option<&'a [u8]>,
  in_out_index: Option<&'a [u8]>,
}

#[inline]
fn check_edge_reciprocity(
  sections: &EdgeReciprocitySections<'_>,
  num_nodes: usize,
  num_edges: usize,
  errors: &mut Vec<String>,
) {
  let (
    Some(out_offsets),
    Some(out_etype),
    Some(out_dst),
    Some(in_offsets),
    Some(in_src),
    Some(in_etype),
    Some(in_out_index),
  ) = (
    sections.out_offsets,
    sections.out_etype,
    sections.out_dst,
    sections.in_offsets,
    sections.in_src,
    sections.in_etype,
    sections.in_out_index,
  )
  else {
    return;
  };

  if !(out_offsets.len() >= (num_nodes + 1) * 4
    && in_offsets.len() >= (num_nodes + 1) * 4
    && out_etype.len() >= num_edges * 4
    && out_dst.len() >= num_edges * 4
    && in_src.len() >= num_edges * 4
    && in_etype.len() >= num_edges * 4
    && in_out_index.len() >= num_edges * 4)
  {
    return;
  }

  for src_phys in 0..num_nodes {
    let out_start = read_u32_at(out_offsets, src_phys) as usize;
    let out_end = read_u32_at(out_offsets, src_phys + 1) as usize;

    for out_idx in out_start..out_end {
      let dst_phys = read_u32_at(out_dst, out_idx) as usize;
      let etype = read_u32_at(out_etype, out_idx);
      if dst_phys >= num_nodes {
        continue;
      }

      let in_start = read_u32_at(in_offsets, dst_phys) as usize;
      let in_end = read_u32_at(in_offsets, dst_phys + 1) as usize;

      let mut found = false;
      for in_idx in in_start..in_end {
        let in_src_phys = read_u32_at(in_src, in_idx) as usize;
        let in_etype_val = read_u32_at(in_etype, in_idx);
        let in_out_idx = read_u32_at(in_out_index, in_idx) as usize;

        if in_src_phys == src_phys && in_etype_val == etype {
          found = true;
          if in_out_idx != out_idx {
            errors.push(format!(
              "in_out_index mismatch: out[{out_idx}] -> in_out_index = {in_out_idx}"
            ));
          }
          break;
        }
      }

      if !found {
        errors.push(format!(
          "Missing reciprocal in-edge: out[{src_phys}] -({etype})-> [{dst_phys}]"
        ));
      }
    }
  }

  for dst_phys in 0..num_nodes {
    let in_start = read_u32_at(in_offsets, dst_phys) as usize;
    let in_end = read_u32_at(in_offsets, dst_phys + 1) as usize;

    for in_idx in in_start..in_end {
      let src_phys = read_u32_at(in_src, in_idx) as usize;
      let etype = read_u32_at(in_etype, in_idx);
      let out_idx = read_u32_at(in_out_index, in_idx) as usize;
      if src_phys >= num_nodes {
        continue;
      }

      if out_idx >= num_edges {
        errors.push(format!("in_out_index[{in_idx}] = {out_idx} out of range"));
        continue;
      }

      let out_src = find_node_for_edge_index(out_offsets, num_nodes, out_idx as u32);
      let out_dst_phys = read_u32_at(out_dst, out_idx) as usize;
      let out_etype_val = read_u32_at(out_etype, out_idx);

      if out_src != src_phys || out_dst_phys != dst_phys || out_etype_val != etype {
        errors.push(format!(
          "Reciprocity mismatch: in[{dst_phys}] from {src_phys} type {etype} -> out[{out_idx}] is ({out_src},{out_etype_val},{out_dst_phys})"
        ));
      }
    }
  }
}

#[inline]
fn check_key_index_ordering(
  key_entries: &[u8],
  key_buckets: Option<&[u8]>,
  errors: &mut Vec<String>,
) {
  let num_key_entries = key_entries.len() / KEY_INDEX_ENTRY_SIZE;
  let has_key_buckets = key_buckets.map(|b| b.len() > 4).unwrap_or(false);
  let num_buckets = if has_key_buckets {
    key_buckets
      .map(|b| (b.len() / 4).saturating_sub(1) as u64)
      .unwrap_or(0)
  } else {
    0
  };

  for i in 1..num_key_entries {
    let prev_offset = (i - 1) * KEY_INDEX_ENTRY_SIZE;
    let curr_offset = i * KEY_INDEX_ENTRY_SIZE;

    let prev_hash = read_u64(key_entries, prev_offset);
    let curr_hash = read_u64(key_entries, curr_offset);

    if has_key_buckets && num_buckets > 0 {
      let prev_bucket = prev_hash % num_buckets;
      let curr_bucket = curr_hash % num_buckets;

      if prev_bucket > curr_bucket {
        errors.push(format!(
          "Key index not sorted by bucket at index {i}: bucket {prev_bucket} > {curr_bucket}"
        ));
        break;
      }

      if prev_bucket < curr_bucket {
        continue;
      }
    }

    if prev_hash > curr_hash {
      errors.push(format!(
        "Key index not sorted by hash at index {i}: {prev_hash} > {curr_hash}"
      ));
      break;
    }

    if prev_hash == curr_hash {
      let prev_string_id = read_u32(key_entries, prev_offset + 8);
      let curr_string_id = read_u32(key_entries, curr_offset + 8);

      if prev_string_id > curr_string_id {
        errors.push(format!("Key index not sorted by stringId at index {i}"));
        break;
      }

      if prev_string_id == curr_string_id {
        let prev_node_id = read_u64(key_entries, prev_offset + 16);
        let curr_node_id = read_u64(key_entries, curr_offset + 16);

        if prev_node_id >= curr_node_id {
          errors.push(format!("Key index not sorted by nodeId at index {i}"));
          break;
        }
      }
    }
  }
}

#[inline]
fn check_string_table_bounds(
  string_offsets: Option<&[u8]>,
  string_bytes: Option<&[u8]>,
  num_strings: usize,
  errors: &mut Vec<String>,
) {
  let (Some(string_offsets), Some(string_bytes)) = (string_offsets, string_bytes) else {
    return;
  };

  if string_offsets.len() < (num_strings + 1) * 4 {
    errors.push("string_offsets section is too small".to_string());
    return;
  }

  let string_bytes_len = string_bytes.len();
  for i in 0..=num_strings {
    let offset = read_u32_at(string_offsets, i) as usize;
    if offset > string_bytes_len {
      errors.push(format!(
        "string_offsets[{i}] = {offset} > string_bytes length {string_bytes_len}"
      ));
      break;
    }
  }
}

/// Quick validation (just CRC and basic structure)
pub fn quick_check(snapshot: &SnapshotData) -> bool {
  let num_nodes = match usize::try_from(snapshot.header.num_nodes) {
    Ok(v) => v,
    Err(_) => return false,
  };
  let num_edges = match usize::try_from(snapshot.header.num_edges) {
    Ok(v) => v,
    Err(_) => return false,
  };

  let out_offsets = match section_data(snapshot, SectionId::OutOffsets) {
    Some(data) => data,
    None => return false,
  };
  if out_offsets.len() < (num_nodes + 1) * 4 {
    return false;
  }
  let last_out_offset = read_u32_at(out_offsets.as_ref(), num_nodes) as usize;
  if last_out_offset != num_edges {
    return false;
  }

  if let Some(in_offsets) = section_data(snapshot, SectionId::InOffsets) {
    if in_offsets.len() < (num_nodes + 1) * 4 {
      return false;
    }
    let last_in_offset = read_u32_at(in_offsets.as_ref(), num_nodes) as usize;
    if last_in_offset != num_edges {
      return false;
    }
  }

  true
}

fn section_data(snapshot: &SnapshotData, id: SectionId) -> Option<Cow<'_, [u8]>> {
  snapshot
    .section_slice(id)
    .map(Cow::Borrowed)
    .or_else(|| snapshot.section_bytes(id).map(Cow::Owned))
}

fn find_node_for_edge_index(out_offsets: &[u8], num_nodes: usize, edge_idx: u32) -> usize {
  let mut lo = 0usize;
  let mut hi = num_nodes;
  let target = edge_idx as usize;

  while lo < hi {
    let mid = (lo + hi).div_ceil(2);
    let offset = read_u32_at(out_offsets, mid) as usize;
    if offset <= target {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }

  lo
}
