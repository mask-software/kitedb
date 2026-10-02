//! Snapshot integrity checks.
//!
//! Ported from src/check/checker.ts
//!
//! Loading a snapshot checks what its accessors need to stay in bounds.
//! `check_snapshot` also checks the invariants the accessors silently rely
//! on: the sort orders binary searches assume, in/out edge reciprocity, the
//! node ID mapping, the key index, schema ID ranges and string encoding. It
//! never panics on a corrupt snapshot, and keeps at most
//! `MAX_REPORTED_MESSAGES` errors and warnings each.

use crate::core::snapshot::node_map::{self, NodeIdMapLayout};
use crate::core::snapshot::reader::SnapshotData;
use crate::types::{CheckResult, PhysNode, SectionId, SnapshotFlags, KEY_INDEX_ENTRY_SIZE};
use crate::util::binary::{read_i32_at, read_u32, read_u32_at, read_u64, read_u64_at};
use crate::util::hash::xxhash64;

/// Errors (and, separately, warnings) kept in a report; the rest are counted.
const MAX_REPORTED_MESSAGES: usize = 1000;

/// Collects errors and warnings, keeping at most `MAX_REPORTED_MESSAGES` of
/// each so a badly corrupted snapshot cannot exhaust memory.
#[derive(Default)]
struct Report {
  errors: Vec<String>,
  warnings: Vec<String>,
  dropped_errors: usize,
  dropped_warnings: usize,
}

impl Report {
  fn error(&mut self, message: String) {
    if self.errors.len() < MAX_REPORTED_MESSAGES {
      self.errors.push(message);
    } else {
      self.dropped_errors += 1;
    }
  }

  fn warning(&mut self, message: String) {
    if self.warnings.len() < MAX_REPORTED_MESSAGES {
      self.warnings.push(message);
    } else {
      self.dropped_warnings += 1;
    }
  }

  fn finish(mut self) -> CheckResult {
    if self.dropped_errors > 0 {
      self
        .errors
        .push(format!("... and {} more errors", self.dropped_errors));
    }
    if self.dropped_warnings > 0 {
      self
        .warnings
        .push(format!("... and {} more warnings", self.dropped_warnings));
    }
    CheckResult {
      valid: self.errors.is_empty(),
      errors: self.errors,
      warnings: self.warnings,
    }
  }
}

/// Fixed-size set of indices below a bound.
struct BitSet(Vec<u64>);

impl BitSet {
  fn new(len: usize) -> Self {
    Self(vec![0; len.div_ceil(64)])
  }

  /// Inserts `index`; returns false if it was already present.
  fn insert(&mut self, index: usize) -> bool {
    let (word, bit) = (index / 64, 1u64 << (index % 64));
    let fresh = self.0[word] & bit == 0;
    self.0[word] |= bit;
    fresh
  }

  fn contains(&self, index: usize) -> bool {
    self.0[index / 64] & (1u64 << (index % 64)) != 0
  }
}

/// Header counts, as usize.
struct Counts {
  nodes: usize,
  edges: usize,
  strings: usize,
  labels: u64,
  etypes: u64,
  propkeys: u64,
}

/// Bytes of section `id`, or an empty slice when it is absent.
fn bytes(snapshot: &SnapshotData, id: SectionId) -> &[u8] {
  snapshot.section(id).unwrap_or(&[])
}

/// Check all snapshot invariants
pub fn check_snapshot(snapshot: &SnapshotData) -> CheckResult {
  let mut report = Report::default();
  let header = &snapshot.header;
  let (Ok(nodes), Ok(edges), Ok(strings)) = (
    usize::try_from(header.num_nodes),
    usize::try_from(header.num_edges),
    usize::try_from(header.num_strings),
  ) else {
    report.error("header counts do not fit in usize".to_string());
    return report.finish();
  };
  let counts = Counts {
    nodes,
    edges,
    strings,
    labels: header.num_labels,
    etypes: header.num_etypes,
    propkeys: header.num_propkeys,
  };
  let has_in_edges = header.flags.contains(SnapshotFlags::HAS_IN_EDGES);

  let out_offsets = snapshot.section(SectionId::OutOffsets);
  let in_offsets = snapshot.section(SectionId::InOffsets);
  check_csr_offsets(&mut report, "out_offsets", out_offsets, &counts, true);
  check_csr_offsets(&mut report, "in_offsets", in_offsets, &counts, has_in_edges);

  check_edge_references(
    &mut report,
    "out_dst",
    bytes(snapshot, SectionId::OutDst),
    &counts,
  );
  if has_in_edges {
    check_edge_references(
      &mut report,
      "in_src",
      bytes(snapshot, SectionId::InSrc),
      &counts,
    );
  }

  check_mapping_bijection(
    &mut report,
    snapshot.section(SectionId::PhysToNodeId),
    snapshot.section(SectionId::NodeIdToPhys),
    snapshot.node_id_map_layout(),
    &counts,
    header.max_node_id,
  );

  let out = Adjacency {
    offsets: out_offsets.unwrap_or(&[]),
    etypes: bytes(snapshot, SectionId::OutEtype),
    others: bytes(snapshot, SectionId::OutDst),
  };
  check_adjacency_sorted(&mut report, "Out-edges", &out, &counts, Some("out-edge"));
  if has_in_edges {
    let incoming = Adjacency {
      offsets: in_offsets.unwrap_or(&[]),
      etypes: bytes(snapshot, SectionId::InEtype),
      others: bytes(snapshot, SectionId::InSrc),
    };
    // A duplicate edge is already reported once, as a duplicate out-edge.
    check_adjacency_sorted(&mut report, "In-edges", &incoming, &counts, None);
    check_edge_reciprocity(
      &mut report,
      &out,
      &incoming,
      bytes(snapshot, SectionId::InOutIndex),
      &counts,
    );
  }

  let strings_ok = check_string_table(&mut report, snapshot, counts.strings);
  check_schema_ids(&mut report, snapshot, &counts);
  check_key_index(&mut report, snapshot, &counts, strings_ok);

  report.finish()
}

fn check_csr_offsets(
  report: &mut Report,
  name: &str,
  offsets: Option<&[u8]>,
  counts: &Counts,
  required: bool,
) {
  let Some(offsets) = offsets else {
    if required {
      report.error(format!("{name} section missing"));
    }
    return;
  };

  if offsets.len() / 4 <= counts.nodes {
    report.error(format!("{name} section is too small"));
    return;
  }

  let mut prev = 0u32;
  for i in 0..=counts.nodes {
    let current = read_u32_at(offsets, i);
    if current < prev {
      report.error(format!(
        "{name} not monotonic at index {i}: {prev} -> {current}"
      ));
      break;
    }
    prev = current;
  }

  let last_offset = read_u32_at(offsets, counts.nodes);
  if last_offset as usize != counts.edges {
    report.error(format!(
      "{name} final value {last_offset} != numEdges {}",
      counts.edges
    ));
  }
}

fn check_edge_references(report: &mut Report, name: &str, data: &[u8], counts: &Counts) {
  if data.len() / 4 < counts.edges {
    report.error(format!("{name} section is too small"));
    return;
  }

  for i in 0..counts.edges {
    let value = read_u32_at(data, i);
    if value as usize >= counts.nodes {
      report.error(format!(
        "{name}[{i}] = {value} out of range [0, {})",
        counts.nodes
      ));
    }
  }
}

/// Checks both directions of the NodeID <-> phys mapping, in either
/// NodeIdToPhys layout.
fn check_mapping_bijection(
  report: &mut Report,
  phys_to_nodeid: Option<&[u8]>,
  nodeid_to_phys: Option<&[u8]>,
  layout: NodeIdMapLayout,
  counts: &Counts,
  max_node_id: u64,
) {
  if counts.nodes == 0 {
    return;
  }
  let (Some(phys_to_nodeid), Some(nodeid_to_phys)) = (phys_to_nodeid, nodeid_to_phys) else {
    report.error("node/phys mapping sections missing".to_string());
    return;
  };

  if phys_to_nodeid.len() / 8 < counts.nodes {
    report.error("phys_to_nodeid section is too small".to_string());
  }

  let phys_limit = std::cmp::min(counts.nodes, phys_to_nodeid.len() / 8);
  for phys in 0..phys_limit {
    let node_id = read_u64_at(phys_to_nodeid, phys);
    if node_id > max_node_id {
      report.error(format!(
        "phys_to_nodeid[{phys}] = {node_id} > maxNodeId {max_node_id}"
      ));
      continue;
    }

    let back_phys = match layout {
      NodeIdMapLayout::Dense => node_map::dense_lookup(nodeid_to_phys, node_id),
      NodeIdMapLayout::Sparse => node_map::sparse_lookup(nodeid_to_phys, node_id),
    };
    match back_phys {
      Some(back_phys) if back_phys as usize == phys => {}
      Some(back_phys) => report.error(format!(
        "Mapping mismatch: phys {phys} -> nodeId {node_id} -> phys {back_phys}"
      )),
      None => report.error(format!("nodeid_to_phys has no entry for nodeId {node_id}")),
    }
  }

  // Every mapped (nodeId, phys) pair must point back to its nodeId.
  let check_reverse = |node_id: u64, phys: PhysNode, report: &mut Report| {
    if phys as usize >= phys_limit {
      report.error(format!("nodeid_to_phys[{node_id}] = {phys} out of range"));
      return;
    }
    let back_node_id = read_u64_at(phys_to_nodeid, phys as usize);
    if back_node_id != node_id {
      report.error(format!(
        "Mapping mismatch: nodeId {node_id} -> phys {phys} -> nodeId {back_node_id}"
      ));
    }
  };

  match layout {
    NodeIdMapLayout::Dense => {
      let mapping_size = nodeid_to_phys.len() / node_map::DENSE_ENTRY_SIZE;
      for node_id in 0..mapping_size {
        let phys = read_i32_at(nodeid_to_phys, node_id);
        if phys == -1 {
          continue;
        }
        match PhysNode::try_from(phys) {
          Ok(phys) => check_reverse(node_id as u64, phys, report),
          Err(_) => report.error(format!("nodeid_to_phys[{node_id}] = {phys} out of range")),
        }
      }
    }
    NodeIdMapLayout::Sparse => {
      if nodeid_to_phys.len() % node_map::SPARSE_ENTRY_SIZE != 0 {
        report.error("nodeid_to_phys sparse map has a partial entry".to_string());
      }
      let mut previous: Option<u64> = None;
      for index in 0..node_map::sparse_len(nodeid_to_phys) {
        let (node_id, phys) = node_map::sparse_entry(nodeid_to_phys, index);
        if previous.is_some_and(|previous| previous >= node_id) {
          report.error(format!(
            "nodeid_to_phys sparse map not sorted at entry {index}: nodeId {node_id}"
          ));
        }
        previous = Some(node_id);
        check_reverse(node_id, phys, report);
      }
    }
  }
}

/// One direction of the CSR: per-node ranges of (etype, other node).
struct Adjacency<'a> {
  offsets: &'a [u8],
  etypes: &'a [u8],
  others: &'a [u8],
}

impl Adjacency<'_> {
  /// Whether the arrays are large enough for the header counts.
  fn fits(&self, counts: &Counts) -> bool {
    self.offsets.len() / 4 > counts.nodes
      && self.etypes.len() / 4 >= counts.edges
      && self.others.len() / 4 >= counts.edges
  }

  /// Edge range of `node`, clamped to the edge count.
  fn range(&self, node: usize, counts: &Counts) -> std::ops::Range<usize> {
    let start = (read_u32_at(self.offsets, node) as usize).min(counts.edges);
    let end = (read_u32_at(self.offsets, node + 1) as usize).min(counts.edges);
    start..end.max(start)
  }

  fn edge(&self, index: usize) -> (u32, u32) {
    (
      read_u32_at(self.etypes, index),
      read_u32_at(self.others, index),
    )
  }

  /// Node whose range holds edge `index` (offsets are monotonic).
  fn owner(&self, index: usize, counts: &Counts) -> usize {
    let (mut lo, mut hi) = (0usize, counts.nodes);
    while lo < hi {
      let mid = (lo + hi).div_ceil(2);
      if read_u32_at(self.offsets, mid) as usize <= index {
        lo = mid;
      } else {
        hi = mid - 1;
      }
    }
    lo
  }
}

/// Each node's edges must be sorted by (etype, other node): `has_edge` and
/// `find_edge_index` binary-search out-edges, and the writer emits in-edges
/// in the same order. Equal neighbours are duplicate edges, which the writer
/// keeps; they are warned about as `duplicate_name` when given.
fn check_adjacency_sorted(
  report: &mut Report,
  label: &str,
  adjacency: &Adjacency<'_>,
  counts: &Counts,
  duplicate_name: Option<&str>,
) {
  if !adjacency.fits(counts) {
    if counts.edges > 0 {
      report.error(format!("{label}: CSR sections are too small"));
    }
    return;
  }

  for node in 0..counts.nodes {
    let range = adjacency.range(node, counts);
    for i in range.start + 1..range.end {
      let previous = adjacency.edge(i - 1);
      let current = adjacency.edge(i);
      if previous > current {
        report.error(format!(
          "{label} not sorted for phys {node} at index {i}: ({},{}) > ({},{})",
          previous.0, previous.1, current.0, current.1
        ));
        break;
      }
      if let Some(name) = duplicate_name.filter(|_| previous == current) {
        report.warning(format!(
          "Duplicate {name} for phys {node}: ({},{})",
          current.0, current.1
        ));
      }
    }
  }
}

/// In-edges and out-edges must describe the same edges: each in-edge's
/// InOutIndex names a distinct out-edge with the same (src, etype, dst), and
/// every out-edge is named. Matching by index keeps duplicate edges apart and
/// costs O(E log N).
fn check_edge_reciprocity(
  report: &mut Report,
  out: &Adjacency<'_>,
  incoming: &Adjacency<'_>,
  in_out_index: &[u8],
  counts: &Counts,
) {
  if !out.fits(counts) || !incoming.fits(counts) || in_out_index.len() / 4 < counts.edges {
    if counts.edges > 0 {
      report.error("edge reciprocity: CSR sections are too small".to_string());
    }
    return;
  }

  let mut matched = BitSet::new(counts.edges);
  for dst in 0..counts.nodes {
    for in_idx in incoming.range(dst, counts) {
      let (etype, src) = incoming.edge(in_idx);
      let out_idx = read_u32_at(in_out_index, in_idx) as usize;
      if out_idx >= counts.edges {
        report.error(format!("in_out_index[{in_idx}] = {out_idx} out of range"));
        continue;
      }
      if !matched.insert(out_idx) {
        report.error(format!(
          "in_out_index mismatch: out[{out_idx}] is named by more than one in-edge (in[{in_idx}])"
        ));
        continue;
      }
      let out_src = out.owner(out_idx, counts);
      let (out_etype, out_dst) = out.edge(out_idx);
      if out_src != src as usize || out_etype != etype || out_dst as usize != dst {
        report.error(format!(
          "Reciprocity mismatch: in[{dst}] from {src} type {etype} -> out[{out_idx}] is ({out_src},{out_etype},{out_dst})"
        ));
      }
    }
  }

  for src in 0..counts.nodes {
    for out_idx in out.range(src, counts) {
      if !matched.contains(out_idx) {
        let (etype, dst) = out.edge(out_idx);
        report.error(format!(
          "Missing reciprocal in-edge: out[{src}] -({etype})-> [{dst}]"
        ));
      }
    }
  }
}

/// StringOffsets must stay within StringBytes, and every string must be valid
/// UTF-8. Returns whether the offsets are usable.
fn check_string_table(report: &mut Report, snapshot: &SnapshotData, num_strings: usize) -> bool {
  let Some(string_offsets) = snapshot.section(SectionId::StringOffsets) else {
    report.error("string_offsets section missing".to_string());
    return false;
  };
  let string_bytes = bytes(snapshot, SectionId::StringBytes);
  let offset_size = snapshot.string_offset_size();

  if string_offsets.len() / offset_size <= num_strings {
    report.error("string_offsets section is too small".to_string());
    return false;
  }

  let offset_at = |i: usize| {
    if offset_size == 8 {
      read_u64_at(string_offsets, i)
    } else {
      u64::from(read_u32_at(string_offsets, i))
    }
  };
  let string_bytes_len = string_bytes.len() as u64;
  let mut previous = 0u64;
  for i in 0..=num_strings {
    let offset = offset_at(i);
    if offset > string_bytes_len {
      report.error(format!(
        "string_offsets[{i}] = {offset} > string_bytes length {string_bytes_len}"
      ));
      return false;
    }
    if offset < previous {
      report.error(format!(
        "string_offsets not monotonic at index {i}: {previous} -> {offset}"
      ));
      return false;
    }
    previous = offset;
  }

  for i in 1..num_strings {
    // Offsets are monotonic and within string_bytes (checked above).
    let string = &string_bytes[offset_at(i) as usize..offset_at(i + 1) as usize];
    if let Err(error) = std::str::from_utf8(string) {
      report.error(format!("string {i} is not valid UTF-8: {error}"));
    }
  }
  true
}

/// Labels, edge types and property keys stored per node or edge must be
/// schema IDs: `1..=num_labels`, `1..=num_etypes`, `1..=num_propkeys`.
fn check_schema_ids(report: &mut Report, snapshot: &SnapshotData, counts: &Counts) {
  let mut check = |name: &str, kind: &str, data: &[u8], len: usize, max_id: u64| {
    let len = len.min(data.len() / 4);
    let mut outside = (0..len).filter(|&i| {
      let id = u64::from(read_u32_at(data, i));
      id == 0 || id > max_id
    });
    if let Some(first) = outside.next() {
      let total = 1 + outside.count();
      report.error(format!(
        "{name}: {total} entries are not {kind} IDs 1..={max_id} (first: {name}[{first}] = {})",
        read_u32_at(data, first)
      ));
    }
  };

  if snapshot
    .header
    .flags
    .contains(SnapshotFlags::HAS_NODE_LABELS)
  {
    let labels = bytes(snapshot, SectionId::NodeLabelIds);
    check(
      "NodeLabelIds",
      "label",
      labels,
      labels.len() / 4,
      counts.labels,
    );
  }
  check(
    "OutEtype",
    "edge type",
    bytes(snapshot, SectionId::OutEtype),
    counts.edges,
    counts.etypes,
  );
  for id in [SectionId::NodePropKeys, SectionId::EdgePropKeys] {
    let keys = bytes(snapshot, id);
    let name = if id == SectionId::NodePropKeys {
      "NodePropKeys"
    } else {
      "EdgePropKeys"
    };
    check(name, "property key", keys, keys.len() / 4, counts.propkeys);
  }
}

/// The key index must find every keyed node: entries sorted by (bucket,
/// hash), each in the bucket its hash selects, each hash the xxHash64 of its
/// key string, and KeyEntries and NodeKeyString describing the same
/// (key, node) pairs.
fn check_key_index(
  report: &mut Report,
  snapshot: &SnapshotData,
  counts: &Counts,
  strings_ok: bool,
) {
  let entries = bytes(snapshot, SectionId::KeyEntries);
  let num_entries = entries.len() / KEY_INDEX_ENTRY_SIZE;
  let key_buckets = snapshot.section(SectionId::KeyBuckets);

  check_key_index_ordering(report, entries, key_buckets);
  if let Some(buckets) = key_buckets {
    check_key_buckets(report, entries, buckets);
  }

  let node_keys = bytes(snapshot, SectionId::NodeKeyString);
  if node_keys.len() / 4 < counts.nodes {
    report.error("NodeKeyString section is too small".to_string());
    return;
  }
  let mut indexed = BitSet::new(counts.nodes);
  for index in 0..num_entries {
    let offset = index * KEY_INDEX_ENTRY_SIZE;
    let hash = read_u64(entries, offset);
    let string_id = read_u32(entries, offset + 8);
    let node_id = read_u64(entries, offset + 16);

    if string_id == 0 {
      report.error(format!("key entry {index} has no key string"));
    } else if strings_ok {
      match snapshot.string_bytes(string_id) {
        Some(key) if xxhash64(key) == hash => {}
        Some(key) => report.error(format!(
          "key entry {index}: hash {hash} != xxhash64 of its key ({})",
          xxhash64(key)
        )),
        None => report.error(format!(
          "key entry {index}: string ID {string_id} is not in the string table"
        )),
      }
    }

    let Some(phys) = snapshot.phys_node(node_id) else {
      report.error(format!(
        "key entry {index}: node ID {node_id} is not in the snapshot"
      ));
      continue;
    };
    let phys = phys as usize;
    let node_key = read_u32_at(node_keys, phys);
    if node_key != string_id {
      report.error(format!(
        "key entry {index} maps key string {string_id} to node {node_id}, whose NodeKeyString is {node_key}"
      ));
    }
    if !indexed.insert(phys) {
      report.error(format!(
        "key entry {index}: node {node_id} has more than one key entry"
      ));
    }
  }

  let mut unindexed =
    (0..counts.nodes).filter(|&phys| read_u32_at(node_keys, phys) != 0 && !indexed.contains(phys));
  if let Some(first) = unindexed.next() {
    let total = 1 + unindexed.count();
    report.error(format!(
      "{total} keyed nodes have no KeyEntries entry (first: phys {first}, key string {})",
      read_u32_at(node_keys, first)
    ));
  }
}

fn check_key_index_ordering(report: &mut Report, key_entries: &[u8], key_buckets: Option<&[u8]>) {
  let num_key_entries = key_entries.len() / KEY_INDEX_ENTRY_SIZE;
  let num_buckets = key_buckets
    .map(|b| (b.len() / 4).saturating_sub(1) as u64)
    .unwrap_or(0);

  for i in 1..num_key_entries {
    let prev_offset = (i - 1) * KEY_INDEX_ENTRY_SIZE;
    let curr_offset = i * KEY_INDEX_ENTRY_SIZE;

    let prev_hash = read_u64(key_entries, prev_offset);
    let curr_hash = read_u64(key_entries, curr_offset);

    if num_buckets > 0 {
      let prev_bucket = prev_hash % num_buckets;
      let curr_bucket = curr_hash % num_buckets;

      if prev_bucket > curr_bucket {
        report.error(format!(
          "Key index not sorted by bucket at index {i}: bucket {prev_bucket} > {curr_bucket}"
        ));
        break;
      }

      if prev_bucket < curr_bucket {
        continue;
      }
    }

    if prev_hash > curr_hash {
      report.error(format!(
        "Key index not sorted by hash at index {i}: {prev_hash} > {curr_hash}"
      ));
      break;
    }

    if prev_hash == curr_hash {
      let prev_string_id = read_u32(key_entries, prev_offset + 8);
      let curr_string_id = read_u32(key_entries, curr_offset + 8);

      if prev_string_id > curr_string_id {
        report.error(format!("Key index not sorted by stringId at index {i}"));
        break;
      }

      if prev_string_id == curr_string_id {
        let prev_node_id = read_u64(key_entries, prev_offset + 16);
        let curr_node_id = read_u64(key_entries, curr_offset + 16);

        if prev_node_id >= curr_node_id {
          report.error(format!("Key index not sorted by nodeId at index {i}"));
          break;
        }
      }
    }
  }
}

/// KeyBuckets offsets must cover the entries, and bucket `b` must hold
/// exactly the entries whose hash selects `b`: lookup_by_key reads only that
/// range.
fn check_key_buckets(report: &mut Report, key_entries: &[u8], buckets: &[u8]) {
  let num_entries = key_entries.len() / KEY_INDEX_ENTRY_SIZE;
  let num_offsets = buckets.len() / 4;
  if num_offsets < 2 {
    report.error("key_buckets section has fewer than two offsets".to_string());
    return;
  }
  let num_buckets = num_offsets - 1;
  if read_u32_at(buckets, 0) != 0 || read_u32_at(buckets, num_buckets) as usize != num_entries {
    report.error(format!(
      "key_buckets offsets do not cover the {num_entries} key entries"
    ));
    return;
  }

  for bucket in 0..num_buckets {
    let start = read_u32_at(buckets, bucket) as usize;
    let end = (read_u32_at(buckets, bucket + 1) as usize).min(num_entries);
    for index in start..end {
      let hash = read_u64(key_entries, index * KEY_INDEX_ENTRY_SIZE);
      let expected = hash % num_buckets as u64;
      if expected != bucket as u64 {
        report.error(format!(
          "key entry {index} (hash {hash}) is in bucket {bucket}, but its hash selects bucket {expected}"
        ));
      }
    }
  }
}

/// Quick validation (just CRC and basic structure)
pub fn quick_check(snapshot: &SnapshotData) -> bool {
  let (Ok(num_nodes), Ok(num_edges)) = (
    usize::try_from(snapshot.header.num_nodes),
    usize::try_from(snapshot.header.num_edges),
  ) else {
    return false;
  };
  let last_offset_is_edge_count = |offsets: &[u8]| {
    offsets.len() / 4 > num_nodes && read_u32_at(offsets, num_nodes) as usize == num_edges
  };

  let Some(out_offsets) = snapshot.section(SectionId::OutOffsets) else {
    return false;
  };
  if !last_offset_is_edge_count(out_offsets) {
    return false;
  }
  snapshot
    .section(SectionId::InOffsets)
    .is_none_or(last_offset_is_edge_count)
}
