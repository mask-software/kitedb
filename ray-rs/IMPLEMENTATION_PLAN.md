# RayDB Rust Implementation Plan

A comprehensive plan to port RayDB (TypeScript/Bun embedded graph database) to Rust, maintaining feature parity while leveraging Rust's strengths.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Project Structure](#project-structure)
3. [Phase 1: Core Foundation](#phase-1-core-foundation)
4. [Phase 2: Storage Layer](#phase-2-storage-layer)
5. [Phase 3: Graph Operations](#phase-3-graph-operations)
6. [Phase 4: MVCC](#phase-4-mvcc)
7. [Phase 5: Vector Embeddings](#phase-5-vector-embeddings)
8. [Phase 6: High-Level API](#phase-6-high-level-api)
9. [Phase 7: Testing & Benchmarks](#phase-7-testing--benchmarks)
10. [Dependencies](#dependencies)
11. [Binary Compatibility](#binary-compatibility)
12. [Phase 8: Language Bindings](#phase-8-language-bindings)

---

## Architecture Overview

RayDB uses a **Snapshot + Delta + WAL** architecture:

```
┌─────────────────────────────────────────────────────────────────┐
│                        GraphDB Handle                           │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────────┐  │
│  │   Snapshot   │    │    Delta     │    │    WAL Buffer    │  │
│  │   (mmap'd)   │    │  (in-memory) │    │   (circular)     │  │
│  │              │    │              │    │                  │  │
│  │  CSR Format  │ +  │  Pending     │ →  │  Durability      │  │
│  │  Zero-copy   │    │  Changes     │    │  Crash Recovery  │  │
│  └──────────────┘    └──────────────┘    └──────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions for Rust

1. **Memory-mapped I/O**: Use `memmap2` crate for zero-copy snapshot reading
2. **Zero-copy parsing**: Use `zerocopy` or raw pointer arithmetic for CSR access
3. **Concurrency**: Use `parking_lot` for faster mutexes, `crossbeam` for channels
4. **SIMD**: Use `std::simd` (nightly) or `simdeez`/`wide` for vector operations
5. **Async**: Optional `tokio` support for async I/O, but core is sync
6. **Error handling**: Use `thiserror` for error types

---

## Project Structure

```
ray-rs/
├── Cargo.toml
├── src/
│   ├── lib.rs                 # Public API exports
│   ├── types.rs               # Core type definitions
│   ├── constants.rs           # Magic numbers, thresholds
│   ├── error.rs               # Error types
│   │
│   ├── core/                  # Low-level storage
│   │   ├── mod.rs
│   │   ├── snapshot/
│   │   │   ├── mod.rs
│   │   │   ├── reader.rs      # CSR snapshot reading (mmap)
│   │   │   ├── writer.rs      # CSR snapshot building
│   │   │   └── sections.rs    # Section parsing
│   │   ├── delta.rs           # In-memory delta overlay
│   │   ├── wal/
│   │   │   ├── mod.rs
│   │   │   ├── record.rs      # WAL record types
│   │   │   ├── writer.rs      # WAL writing
│   │   │   ├── reader.rs      # WAL parsing/recovery
│   │   │   └── buffer.rs      # Circular WAL buffer
│   │   ├── header.rs          # Single-file header
│   │   ├── pager.rs           # Page-based I/O
│   │   ├── manifest.rs        # Multi-file manifest
│   │   └── compactor.rs       # Compaction logic
│   │
│   ├── graph/                 # Graph database operations
│   │   ├── mod.rs
│   │   ├── db.rs              # GraphDB struct & lifecycle
│   │   ├── tx.rs              # Transaction handling
│   │   ├── nodes.rs           # Node CRUD
│   │   ├── edges.rs           # Edge CRUD
│   │   ├── definitions.rs     # Schema definitions
│   │   ├── key_index.rs       # Key lookup
│   │   ├── iterators.rs       # Traversal iterators
│   │   └── checkpoint.rs      # Background checkpointing
│   │
│   ├── mvcc/                  # Multi-Version Concurrency Control
│   │   ├── mod.rs
│   │   ├── tx_manager.rs      # Transaction lifecycle
│   │   ├── version_chain.rs   # Version chain storage
│   │   ├── visibility.rs      # Snapshot isolation
│   │   ├── conflict.rs        # Write-write conflict detection
│   │   └── gc.rs              # Garbage collection
│   │
│   ├── vector/                # Vector embeddings
│   │   ├── mod.rs
│   │   ├── types.rs           # Vector types & config
│   │   ├── store.rs           # Columnar vector store
│   │   ├── fragment.rs        # Fragment management
│   │   ├── row_group.rs       # Row group operations
│   │   ├── distance.rs        # Distance functions (SIMD)
│   │   ├── normalize.rs       # Vector normalization
│   │   ├── ivf/
│   │   │   ├── mod.rs
│   │   │   ├── index.rs       # IVF index
│   │   │   ├── kmeans.rs      # K-means clustering
│   │   │   └── serialize.rs   # IVF serialization
│   │   ├── pq.rs              # Product Quantization
│   │   └── compaction.rs      # Vector compaction
│   │
│   ├── cache/                 # Caching layer
│   │   ├── mod.rs
│   │   ├── lru.rs             # LRU cache
│   │   ├── property.rs        # Property cache
│   │   ├── traversal.rs       # Traversal cache
│   │   └── query.rs           # Query cache
│   │
│   ├── api/                   # High-level API
│   │   ├── mod.rs
│   │   ├── ray.rs             # Ray struct (main entry)
│   │   ├── schema.rs          # Schema definitions
│   │   ├── builders.rs        # Query builders
│   │   ├── traversal.rs       # Traversal API
│   │   └── pathfinding.rs     # Dijkstra/A*
│   │
│   └── util/                  # Utilities
│       ├── mod.rs
│       ├── binary.rs          # Binary encoding/decoding
│       ├── crc.rs             # CRC32C (use crc32fast)
│       ├── hash.rs            # xxHash64
│       ├── compression.rs     # zstd/gzip compression
│       ├── lock.rs            # File locking
│       └── heap.rs            # Min-heap for pathfinding
│
├── benches/                   # Benchmarks
│   ├── snapshot.rs
│   ├── traversal.rs
│   ├── mvcc.rs
│   └── vector.rs
│
└── tests/                     # Integration tests
    ├── snapshot.rs
    ├── wal.rs
    ├── mvcc.rs
    ├── vector.rs
    └── api.rs
```

---

## Phase 1: Core Foundation

### 1.1 Types (`src/types.rs`)

```rust
//! Core type definitions for RayDB

/// Monotonic node ID, never reused (safe integer up to 2^53-1)
pub type NodeId = u64;

/// Physical node index in snapshot arrays (0..num_nodes-1)
pub type PhysNode = u32;

/// Label ID
pub type LabelId = u32;

/// Edge type ID
pub type ETypeId = u32;

/// Property key ID
pub type PropKeyId = u32;

/// String ID in string table (0 = none)
pub type StringId = u32;

/// Transaction ID
pub type TxId = u64;

/// Timestamp for MVCC
pub type Timestamp = u64;

/// Property value types
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    VectorF32(Vec<f32>),
}

/// Property value tag for binary encoding
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropValueTag {
    Null = 0,
    Bool = 1,
    I64 = 2,
    F64 = 3,
    String = 4,
    VectorF32 = 5,
}

/// Edge representation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub src: NodeId,
    pub etype: ETypeId,
    pub dst: NodeId,
}

/// Edge patch for delta overlay
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct EdgePatch {
    pub etype: ETypeId,
    pub other: NodeId,
}
```

### 1.2 Constants (`src/constants.rs`)

```rust
//! Magic numbers and constants

// Magic bytes as little-endian u32
pub const MAGIC_MANIFEST: u32 = 0x4D424447; // "GDBM"
pub const MAGIC_SNAPSHOT: u32 = 0x31534447; // "GDS1"
pub const MAGIC_WAL: u32 = 0x31574447;      // "GDW1"

// Single-file magic: "RayDB format 1\0"
pub const MAGIC_RAYDB: [u8; 16] = [
    0x52, 0x61, 0x79, 0x44, 0x42, 0x20, 0x66, 0x6f,
    0x72, 0x6d, 0x61, 0x74, 0x20, 0x31, 0x00, 0x00,
];

// Versions
pub const VERSION_SNAPSHOT: u32 = 1;
pub const VERSION_WAL: u32 = 1;
pub const VERSION_SINGLE_FILE: u32 = 1;

// Alignment
pub const SECTION_ALIGNMENT: usize = 64;
pub const WAL_RECORD_ALIGNMENT: usize = 8;

// Sizes
pub const DEFAULT_PAGE_SIZE: usize = 4096;
pub const DB_HEADER_SIZE: usize = 4096;
pub const WAL_DEFAULT_SIZE: usize = 64 * 1024 * 1024; // 64MB

// Thresholds
pub const COMPACT_EDGE_RATIO: f64 = 0.1;
pub const COMPACT_NODE_RATIO: f64 = 0.1;
pub const DELTA_SET_UPGRADE_THRESHOLD: usize = 64;

// Initial IDs
pub const INITIAL_NODE_ID: NodeId = 1;
pub const INITIAL_LABEL_ID: LabelId = 1;
pub const INITIAL_ETYPE_ID: ETypeId = 1;
pub const INITIAL_PROPKEY_ID: PropKeyId = 1;
pub const INITIAL_TX_ID: TxId = 1;
```

### 1.3 Error Types (`src/error.rs`)

```rust
//! Error types for RayDB

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RayError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid magic number: expected 0x{expected:08X}, got 0x{got:08X}")]
    InvalidMagic { expected: u32, got: u32 },

    #[error("Version mismatch: requires {required}, we have {current}")]
    VersionMismatch { required: u32, current: u32 },

    #[error("CRC mismatch: stored 0x{stored:08X}, computed 0x{computed:08X}")]
    CrcMismatch { stored: u32, computed: u32 },

    #[error("Node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Transaction conflict on keys: {keys:?}")]
    Conflict { txid: TxId, keys: Vec<String> },

    #[error("WAL buffer full")]
    WalBufferFull,

    #[error("Database is read-only")]
    ReadOnly,

    #[error("Invalid snapshot: {0}")]
    InvalidSnapshot(String),

    #[error("Compression error: {0}")]
    Compression(String),
}

pub type Result<T> = std::result::Result<T, RayError>;
```

### 1.4 Utilities (`src/util/`)

#### Binary Helpers (`src/util/binary.rs`)

```rust
//! Binary encoding/decoding helpers

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io::{Read, Write};

/// Read u32 at index from slice (element-indexed, not byte-indexed)
#[inline]
pub fn read_u32_at(data: &[u8], index: usize) -> u32 {
    let offset = index * 4;
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read u64 at index from slice (element-indexed)
#[inline]
pub fn read_u64_at(data: &[u8], index: usize) -> u64 {
    let offset = index * 8;
    u64::from_le_bytes([
        data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
        data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7],
    ])
}

/// Read i32 at index (returns -1 for 0xFFFFFFFF)
#[inline]
pub fn read_i32_at(data: &[u8], index: usize) -> i32 {
    read_u32_at(data, index) as i32
}

/// Align value up to alignment
#[inline]
pub const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

/// Calculate padding needed for alignment
#[inline]
pub const fn padding_for(value: usize, alignment: usize) -> usize {
    align_up(value, alignment) - value
}
```

#### CRC32C (`src/util/crc.rs`)

```rust
//! CRC32C checksums using hardware acceleration when available

use crc32fast::Hasher;

/// Compute CRC32C of data
#[inline]
pub fn crc32c(data: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(data);
    hasher.finalize()
}
```

#### xxHash64 (`src/util/hash.rs`)

```rust
//! xxHash64 for key hashing

use xxhash_rust::xxh64::xxh64;

/// Compute xxHash64 of string
#[inline]
pub fn xxhash64_string(s: &str) -> u64 {
    xxh64(s.as_bytes(), 0)
}

/// Compute xxHash64 of bytes
#[inline]
pub fn xxhash64(data: &[u8]) -> u64 {
    xxh64(data, 0)
}
```

#### Compression (`src/util/compression.rs`)

```rust
//! Compression support (zstd, gzip, deflate)

use crate::error::{RayError, Result};

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionType {
    None = 0,
    Zstd = 1,
    Gzip = 2,
    Deflate = 3,
}

/// Compress data with specified algorithm
pub fn compress(data: &[u8], compression: CompressionType, level: i32) -> Result<Vec<u8>> {
    match compression {
        CompressionType::None => Ok(data.to_vec()),
        CompressionType::Zstd => {
            zstd::encode_all(data, level)
                .map_err(|e| RayError::Compression(e.to_string()))
        }
        CompressionType::Gzip => {
            use flate2::write::GzEncoder;
            use flate2::Compression;
            let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level as u32));
            encoder.write_all(data)?;
            encoder.finish().map_err(|e| RayError::Compression(e.to_string()))
        }
        CompressionType::Deflate => {
            use flate2::write::DeflateEncoder;
            use flate2::Compression;
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level as u32));
            encoder.write_all(data)?;
            encoder.finish().map_err(|e| RayError::Compression(e.to_string()))
        }
    }
}

/// Decompress data
pub fn decompress(data: &[u8], compression: CompressionType) -> Result<Vec<u8>> {
    match compression {
        CompressionType::None => Ok(data.to_vec()),
        CompressionType::Zstd => {
            zstd::decode_all(data)
                .map_err(|e| RayError::Compression(e.to_string()))
        }
        CompressionType::Gzip => {
            use flate2::read::GzDecoder;
            let mut decoder = GzDecoder::new(data);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
        CompressionType::Deflate => {
            use flate2::read::DeflateDecoder;
            let mut decoder = DeflateDecoder::new(data);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out)?;
            Ok(out)
        }
    }
}
```

---

## Phase 2: Storage Layer

### 2.1 Snapshot Reader (`src/core/snapshot/reader.rs`)

Key aspects:
- Memory-mapped file access via `memmap2`
- Zero-copy section access
- Lazy decompression with caching
- CSR (Compressed Sparse Row) format for edges

```rust
//! CSR Snapshot Reader

use memmap2::Mmap;
use std::fs::File;
use std::path::Path;
use crate::error::Result;
use crate::types::*;

/// Section identifiers
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum SectionId {
    PhysToNodeId = 0,
    NodeIdToPhys = 1,
    OutOffsets = 2,
    OutDst = 3,
    OutEtype = 4,
    InOffsets = 5,
    InSrc = 6,
    InEtype = 7,
    InOutIndex = 8,
    StringOffsets = 9,
    StringBytes = 10,
    LabelStringIds = 11,
    EtypeStringIds = 12,
    PropkeyStringIds = 13,
    NodeKeyString = 14,
    KeyEntries = 15,
    KeyBuckets = 16,
    NodePropOffsets = 17,
    NodePropKeys = 18,
    NodePropVals = 19,
    EdgePropOffsets = 20,
    EdgePropKeys = 21,
    EdgePropVals = 22,
    Count = 23,
}

/// Section entry in snapshot header
#[derive(Debug, Clone, Copy)]
pub struct SectionEntry {
    pub offset: u64,
    pub length: u64,
    pub compression: u32,
    pub uncompressed_size: u32,
}

/// Snapshot header
#[derive(Debug)]
pub struct SnapshotHeader {
    pub magic: u32,
    pub version: u32,
    pub min_reader_version: u32,
    pub flags: u32,
    pub generation: u64,
    pub created_unix_ns: u64,
    pub num_nodes: u64,
    pub num_edges: u64,
    pub max_node_id: u64,
    pub num_labels: u64,
    pub num_etypes: u64,
    pub num_propkeys: u64,
    pub num_strings: u64,
}

/// Snapshot flags
bitflags::bitflags! {
    pub struct SnapshotFlags: u32 {
        const HAS_IN_EDGES = 1 << 0;
        const HAS_PROPERTIES = 1 << 1;
        const HAS_KEY_BUCKETS = 1 << 2;
        const HAS_EDGE_BLOOM = 1 << 3;
    }
}

/// Memory-mapped snapshot data
pub struct SnapshotData {
    mmap: Mmap,
    pub header: SnapshotHeader,
    sections: Vec<SectionEntry>,
    
    // Cached decompressed sections
    decompressed_cache: parking_lot::RwLock<HashMap<SectionId, Vec<u8>>>,
}

impl SnapshotData {
    /// Load snapshot from file
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        Self::parse(mmap)
    }

    /// Parse from mmap buffer
    pub fn parse(mmap: Mmap) -> Result<Self> {
        // Parse header and sections...
        todo!()
    }

    /// Get section bytes (decompressed)
    pub fn section(&self, id: SectionId) -> Option<&[u8]> {
        // Check decompression cache, decompress if needed
        todo!()
    }

    /// Get NodeID for physical index
    #[inline]
    pub fn get_node_id(&self, phys: PhysNode) -> NodeId {
        let section = self.section(SectionId::PhysToNodeId).unwrap();
        read_u64_at(section, phys as usize)
    }

    /// Get physical index for NodeID (-1 if not found)
    #[inline]
    pub fn get_phys_node(&self, node_id: NodeId) -> Option<PhysNode> {
        let section = self.section(SectionId::NodeIdToPhys)?;
        let idx = node_id as usize;
        if idx * 4 >= section.len() {
            return None;
        }
        let phys = read_i32_at(section, idx);
        if phys < 0 { None } else { Some(phys as PhysNode) }
    }

    /// Check if edge exists (binary search in CSR)
    pub fn has_edge(&self, src_phys: PhysNode, etype: ETypeId, dst_phys: PhysNode) -> bool {
        let offsets = self.section(SectionId::OutOffsets).unwrap();
        let dst_arr = self.section(SectionId::OutDst).unwrap();
        let etype_arr = self.section(SectionId::OutEtype).unwrap();

        let start = read_u32_at(offsets, src_phys as usize) as usize;
        let end = read_u32_at(offsets, src_phys as usize + 1) as usize;

        // Binary search for (etype, dst)
        let mut lo = start;
        let mut hi = end;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let mid_etype = read_u32_at(etype_arr, mid);
            let mid_dst = read_u32_at(dst_arr, mid);

            if mid_etype < etype || (mid_etype == etype && mid_dst < dst_phys) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo < end {
            let found_etype = read_u32_at(etype_arr, lo);
            let found_dst = read_u32_at(dst_arr, lo);
            found_etype == etype && found_dst == dst_phys
        } else {
            false
        }
    }

    /// Iterate out-edges for a physical node
    pub fn iter_out_edges(&self, phys: PhysNode) -> OutEdgeIter<'_> {
        OutEdgeIter::new(self, phys)
    }

    /// Lookup node by key
    pub fn lookup_by_key(&self, key: &str) -> Option<NodeId> {
        let hash = xxhash64_string(key);
        let entries = self.section(SectionId::KeyEntries)?;
        let buckets = self.section(SectionId::KeyBuckets);
        
        // Hash-bucketed lookup with collision resolution
        todo!()
    }
}

/// Iterator over out-edges
pub struct OutEdgeIter<'a> {
    snapshot: &'a SnapshotData,
    current: usize,
    end: usize,
}

impl<'a> Iterator for OutEdgeIter<'a> {
    type Item = (PhysNode, ETypeId); // (dst, etype)

    fn next(&mut self) -> Option<Self::Item> {
        if self.current >= self.end {
            return None;
        }
        let dst_arr = self.snapshot.section(SectionId::OutDst)?;
        let etype_arr = self.snapshot.section(SectionId::OutEtype)?;
        
        let dst = read_u32_at(dst_arr, self.current);
        let etype = read_u32_at(etype_arr, self.current);
        self.current += 1;
        Some((dst, etype))
    }
}
```

### 2.2 Delta State (`src/core/delta.rs`)

```rust
//! In-memory delta overlay for uncommitted changes

use std::collections::{HashMap, HashSet, BTreeSet};
use crate::types::*;

/// Node delta - changes to a single node
#[derive(Debug, Default, Clone)]
pub struct NodeDelta {
    pub key: Option<String>,
    pub labels: Option<HashSet<LabelId>>,
    pub labels_deleted: Option<HashSet<LabelId>>,
    pub props: Option<HashMap<PropKeyId, Option<PropValue>>>,
}

/// Delta state - all uncommitted changes
#[derive(Debug, Default)]
pub struct DeltaState {
    // Node mutations
    pub created_nodes: HashMap<NodeId, NodeDelta>,
    pub deleted_nodes: HashSet<NodeId>,
    pub modified_nodes: HashMap<NodeId, NodeDelta>,

    // Edge patches (sorted for binary search)
    pub out_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub out_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub in_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub in_del: HashMap<NodeId, BTreeSet<EdgePatch>>,

    // Edge properties
    pub edge_props: HashMap<(NodeId, ETypeId, NodeId), HashMap<PropKeyId, Option<PropValue>>>,

    // Schema definitions
    pub new_labels: HashMap<LabelId, String>,
    pub new_etypes: HashMap<ETypeId, String>,
    pub new_propkeys: HashMap<PropKeyId, String>,

    // Key index
    pub key_index: HashMap<String, NodeId>,
    pub key_index_deleted: HashSet<String>,

    // Reverse index for O(k) edge cleanup
    pub incoming_edge_sources: HashMap<NodeId, HashSet<NodeId>>,
}

impl DeltaState {
    /// Create empty delta
    pub fn new() -> Self {
        Self::default()
    }

    /// Add edge with cancellation logic
    pub fn add_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
        let patch = EdgePatch { etype, other: dst };
        
        // Check if cancels a pending delete
        if let Some(del_set) = self.out_del.get_mut(&src) {
            if del_set.remove(&patch) {
                if del_set.is_empty() {
                    self.out_del.remove(&src);
                }
            } else {
                self.out_add.entry(src).or_default().insert(patch);
            }
        } else {
            self.out_add.entry(src).or_default().insert(patch);
        }

        // Same for in-edges
        let in_patch = EdgePatch { etype, other: src };
        if let Some(del_set) = self.in_del.get_mut(&dst) {
            if del_set.remove(&in_patch) {
                if del_set.is_empty() {
                    self.in_del.remove(&dst);
                }
            } else {
                self.in_add.entry(dst).or_default().insert(in_patch);
            }
        } else {
            self.in_add.entry(dst).or_default().insert(in_patch);
        }

        // Track reverse index
        self.incoming_edge_sources
            .entry(dst)
            .or_default()
            .insert(src);
    }

    /// Delete edge with cancellation logic
    pub fn delete_edge(&mut self, src: NodeId, etype: ETypeId, dst: NodeId) {
        let patch = EdgePatch { etype, other: dst };
        
        // Check if cancels a pending add
        if let Some(add_set) = self.out_add.get_mut(&src) {
            if add_set.remove(&patch) {
                if add_set.is_empty() {
                    self.out_add.remove(&src);
                }
                // Also remove from in_add
                if let Some(in_add_set) = self.in_add.get_mut(&dst) {
                    let in_patch = EdgePatch { etype, other: src };
                    in_add_set.remove(&in_patch);
                    if in_add_set.is_empty() {
                        self.in_add.remove(&dst);
                    }
                }
                return;
            }
        }

        // Add to delete set
        self.out_del.entry(src).or_default().insert(patch);
        let in_patch = EdgePatch { etype, other: src };
        self.in_del.entry(dst).or_default().insert(in_patch);
    }

    /// Check if edge is deleted in delta
    pub fn is_edge_deleted(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
        self.out_del
            .get(&src)
            .map(|s| s.contains(&EdgePatch { etype, other: dst }))
            .unwrap_or(false)
    }

    /// Check if edge is added in delta
    pub fn is_edge_added(&self, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
        self.out_add
            .get(&src)
            .map(|s| s.contains(&EdgePatch { etype, other: dst }))
            .unwrap_or(false)
    }

    /// Clear all delta state
    pub fn clear(&mut self) {
        self.created_nodes.clear();
        self.deleted_nodes.clear();
        self.modified_nodes.clear();
        self.out_add.clear();
        self.out_del.clear();
        self.in_add.clear();
        self.in_del.clear();
        self.edge_props.clear();
        self.new_labels.clear();
        self.new_etypes.clear();
        self.new_propkeys.clear();
        self.key_index.clear();
        self.key_index_deleted.clear();
        self.incoming_edge_sources.clear();
    }
}
```

### 2.3 WAL (`src/core/wal/`)

#### Record Types (`src/core/wal/record.rs`)

```rust
//! WAL record types and serialization

use crate::types::*;

/// WAL record types
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalRecordType {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
    CreateNode = 10,
    DeleteNode = 11,
    AddEdge = 20,
    DeleteEdge = 21,
    DefineLabel = 30,
    AddNodeLabel = 31,
    RemoveNodeLabel = 32,
    DefineEtype = 40,
    DefinePropkey = 50,
    SetNodeProp = 51,
    DelNodeProp = 52,
    SetEdgeProp = 53,
    DelEdgeProp = 54,
    SetNodeVector = 60,
    DelNodeVector = 61,
    BatchVectors = 62,
    SealFragment = 63,
    CompactFragments = 64,
}

/// WAL record header (20 bytes)
#[repr(C, packed)]
pub struct WalRecordHeader {
    pub rec_len: u32,      // Unpadded length
    pub record_type: u8,
    pub flags: u8,
    pub reserved: u16,
    pub txid: u64,
    pub payload_len: u32,
}

/// Parsed WAL record
pub struct ParsedWalRecord {
    pub record_type: WalRecordType,
    pub flags: u8,
    pub txid: TxId,
    pub payload: Vec<u8>,
    pub record_end: usize,
}

// Payload structs
pub struct CreateNodePayload {
    pub node_id: NodeId,
    pub key: Option<String>,
}

pub struct AddEdgePayload {
    pub src: NodeId,
    pub etype: ETypeId,
    pub dst: NodeId,
}

pub struct SetNodePropPayload {
    pub node_id: NodeId,
    pub key_id: PropKeyId,
    pub value: PropValue,
}

// ... more payload types
```

### 2.4 Single-File Header (`src/core/header.rs`)

```rust
//! Single-file database header management

use crate::constants::*;
use crate::error::Result;
use crate::util::crc::crc32c;

/// Database header for single-file format (4KB)
#[derive(Debug, Clone)]
pub struct DbHeader {
    pub magic: [u8; 16],
    pub page_size: u32,
    pub version: u32,
    pub min_reader_version: u32,
    pub flags: u32,
    pub change_counter: u64,
    pub db_size_pages: u64,
    pub snapshot_start_page: u64,
    pub snapshot_page_count: u64,
    pub wal_start_page: u64,
    pub wal_page_count: u64,
    pub wal_head: u64,
    pub wal_tail: u64,
    pub active_snapshot_gen: u64,
    pub prev_snapshot_gen: u64,
    pub max_node_id: u64,
    pub next_tx_id: u64,
    pub last_commit_ts: u64,
    pub schema_cookie: u64,
    // V2 dual-buffer WAL fields
    pub wal_primary_head: u64,
    pub wal_secondary_head: u64,
    pub active_wal_region: u8,
    pub checkpoint_in_progress: u8,
}

impl DbHeader {
    /// Parse header from page buffer
    pub fn parse(data: &[u8]) -> Result<Self> {
        assert!(data.len() >= DB_HEADER_SIZE);
        
        // Verify magic
        if &data[0..16] != MAGIC_RAYDB {
            return Err(RayError::InvalidMagic {
                expected: u32::from_le_bytes(MAGIC_RAYDB[0..4].try_into().unwrap()),
                got: u32::from_le_bytes(data[0..4].try_into().unwrap()),
            });
        }

        // Verify checksums
        let header_crc = u32::from_le_bytes(data[176..180].try_into().unwrap());
        let computed_header_crc = crc32c(&data[0..176]);
        if header_crc != computed_header_crc {
            return Err(RayError::CrcMismatch {
                stored: header_crc,
                computed: computed_header_crc,
            });
        }

        // Parse fields...
        todo!()
    }

    /// Serialize header to page buffer
    pub fn serialize(&self) -> [u8; DB_HEADER_SIZE] {
        let mut buf = [0u8; DB_HEADER_SIZE];
        buf[0..16].copy_from_slice(&self.magic);
        // ... write all fields
        
        // Compute and write checksums
        let header_crc = crc32c(&buf[0..176]);
        buf[176..180].copy_from_slice(&header_crc.to_le_bytes());
        
        let footer_crc = crc32c(&buf[0..4092]);
        buf[4092..4096].copy_from_slice(&footer_crc.to_le_bytes());
        
        buf
    }
}
```

---

## Phase 3: Graph Operations

### 3.1 GraphDB Struct (`src/graph/db.rs`)

```rust
//! Main GraphDB struct and lifecycle

use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::{RwLock, Mutex};
use crate::core::snapshot::SnapshotData;
use crate::core::delta::DeltaState;
use crate::mvcc::MvccManager;
use crate::cache::CacheManager;

/// Database open options
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub read_only: bool,
    pub create_if_missing: bool,
    pub lock_file: bool,
    pub mvcc: bool,
    pub mvcc_gc_interval_ms: u64,
    pub mvcc_retention_ms: u64,
    pub cache: Option<CacheOptions>,
    pub auto_checkpoint: bool,
    pub checkpoint_threshold: f64,
    pub page_size: usize,
    pub wal_size: usize,
}

/// Graph database handle
pub struct GraphDB {
    pub(crate) path: PathBuf,
    pub(crate) read_only: bool,
    pub(crate) is_single_file: bool,

    // Snapshot (mmap'd, read-only after load)
    pub(crate) snapshot: Option<Arc<SnapshotData>>,
    
    // Delta state (protected by RwLock for concurrent reads)
    pub(crate) delta: RwLock<DeltaState>,
    
    // ID counters (atomic for lock-free allocation)
    pub(crate) next_node_id: AtomicU64,
    pub(crate) next_label_id: AtomicU32,
    pub(crate) next_etype_id: AtomicU32,
    pub(crate) next_propkey_id: AtomicU32,
    pub(crate) next_tx_id: AtomicU64,

    // Single-file specific
    pub(crate) header: Option<RwLock<DbHeader>>,
    pub(crate) pager: Option<Pager>,
    
    // Multi-file specific
    pub(crate) manifest: Option<RwLock<Manifest>>,
    pub(crate) wal_fd: Option<Mutex<std::fs::File>>,
    pub(crate) wal_offset: AtomicUsize,

    // Optional components
    pub(crate) cache: Option<CacheManager>,
    pub(crate) mvcc: Option<MvccManager>,
    
    // Lock file handle
    pub(crate) lock_handle: Option<fs2::FileLock>,
}

impl GraphDB {
    /// Open or create a database
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self> {
        let path = path.as_ref();
        let is_single_file = path.extension()
            .map(|e| e == "raydb")
            .unwrap_or(false);

        if is_single_file {
            Self::open_single_file(path, options)
        } else {
            Self::open_multi_file(path, options)
        }
    }

    fn open_single_file(path: &Path, options: OpenOptions) -> Result<Self> {
        todo!()
    }

    fn open_multi_file(path: &Path, options: OpenOptions) -> Result<Self> {
        todo!()
    }

    /// Close the database
    pub fn close(self) -> Result<()> {
        // Flush pending writes, release locks
        todo!()
    }
}
```

### 3.2 Transaction Handling (`src/graph/tx.rs`)

```rust
//! Transaction handling

use crate::types::*;
use crate::graph::db::GraphDB;

/// Transaction state
pub struct TxState {
    pub txid: TxId,
    pub pending_created_nodes: HashMap<NodeId, NodeDelta>,
    pub pending_deleted_nodes: HashSet<NodeId>,
    pub pending_out_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub pending_out_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub pending_in_add: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub pending_in_del: HashMap<NodeId, BTreeSet<EdgePatch>>,
    pub pending_node_props: HashMap<NodeId, HashMap<PropKeyId, Option<PropValue>>>,
    pub pending_edge_props: HashMap<(NodeId, ETypeId, NodeId), HashMap<PropKeyId, Option<PropValue>>>,
    pub pending_new_labels: HashMap<LabelId, String>,
    pub pending_new_etypes: HashMap<ETypeId, String>,
    pub pending_new_propkeys: HashMap<PropKeyId, String>,
    pub pending_key_updates: HashMap<String, NodeId>,
    pub pending_key_deletes: HashSet<String>,
    // Vector operations
    pub pending_vector_sets: HashMap<(NodeId, PropKeyId), Vec<f32>>,
    pub pending_vector_deletes: HashSet<(NodeId, PropKeyId)>,
}

/// Transaction handle
pub struct TxHandle<'a> {
    pub(crate) db: &'a GraphDB,
    pub(crate) state: TxState,
}

impl<'a> TxHandle<'a> {
    /// Begin a new transaction
    pub fn begin(db: &'a GraphDB) -> Self {
        let txid = db.next_tx_id.fetch_add(1, Ordering::SeqCst);
        TxHandle {
            db,
            state: TxState {
                txid,
                ..Default::default()
            },
        }
    }

    /// Commit the transaction
    pub fn commit(self) -> Result<()> {
        // 1. MVCC validation (if enabled)
        if let Some(ref mvcc) = self.db.mvcc {
            mvcc.validate_commit(&self.state)?;
        }

        // 2. Build WAL records
        let records = self.build_wal_records();

        // 3. Write to WAL
        self.db.write_wal_records(&records)?;

        // 4. Apply to delta
        self.db.apply_tx_to_delta(&self.state)?;

        // 5. Create version chains (if MVCC)
        if let Some(ref mvcc) = self.db.mvcc {
            mvcc.create_versions(&self.state);
        }

        // 6. Invalidate caches
        if let Some(ref cache) = self.db.cache {
            cache.invalidate_for_tx(&self.state);
        }

        Ok(())
    }

    /// Rollback the transaction
    pub fn rollback(self) {
        // Just drop - nothing persisted yet
    }

    fn build_wal_records(&self) -> Vec<WalRecord> {
        todo!()
    }
}

/// Begin a transaction
pub fn begin_tx(db: &GraphDB) -> TxHandle<'_> {
    TxHandle::begin(db)
}

/// Commit a transaction
pub fn commit(tx: TxHandle<'_>) -> Result<()> {
    tx.commit()
}

/// Rollback a transaction
pub fn rollback(tx: TxHandle<'_>) {
    tx.rollback()
}
```

### 3.3 Node Operations (`src/graph/nodes.rs`)

```rust
//! Node CRUD operations

use crate::graph::tx::TxHandle;
use crate::graph::db::GraphDB;
use crate::types::*;

/// Create a new node
pub fn create_node(tx: &mut TxHandle<'_>, opts: NodeOpts) -> NodeId {
    let node_id = tx.db.next_node_id.fetch_add(1, Ordering::SeqCst);
    
    let mut delta = NodeDelta::default();
    if let Some(key) = &opts.key {
        delta.key = Some(key.clone());
        tx.state.pending_key_updates.insert(key.clone(), node_id);
    }
    
    tx.state.pending_created_nodes.insert(node_id, delta);
    
    // Apply properties
    if let Some(props) = opts.props {
        for (key_id, value) in props {
            tx.state
                .pending_node_props
                .entry(node_id)
                .or_default()
                .insert(key_id, Some(value));
        }
    }
    
    node_id
}

/// Delete a node
pub fn delete_node(tx: &mut TxHandle<'_>, node_id: NodeId) -> bool {
    // Check if exists
    if !node_exists(&tx.db, node_id) {
        return false;
    }
    
    tx.state.pending_deleted_nodes.insert(node_id);
    
    // Note: Edge cleanup handled by delta.delete_node()
    true
}

/// Check if node exists
pub fn node_exists(db: &GraphDB, node_id: NodeId) -> bool {
    let delta = db.delta.read();
    
    // Check delta first
    if delta.deleted_nodes.contains(&node_id) {
        return false;
    }
    if delta.created_nodes.contains_key(&node_id) {
        return true;
    }
    
    // Check snapshot
    if let Some(ref snapshot) = db.snapshot {
        return snapshot.get_phys_node(node_id).is_some();
    }
    
    false
}

/// Get node by key
pub fn get_node_by_key(db: &GraphDB, key: &str) -> Option<NodeId> {
    let delta = db.delta.read();
    
    // Check delta key index
    if delta.key_index_deleted.contains(key) {
        return None;
    }
    if let Some(&node_id) = delta.key_index.get(key) {
        return Some(node_id);
    }
    
    // Check snapshot
    if let Some(ref snapshot) = db.snapshot {
        return snapshot.lookup_by_key(key);
    }
    
    None
}

/// List all nodes (iterator)
pub fn list_nodes(db: &GraphDB) -> impl Iterator<Item = NodeId> + '_ {
    NodeIterator::new(db)
}

/// Count nodes
pub fn count_nodes(db: &GraphDB) -> usize {
    let delta = db.delta.read();
    let snapshot_count = db.snapshot
        .as_ref()
        .map(|s| s.header.num_nodes as usize)
        .unwrap_or(0);
    
    snapshot_count
        + delta.created_nodes.len()
        - delta.deleted_nodes.len()
}
```

### 3.4 Edge Operations (`src/graph/edges.rs`)

```rust
//! Edge CRUD operations

use crate::graph::tx::TxHandle;
use crate::graph::db::GraphDB;
use crate::types::*;

/// Add an edge
pub fn add_edge(tx: &mut TxHandle<'_>, src: NodeId, etype: ETypeId, dst: NodeId) {
    let patch = EdgePatch { etype, other: dst };
    tx.state.pending_out_add.entry(src).or_default().insert(patch);
    
    let in_patch = EdgePatch { etype, other: src };
    tx.state.pending_in_add.entry(dst).or_default().insert(in_patch);
}

/// Delete an edge
pub fn delete_edge(tx: &mut TxHandle<'_>, src: NodeId, etype: ETypeId, dst: NodeId) {
    let patch = EdgePatch { etype, other: dst };
    
    // Check if cancels pending add
    if let Some(add_set) = tx.state.pending_out_add.get_mut(&src) {
        if add_set.remove(&patch) {
            // Also remove from in_add
            if let Some(in_set) = tx.state.pending_in_add.get_mut(&dst) {
                in_set.remove(&EdgePatch { etype, other: src });
            }
            return;
        }
    }
    
    tx.state.pending_out_del.entry(src).or_default().insert(patch);
    tx.state.pending_in_del.entry(dst).or_default().insert(EdgePatch { etype, other: src });
}

/// Check if edge exists
pub fn edge_exists(db: &GraphDB, src: NodeId, etype: ETypeId, dst: NodeId) -> bool {
    let delta = db.delta.read();
    
    // Check delta deletions
    if delta.is_edge_deleted(src, etype, dst) {
        return false;
    }
    
    // Check delta additions
    if delta.is_edge_added(src, etype, dst) {
        return true;
    }
    
    // Check snapshot
    if let Some(ref snapshot) = db.snapshot {
        if let (Some(src_phys), Some(dst_phys)) = 
            (snapshot.get_phys_node(src), snapshot.get_phys_node(dst)) 
        {
            return snapshot.has_edge(src_phys, etype, dst_phys);
        }
    }
    
    false
}

/// Get out-neighbors for a node
pub fn get_neighbors_out<'a>(
    db: &'a GraphDB,
    node_id: NodeId,
    etype_filter: Option<ETypeId>,
) -> impl Iterator<Item = Edge> + 'a {
    NeighborOutIterator::new(db, node_id, etype_filter)
}

/// Get in-neighbors for a node  
pub fn get_neighbors_in<'a>(
    db: &'a GraphDB,
    node_id: NodeId,
    etype_filter: Option<ETypeId>,
) -> impl Iterator<Item = Edge> + 'a {
    NeighborInIterator::new(db, node_id, etype_filter)
}
```

---

## Phase 4: MVCC

### 4.1 MVCC Manager (`src/mvcc/mod.rs`)

```rust
//! Multi-Version Concurrency Control

mod tx_manager;
mod version_chain;
mod visibility;
mod conflict;
mod gc;

pub use tx_manager::TxManager;
pub use version_chain::VersionChainManager;
pub use visibility::*;
pub use conflict::ConflictDetector;
pub use gc::GarbageCollector;

use std::sync::Arc;
use parking_lot::RwLock;

/// MVCC Manager - coordinates all MVCC components
pub struct MvccManager {
    pub tx_manager: TxManager,
    pub version_chain: Arc<RwLock<VersionChainManager>>,
    pub conflict_detector: ConflictDetector,
    pub gc: GarbageCollector,
}

impl MvccManager {
    pub fn new(
        initial_tx_id: TxId,
        initial_commit_ts: Timestamp,
        gc_interval_ms: u64,
        retention_ms: u64,
        max_chain_depth: usize,
    ) -> Self {
        let tx_manager = TxManager::new(initial_tx_id, initial_commit_ts);
        let version_chain = Arc::new(RwLock::new(VersionChainManager::new()));
        let conflict_detector = ConflictDetector::new();
        let gc = GarbageCollector::new(
            gc_interval_ms,
            retention_ms,
            max_chain_depth,
            Arc::clone(&version_chain),
        );

        Self {
            tx_manager,
            version_chain,
            conflict_detector,
            gc,
        }
    }

    /// Start background GC
    pub fn start(&self) {
        self.gc.start();
    }

    /// Stop background GC
    pub fn stop(&self) {
        self.gc.stop();
    }
}
```

### 4.2 Version Chain (`src/mvcc/version_chain.rs`)

```rust
//! Version chain storage for MVCC

use std::collections::HashMap;
use crate::types::*;

/// Versioned record
#[derive(Debug)]
pub struct VersionedRecord<T> {
    pub data: T,
    pub txid: TxId,
    pub commit_ts: Timestamp,
    pub prev: Option<Box<VersionedRecord<T>>>,
    pub deleted: bool,
}

/// Node version data
#[derive(Debug, Clone)]
pub struct NodeVersionData {
    pub node_id: NodeId,
    pub delta: NodeDelta,
}

/// Edge version data
#[derive(Debug, Clone)]
pub struct EdgeVersionData {
    pub src: NodeId,
    pub etype: ETypeId,
    pub dst: NodeId,
    pub added: bool,
}

/// Version chain storage
pub struct VersionChainManager {
    node_versions: HashMap<NodeId, VersionedRecord<NodeVersionData>>,
    edge_versions: HashMap<u64, VersionedRecord<EdgeVersionData>>, // Composite key
    node_prop_versions: HashMap<u64, VersionedRecord<Option<PropValue>>>,
    edge_prop_versions: HashMap<u64, VersionedRecord<Option<PropValue>>>,
}

impl VersionChainManager {
    pub fn new() -> Self {
        Self {
            node_versions: HashMap::new(),
            edge_versions: HashMap::new(),
            node_prop_versions: HashMap::new(),
            edge_prop_versions: HashMap::new(),
        }
    }

    /// Create composite key for edge
    #[inline]
    fn edge_key(src: NodeId, etype: ETypeId, dst: NodeId) -> u64 {
        // Pack: src(20 bits) | etype(12 bits) | dst(32 bits)
        ((src & 0xFFFFF) << 44) | ((etype as u64 & 0xFFF) << 32) | (dst as u64 & 0xFFFFFFFF)
    }

    /// Add version for node
    pub fn add_node_version(&mut self, node_id: NodeId, data: NodeVersionData, txid: TxId, commit_ts: Timestamp) {
        let prev = self.node_versions.remove(&node_id).map(Box::new);
        self.node_versions.insert(node_id, VersionedRecord {
            data,
            txid,
            commit_ts,
            prev,
            deleted: false,
        });
    }

    /// Get visible version for node
    pub fn get_visible_node(&self, node_id: NodeId, start_ts: Timestamp) -> Option<&NodeVersionData> {
        let mut current = self.node_versions.get(&node_id)?;
        
        // Walk chain to find visible version
        loop {
            if current.commit_ts <= start_ts && !current.deleted {
                return Some(&current.data);
            }
            match &current.prev {
                Some(prev) => current = prev,
                None => return None,
            }
        }
    }

    /// Prune old versions
    pub fn prune(&mut self, min_active_ts: Timestamp, max_depth: usize) -> usize {
        let mut pruned = 0;
        
        for version in self.node_versions.values_mut() {
            pruned += Self::prune_chain(version, min_active_ts, max_depth);
        }
        
        pruned
    }

    fn prune_chain<T>(record: &mut VersionedRecord<T>, min_ts: Timestamp, max_depth: usize) -> usize {
        let mut count = 0;
        let mut depth = 1;
        let mut current = record;
        
        while let Some(ref mut prev) = current.prev {
            depth += 1;
            if prev.commit_ts < min_ts || depth > max_depth {
                // Truncate chain here
                count += Self::count_chain(&prev);
                current.prev = None;
                break;
            }
            current = prev;
        }
        
        count
    }

    fn count_chain<T>(record: &VersionedRecord<T>) -> usize {
        let mut count = 1;
        let mut current = record;
        while let Some(ref prev) = current.prev {
            count += 1;
            current = prev;
        }
        count
    }
}
```

### 4.3 Conflict Detection (`src/mvcc/conflict.rs`)

```rust
//! Write-write conflict detection (First-Committer-Wins)

use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;
use crate::types::*;
use crate::error::{RayError, Result};

/// Conflict detector
pub struct ConflictDetector {
    /// Write timestamps: key -> commit timestamp of last write
    write_timestamps: RwLock<HashMap<String, Timestamp>>,
}

impl ConflictDetector {
    pub fn new() -> Self {
        Self {
            write_timestamps: RwLock::new(HashMap::new()),
        }
    }

    /// Check for conflicts before commit
    pub fn check_conflicts(
        &self,
        start_ts: Timestamp,
        write_set: &HashSet<String>,
    ) -> Result<()> {
        let timestamps = self.write_timestamps.read();
        let mut conflicts = Vec::new();

        for key in write_set {
            if let Some(&last_write_ts) = timestamps.get(key) {
                if last_write_ts > start_ts {
                    conflicts.push(key.clone());
                }
            }
        }

        if conflicts.is_empty() {
            Ok(())
        } else {
            Err(RayError::Conflict {
                txid: 0, // Will be filled by caller
                keys: conflicts,
            })
        }
    }

    /// Record writes after successful commit
    pub fn record_writes(&self, commit_ts: Timestamp, write_set: &HashSet<String>) {
        let mut timestamps = self.write_timestamps.write();
        for key in write_set {
            timestamps.insert(key.clone(), commit_ts);
        }
    }
}
```

---

## Phase 5: Vector Embeddings

### 5.1 Vector Types (`src/vector/types.rs`)

```rust
//! Vector embedding types

/// Vector store configuration
#[derive(Debug, Clone)]
pub struct VectorStoreConfig {
    pub dimensions: usize,
    pub metric: DistanceMetric,
    pub row_group_size: usize,
    pub fragment_target_size: usize,
    pub normalize: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    Cosine,
    Euclidean,
    DotProduct,
}

/// IVF index configuration
#[derive(Debug, Clone)]
pub struct IvfConfig {
    pub n_clusters: usize,
    pub n_probe: usize,
    pub metric: DistanceMetric,
    pub use_pq: bool,
    pub pq_subvectors: Option<usize>,
    pub pq_bits: Option<u8>,
}

/// Row group - batch of vectors
#[derive(Debug)]
pub struct RowGroup {
    pub id: u32,
    pub count: usize,
    pub data: Vec<f32>, // Contiguous: [v0_d0, v0_d1, ..., v1_d0, ...]
}

/// Fragment - collection of row groups
#[derive(Debug)]
pub struct Fragment {
    pub id: u32,
    pub state: FragmentState,
    pub row_groups: Vec<RowGroup>,
    pub total_vectors: usize,
    pub deletion_bitmap: Vec<u32>,
    pub deleted_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentState {
    Active,
    Sealed,
}

/// Search result
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub vector_id: u64,
    pub node_id: NodeId,
    pub distance: f32,
    pub similarity: f32,
}
```

### 5.2 Distance Functions (`src/vector/distance.rs`)

```rust
//! Distance functions with SIMD acceleration

use std::simd::{f32x8, SimdFloat};

/// Dot product (SIMD accelerated)
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    
    let chunks = a.len() / 8;
    let mut sum = f32x8::splat(0.0);
    
    for i in 0..chunks {
        let va = f32x8::from_slice(&a[i * 8..]);
        let vb = f32x8::from_slice(&b[i * 8..]);
        sum += va * vb;
    }
    
    let mut result = sum.reduce_sum();
    
    // Handle remainder
    for i in (chunks * 8)..a.len() {
        result += a[i] * b[i];
    }
    
    result
}

/// Squared Euclidean distance (SIMD)
#[inline]
pub fn squared_euclidean(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    
    let chunks = a.len() / 8;
    let mut sum = f32x8::splat(0.0);
    
    for i in 0..chunks {
        let va = f32x8::from_slice(&a[i * 8..]);
        let vb = f32x8::from_slice(&b[i * 8..]);
        let diff = va - vb;
        sum += diff * diff;
    }
    
    let mut result = sum.reduce_sum();
    
    for i in (chunks * 8)..a.len() {
        let diff = a[i] - b[i];
        result += diff * diff;
    }
    
    result
}

/// Cosine distance (1 - cosine_similarity)
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    // For normalized vectors, cosine = dot product
    1.0 - dot_product(a, b)
}

/// L2 normalize vector in-place
pub fn normalize_in_place(v: &mut [f32]) {
    let norm = dot_product(v, v).sqrt();
    if norm > 1e-10 {
        let inv_norm = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv_norm;
        }
    }
}

/// Find k nearest neighbors
pub fn find_k_nearest(
    query: &[f32],
    vectors: impl Iterator<Item = (u64, &[f32])>,
    k: usize,
    distance_fn: impl Fn(&[f32], &[f32]) -> f32,
) -> Vec<(u64, f32)> {
    use std::collections::BinaryHeap;
    use std::cmp::Reverse;
    
    // Max-heap to keep k smallest distances
    let mut heap: BinaryHeap<(Reverse<ordered_float::OrderedFloat<f32>>, u64)> = BinaryHeap::new();
    
    for (id, vec) in vectors {
        let dist = distance_fn(query, vec);
        
        if heap.len() < k {
            heap.push((Reverse(ordered_float::OrderedFloat(dist)), id));
        } else if let Some(&(Reverse(ordered_float::OrderedFloat(max_dist)), _)) = heap.peek() {
            if dist < max_dist {
                heap.pop();
                heap.push((Reverse(ordered_float::OrderedFloat(dist)), id));
            }
        }
    }
    
    heap.into_sorted_vec()
        .into_iter()
        .map(|(Reverse(ordered_float::OrderedFloat(d)), id)| (id, d))
        .collect()
}
```

### 5.3 IVF Index (`src/vector/ivf/index.rs`)

```rust
//! IVF (Inverted File) index for approximate nearest neighbor search

use crate::vector::types::*;
use crate::vector::distance::*;

/// IVF Index
pub struct IvfIndex {
    pub config: IvfConfig,
    pub centroids: Vec<f32>, // n_clusters * dimensions
    pub inverted_lists: Vec<Vec<u64>>, // cluster_id -> vector_ids
    pub trained: bool,
    training_vectors: Option<Vec<f32>>,
    training_count: usize,
    dimensions: usize,
}

impl IvfIndex {
    pub fn new(config: IvfConfig, dimensions: usize) -> Self {
        Self {
            config,
            centroids: Vec::new(),
            inverted_lists: vec![Vec::new(); config.n_clusters],
            trained: false,
            training_vectors: Some(Vec::new()),
            training_count: 0,
            dimensions,
        }
    }

    /// Add vectors for training
    pub fn add_training_vectors(&mut self, vectors: &[f32]) {
        if let Some(ref mut tv) = self.training_vectors {
            tv.extend_from_slice(vectors);
            self.training_count += vectors.len() / self.dimensions;
        }
    }

    /// Train the index using k-means clustering
    pub fn train(&mut self) -> Result<(), String> {
        let training_data = self.training_vectors.take()
            .ok_or("No training vectors")?;
        
        if self.training_count < self.config.n_clusters {
            return Err("Not enough training vectors".into());
        }

        // Run k-means
        self.centroids = kmeans(
            &training_data,
            self.dimensions,
            self.config.n_clusters,
            20, // iterations
            &self.config.metric,
        );
        
        self.trained = true;
        Ok(())
    }

    /// Insert a vector
    pub fn insert(&mut self, vector_id: u64, vector: &[f32]) {
        if !self.trained {
            return;
        }

        let cluster = self.find_nearest_centroid(vector);
        self.inverted_lists[cluster].push(vector_id);
    }

    /// Delete a vector
    pub fn delete(&mut self, vector_id: u64) {
        for list in &mut self.inverted_lists {
            list.retain(|&id| id != vector_id);
        }
    }

    /// Search for nearest neighbors
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        n_probe: Option<usize>,
        get_vector: impl Fn(u64) -> Option<Vec<f32>>,
    ) -> Vec<VectorSearchResult> {
        if !self.trained {
            return Vec::new();
        }

        let n_probe = n_probe.unwrap_or(self.config.n_probe);
        
        // Find nearest centroids
        let nearest_clusters = self.find_nearest_centroids(query, n_probe);
        
        // Collect candidates from probed clusters
        let candidates: Vec<u64> = nearest_clusters
            .iter()
            .flat_map(|&c| &self.inverted_lists[c])
            .copied()
            .collect();
        
        // Re-rank with exact distances
        let distance_fn = match self.config.metric {
            DistanceMetric::Cosine => cosine_distance,
            DistanceMetric::Euclidean => |a, b| squared_euclidean(a, b).sqrt(),
            DistanceMetric::DotProduct => |a, b| -dot_product(a, b),
        };
        
        let mut results: Vec<_> = candidates
            .iter()
            .filter_map(|&id| {
                let vec = get_vector(id)?;
                let dist = distance_fn(query, &vec);
                Some((id, dist))
            })
            .collect();
        
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        results.truncate(k);
        
        results
            .into_iter()
            .map(|(id, dist)| VectorSearchResult {
                vector_id: id,
                node_id: id, // Will be mapped externally
                distance: dist,
                similarity: 1.0 - dist, // For cosine
            })
            .collect()
    }

    fn find_nearest_centroid(&self, vector: &[f32]) -> usize {
        let mut best = 0;
        let mut best_dist = f32::MAX;
        
        for i in 0..self.config.n_clusters {
            let centroid = &self.centroids[i * self.dimensions..(i + 1) * self.dimensions];
            let dist = squared_euclidean(vector, centroid);
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        
        best
    }

    fn find_nearest_centroids(&self, vector: &[f32], n: usize) -> Vec<usize> {
        let mut distances: Vec<_> = (0..self.config.n_clusters)
            .map(|i| {
                let centroid = &self.centroids[i * self.dimensions..(i + 1) * self.dimensions];
                (i, squared_euclidean(vector, centroid))
            })
            .collect();
        
        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        distances.truncate(n);
        distances.into_iter().map(|(i, _)| i).collect()
    }
}

/// K-means clustering
fn kmeans(
    data: &[f32],
    dimensions: usize,
    k: usize,
    max_iterations: usize,
    _metric: &DistanceMetric,
) -> Vec<f32> {
    let n = data.len() / dimensions;
    
    // Initialize centroids (k-means++)
    let mut centroids = vec![0.0f32; k * dimensions];
    // ... k-means++ initialization
    
    // Iterate
    for _ in 0..max_iterations {
        // Assign points to clusters
        let mut assignments = vec![0usize; n];
        let mut counts = vec![0usize; k];
        
        for i in 0..n {
            let point = &data[i * dimensions..(i + 1) * dimensions];
            let mut best = 0;
            let mut best_dist = f32::MAX;
            
            for c in 0..k {
                let centroid = &centroids[c * dimensions..(c + 1) * dimensions];
                let dist = squared_euclidean(point, centroid);
                if dist < best_dist {
                    best_dist = dist;
                    best = c;
                }
            }
            
            assignments[i] = best;
            counts[best] += 1;
        }
        
        // Update centroids
        centroids.fill(0.0);
        for i in 0..n {
            let cluster = assignments[i];
            let point = &data[i * dimensions..(i + 1) * dimensions];
            for d in 0..dimensions {
                centroids[cluster * dimensions + d] += point[d];
            }
        }
        
        for c in 0..k {
            if counts[c] > 0 {
                let inv = 1.0 / counts[c] as f32;
                for d in 0..dimensions {
                    centroids[c * dimensions + d] *= inv;
                }
            }
        }
    }
    
    centroids
}
```

---

## Phase 6: High-Level API

### 6.1 Schema Definitions (`src/api/schema.rs`)

```rust
//! Drizzle-style schema definitions

use std::marker::PhantomData;

/// Property types
pub trait PropType: Sized {
    const TAG: PropValueTag;
    fn to_prop_value(self) -> PropValue;
    fn from_prop_value(pv: PropValue) -> Option<Self>;
}

impl PropType for String {
    const TAG: PropValueTag = PropValueTag::String;
    fn to_prop_value(self) -> PropValue { PropValue::String(self) }
    fn from_prop_value(pv: PropValue) -> Option<Self> {
        match pv { PropValue::String(s) => Some(s), _ => None }
    }
}

impl PropType for i64 {
    const TAG: PropValueTag = PropValueTag::I64;
    fn to_prop_value(self) -> PropValue { PropValue::I64(self) }
    fn from_prop_value(pv: PropValue) -> Option<Self> {
        match pv { PropValue::I64(i) => Some(i), _ => None }
    }
}

impl PropType for f64 {
    const TAG: PropValueTag = PropValueTag::F64;
    fn to_prop_value(self) -> PropValue { PropValue::F64(self) }
    fn from_prop_value(pv: PropValue) -> Option<Self> {
        match pv { PropValue::F64(f) => Some(f), _ => None }
    }
}

impl PropType for bool {
    const TAG: PropValueTag = PropValueTag::Bool;
    fn to_prop_value(self) -> PropValue { PropValue::Bool(self) }
    fn from_prop_value(pv: PropValue) -> Option<Self> {
        match pv { PropValue::Bool(b) => Some(b), _ => None }
    }
}

/// Property definition
pub struct PropDef<T: PropType, const OPTIONAL: bool = false> {
    pub name: &'static str,
    _marker: PhantomData<T>,
}

impl<T: PropType> PropDef<T, false> {
    pub const fn new(name: &'static str) -> Self {
        Self { name, _marker: PhantomData }
    }
    
    pub const fn optional(self) -> PropDef<T, true> {
        PropDef { name: self.name, _marker: PhantomData }
    }
}

/// Property builders
pub mod prop {
    use super::*;
    
    pub const fn string(name: &'static str) -> PropDef<String> {
        PropDef::new(name)
    }
    
    pub const fn int(name: &'static str) -> PropDef<i64> {
        PropDef::new(name)
    }
    
    pub const fn float(name: &'static str) -> PropDef<f64> {
        PropDef::new(name)
    }
    
    pub const fn bool(name: &'static str) -> PropDef<bool> {
        PropDef::new(name)
    }
}

/// Node definition trait (implement via macro)
pub trait NodeDef: Sized {
    const NAME: &'static str;
    type Key;
    
    fn key_fn(key: &Self::Key) -> String;
}

/// Edge definition trait
pub trait EdgeDef: Sized {
    const NAME: &'static str;
}

/// Macro for defining nodes
#[macro_export]
macro_rules! define_node {
    (
        $vis:vis $name:ident {
            key: |$key_arg:ident: $key_type:ty| $key_expr:expr,
            props: {
                $($prop_name:ident: $prop_def:expr),* $(,)?
            }
        }
    ) => {
        #[derive(Debug, Clone)]
        $vis struct $name {
            pub $key_arg: $key_type,
            $(pub $prop_name: <$prop_def as PropType>::Output,)*
        }
        
        impl NodeDef for $name {
            const NAME: &'static str = stringify!($name);
            type Key = $key_type;
            
            fn key_fn($key_arg: &Self::Key) -> String {
                $key_expr
            }
        }
    };
}

/// Macro for defining edges
#[macro_export]
macro_rules! define_edge {
    (
        $vis:vis $name:ident {
            $(props: {
                $($prop_name:ident: $prop_def:expr),* $(,)?
            })?
        }
    ) => {
        #[derive(Debug, Clone, Copy)]
        $vis struct $name;
        
        impl EdgeDef for $name {
            const NAME: &'static str = stringify!($name);
        }
    };
    
    ($vis:vis $name:ident) => {
        #[derive(Debug, Clone, Copy)]
        $vis struct $name;
        
        impl EdgeDef for $name {
            const NAME: &'static str = stringify!($name);
        }
    };
}
```

### 6.2 Ray Database (`src/api/ray.rs`)

```rust
//! High-level Ray database API

use crate::graph::db::GraphDB;
use crate::api::schema::*;
use crate::api::builders::*;
use crate::api::traversal::*;

/// Ray database handle
pub struct Ray {
    db: GraphDB,
    etype_ids: HashMap<&'static str, ETypeId>,
    propkey_ids: HashMap<String, PropKeyId>,
}

impl Ray {
    /// Open a ray database
    pub fn open(path: impl AsRef<Path>, options: RayOptions) -> Result<Self> {
        let db = GraphDB::open(path, options.into())?;
        
        // Initialize schema
        let mut tx = begin_tx(&db);
        let mut etype_ids = HashMap::new();
        let mut propkey_ids = HashMap::new();
        
        // Define edge types
        for edge in &options.edges {
            let id = define_etype(&mut tx, edge.name());
            etype_ids.insert(edge.name(), id);
        }
        
        // Define property keys
        // ...
        
        commit(tx)?;
        
        Ok(Self { db, etype_ids, propkey_ids })
    }

    /// Insert a node
    pub fn insert<N: NodeDef>(&self) -> InsertBuilder<N> {
        InsertBuilder::new(&self.db, &self.propkey_ids)
    }

    /// Get a node by key
    pub fn get<N: NodeDef>(&self, key: &N::Key) -> Result<Option<NodeRef<N>>> {
        let full_key = N::key_fn(key);
        let node_id = get_node_by_key(&self.db, &full_key);
        
        node_id.map(|id| {
            // Load properties...
            todo!()
        }).transpose()
    }

    /// Start a traversal from a node
    pub fn from<N: NodeDef>(&self, node: NodeRef<N>) -> TraversalBuilder<N> {
        TraversalBuilder::new(&self.db, node, &self.etype_ids, &self.propkey_ids)
    }

    /// Link two nodes with an edge
    pub fn link<E: EdgeDef>(
        &self,
        src: impl Into<NodeRef<()>>,
        _edge: E,
        dst: impl Into<NodeRef<()>>,
    ) -> Result<()> {
        let src = src.into();
        let dst = dst.into();
        let etype = self.etype_ids.get(E::NAME)
            .ok_or_else(|| RayError::KeyNotFound(E::NAME.to_string()))?;
        
        let mut tx = begin_tx(&self.db);
        add_edge(&mut tx, src.id, *etype, dst.id);
        commit(tx)
    }

    /// Close the database
    pub fn close(self) -> Result<()> {
        self.db.close()
    }
}

/// Node reference
#[derive(Debug, Clone)]
pub struct NodeRef<N: NodeDef> {
    pub id: NodeId,
    pub key: String,
    _marker: PhantomData<N>,
}
```

---

## Phase 7: Testing & Benchmarks

### 7.1 Test Structure

```
tests/
├── snapshot.rs      # Snapshot read/write tests
├── wal.rs           # WAL append/recovery tests
├── delta.rs         # Delta operations tests
├── mvcc.rs          # MVCC isolation/conflict tests
├── vector.rs        # Vector search tests
├── api.rs           # High-level API tests
└── stress/
    ├── concurrency.rs
    ├── isolation.rs
    └── durability.rs
```

### 7.2 Benchmark Structure

```
benches/
├── snapshot.rs      # Snapshot loading, CSR access
├── traversal.rs     # Graph traversal performance
├── mvcc.rs          # MVCC overhead
├── vector.rs        # Vector search, distance calculations
└── compare_ts.rs    # Compare with TypeScript version
```

---

## Dependencies

```toml
[dependencies]
# Core
thiserror = "1.0"
parking_lot = "0.12"
crossbeam = "0.8"
bitflags = "2.4"

# Binary/IO
byteorder = "1.5"
memmap2 = "0.9"
fs2 = "0.4"  # File locking

# Hashing/Checksums
crc32fast = "1.3"
xxhash-rust = { version = "0.8", features = ["xxh64"] }

# Compression
zstd = "0.13"
flate2 = "1.0"

# Collections
hashbrown = "0.14"
indexmap = "2.1"

# Numerics (for vectors)
ordered-float = "4.2"

# Optional: async support
tokio = { version = "1.0", features = ["rt-multi-thread", "sync"], optional = true }

[dev-dependencies]
criterion = "0.5"
rand = "0.8"
tempfile = "3.10"

[features]
default = []
async = ["tokio"]
simd = []  # Enable explicit SIMD (requires nightly)
```

---

## Binary Compatibility

The Rust implementation should be **binary-compatible** with the TypeScript version:

### File Formats to Match

1. **Snapshot format (.gds)**
   - Same magic number: `0x31534447` ("GDS1")
   - Same section table layout
   - Same CSR encoding
   - Same compression options

2. **WAL format (.gdw)**
   - Same magic number: `0x31574447` ("GDW1")
   - Same record framing (8-byte aligned)
   - Same CRC32C coverage

3. **Single-file format (.raydb)**
   - Same magic: "RayDB format 1\0"
   - Same header layout
   - Same page-based structure

4. **Manifest format (.gdm)**
   - Same magic: `0x4D424447` ("GDBM")
   - Same field layout

### Endianness

All multi-byte integers are stored **little-endian** (same as TypeScript DataView default).

### Alignment

- Sections: 64-byte aligned
- WAL records: 8-byte aligned
- Pages: 4KB aligned (configurable)

---

## Implementation Order

### Milestone 1: Read-Only (2-3 weeks)
1. Types, constants, error handling
2. Binary utilities, CRC, xxHash
3. Snapshot reader (mmap, CSR)
4. Basic node/edge reads
5. Key index lookup

### Milestone 2: Write Support (2-3 weeks)
1. Delta state
2. WAL writing
3. Transaction handling
4. Node/edge CRUD
5. Single-file format

### Milestone 3: MVCC (1-2 weeks)
1. Version chains
2. Visibility rules
3. Conflict detection
4. Garbage collection

### Milestone 4: Vector Search (2-3 weeks)
1. Distance functions (SIMD)
2. Columnar store
3. IVF index
4. k-means clustering
5. Search API

### Milestone 5: High-Level API (1-2 weeks)
1. Schema definitions (macros)
2. Query builders
3. Traversal API
4. Pathfinding

### Milestone 6: Polish (1 week)
1. Benchmarks
2. Documentation
3. Examples
4. CI/CD

---

## Notes

### Rust-Specific Optimizations

1. **Zero-copy parsing**: Use `zerocopy` or raw pointers for CSR section access
2. **SIMD distance**: Use `std::simd` (nightly) or portable-simd when stable
3. **Arena allocation**: Consider `bumpalo` for transaction-scoped allocations
4. **Custom collections**: Use `hashbrown` for faster hash maps
5. **Lock-free counters**: Use `AtomicU64` for ID allocation

### API Ergonomics

1. **Builder pattern**: Fluent API using Rust's ownership
2. **Type-state pattern**: Compile-time transaction state verification
3. **Derive macros**: Auto-generate schema traits
4. **Error handling**: Use `?` operator throughout

### Safety Considerations

1. **Unsafe mmap**: Wrap in safe abstractions
2. **Concurrent access**: Use `parking_lot` for lower overhead
3. **File locking**: Use `fs2` for cross-platform locks
4. **Crash recovery**: Validate all data on load

---

## Phase 8: Language Bindings

> **Note**: This phase should only be started after the full Rust implementation is complete and stable. Language bindings depend on a finalized API surface.

### 8.1 Overview

Once RayDB is fully ported to Rust, we expose it to other languages via native bindings:

| Language | Binding Technology | Package Name |
|----------|-------------------|--------------|
| TypeScript/JavaScript | [NAPI-RS](https://napi.rs/) | `@raydb/core` |
| Python | [PyO3](https://pyo3.rs/) + [Maturin](https://www.maturin.rs/) | `raydb` |

### 8.2 Project Structure

```
ray-rs/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── raydb-core/         # Core Rust library (existing)
│   │   └── Cargo.toml
│   ├── raydb-napi/         # Node.js/Bun bindings
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── db.rs       # Database handle wrapper
│   │   │   ├── node.rs     # Node operations
│   │   │   ├── edge.rs     # Edge operations
│   │   │   ├── query.rs    # Query builders
│   │   │   ├── vector.rs   # Vector search
│   │   │   └── types.rs    # Type conversions
│   │   ├── index.d.ts      # TypeScript declarations
│   │   └── package.json
│   └── raydb-python/       # Python bindings
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs
│       │   ├── db.rs
│       │   ├── node.rs
│       │   ├── edge.rs
│       │   ├── query.rs
│       │   ├── vector.rs
│       │   └── types.rs
│       ├── python/
│       │   └── raydb/
│       │       ├── __init__.py
│       │       └── py.typed  # PEP 561 marker
│       └── pyproject.toml
```

### 8.3 TypeScript/JavaScript Bindings (NAPI-RS)

#### Dependencies (`raydb-napi/Cargo.toml`)

```toml
[package]
name = "raydb-napi"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
raydb-core = { path = "../raydb-core" }
napi = { version = "2", features = ["async", "serde-json"] }
napi-derive = "2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[build-dependencies]
napi-build = "2"
```

#### Core Bindings (`raydb-napi/src/lib.rs`)

```rust
#![deny(clippy::all)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use raydb_core::{GraphDB, OpenOptions as CoreOptions};
use std::sync::Arc;
use parking_lot::RwLock;

mod db;
mod node;
mod edge;
mod query;
mod vector;
mod types;

pub use db::*;
pub use node::*;
pub use edge::*;
pub use query::*;
pub use vector::*;
```

#### Database Handle (`raydb-napi/src/db.rs`)

```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;
use raydb_core::{GraphDB, OpenOptions as CoreOptions};
use std::sync::Arc;
use parking_lot::RwLock;

/// Database open options
#[napi(object)]
pub struct OpenOptions {
    pub read_only: Option<bool>,
    pub create_if_missing: Option<bool>,
    pub mvcc: Option<bool>,
    pub mvcc_gc_interval_ms: Option<u32>,
    pub mvcc_retention_ms: Option<u32>,
    pub auto_checkpoint: Option<bool>,
    pub checkpoint_threshold: Option<f64>,
}

/// RayDB database handle
#[napi]
pub struct RayDB {
    inner: Arc<RwLock<GraphDB>>,
}

#[napi]
impl RayDB {
    /// Open or create a database
    #[napi(factory)]
    pub fn open(path: String, options: Option<OpenOptions>) -> Result<Self> {
        let opts = options.map(|o| CoreOptions {
            read_only: o.read_only.unwrap_or(false),
            create_if_missing: o.create_if_missing.unwrap_or(true),
            mvcc: o.mvcc.unwrap_or(false),
            mvcc_gc_interval_ms: o.mvcc_gc_interval_ms.unwrap_or(60000) as u64,
            mvcc_retention_ms: o.mvcc_retention_ms.unwrap_or(300000) as u64,
            auto_checkpoint: o.auto_checkpoint.unwrap_or(true),
            checkpoint_threshold: o.checkpoint_threshold.unwrap_or(0.1),
            ..Default::default()
        }).unwrap_or_default();

        let db = GraphDB::open(&path, opts)
            .map_err(|e| Error::from_reason(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(RwLock::new(db)),
        })
    }

    /// Close the database
    #[napi]
    pub fn close(&self) -> Result<()> {
        // Note: Actual close happens on drop
        Ok(())
    }

    /// Create a node
    #[napi]
    pub fn create_node(&self, key: Option<String>, props: Option<serde_json::Value>) -> Result<i64> {
        let mut db = self.inner.write();
        let mut tx = raydb_core::begin_tx(&db);
        
        let node_id = raydb_core::create_node(&mut tx, raydb_core::NodeOpts {
            key,
            props: props.map(|p| types::json_to_props(&db, p)).transpose()?,
            ..Default::default()
        });
        
        raydb_core::commit(tx)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        
        Ok(node_id as i64)
    }

    /// Get node by key
    #[napi]
    pub fn get_node_by_key(&self, key: String) -> Option<i64> {
        let db = self.inner.read();
        raydb_core::get_node_by_key(&db, &key).map(|id| id as i64)
    }

    /// Add an edge
    #[napi]
    pub fn add_edge(&self, src: i64, edge_type: String, dst: i64) -> Result<()> {
        let mut db = self.inner.write();
        let mut tx = raydb_core::begin_tx(&db);
        
        let etype = raydb_core::get_or_create_etype(&mut tx, &edge_type);
        raydb_core::add_edge(&mut tx, src as u64, etype, dst as u64);
        
        raydb_core::commit(tx)
            .map_err(|e| Error::from_reason(e.to_string()))
    }

    /// Get neighbors
    #[napi]
    pub fn get_neighbors(&self, node_id: i64, direction: String, edge_type: Option<String>) -> Result<Vec<i64>> {
        let db = self.inner.read();
        let etype = edge_type.map(|et| raydb_core::get_etype_id(&db, &et)).flatten();
        
        let neighbors: Vec<i64> = match direction.as_str() {
            "out" => raydb_core::get_neighbors_out(&db, node_id as u64, etype)
                .map(|e| e.dst as i64)
                .collect(),
            "in" => raydb_core::get_neighbors_in(&db, node_id as u64, etype)
                .map(|e| e.src as i64)
                .collect(),
            "both" => {
                let mut result: Vec<i64> = raydb_core::get_neighbors_out(&db, node_id as u64, etype)
                    .map(|e| e.dst as i64)
                    .collect();
                result.extend(
                    raydb_core::get_neighbors_in(&db, node_id as u64, etype)
                        .map(|e| e.src as i64)
                );
                result
            }
            _ => return Err(Error::from_reason("Invalid direction: use 'in', 'out', or 'both'")),
        };
        
        Ok(neighbors)
    }

    /// Vector search
    #[napi]
    pub fn vector_search(
        &self,
        query: Vec<f64>,
        k: u32,
        options: Option<VectorSearchOptions>,
    ) -> Result<Vec<VectorSearchResult>> {
        let db = self.inner.read();
        let query_f32: Vec<f32> = query.into_iter().map(|v| v as f32).collect();
        
        let results = raydb_core::vector_search(&db, &query_f32, k as usize, options.map(Into::into))
            .map_err(|e| Error::from_reason(e.to_string()))?;
        
        Ok(results.into_iter().map(Into::into).collect())
    }
}

#[napi(object)]
pub struct VectorSearchOptions {
    pub n_probe: Option<u32>,
    pub ef_search: Option<u32>,
}

#[napi(object)]
pub struct VectorSearchResult {
    pub node_id: i64,
    pub distance: f64,
    pub similarity: f64,
}
```

#### TypeScript Declarations (`raydb-napi/index.d.ts`)

```typescript
export interface OpenOptions {
  readOnly?: boolean;
  createIfMissing?: boolean;
  mvcc?: boolean;
  mvccGcIntervalMs?: number;
  mvccRetentionMs?: number;
  autoCheckpoint?: boolean;
  checkpointThreshold?: number;
}

export interface VectorSearchOptions {
  nProbe?: number;
  efSearch?: number;
}

export interface VectorSearchResult {
  nodeId: number;
  distance: number;
  similarity: number;
}

export class RayDB {
  static open(path: string, options?: OpenOptions): RayDB;
  close(): void;
  createNode(key?: string, props?: Record<string, unknown>): number;
  getNodeByKey(key: string): number | null;
  addEdge(src: number, edgeType: string, dst: number): void;
  getNeighbors(nodeId: number, direction: 'in' | 'out' | 'both', edgeType?: string): number[];
  vectorSearch(query: number[], k: number, options?: VectorSearchOptions): VectorSearchResult[];
}
```

### 8.4 Python Bindings (PyO3)

#### Dependencies (`raydb-python/Cargo.toml`)

```toml
[package]
name = "raydb-python"
version = "0.1.0"
edition = "2021"

[lib]
name = "raydb"
crate-type = ["cdylib"]

[dependencies]
raydb-core = { path = "../raydb-core" }
pyo3 = { version = "0.20", features = ["extension-module"] }
numpy = "0.20"  # For efficient array handling

[build-dependencies]
pyo3-build-config = "0.20"
```

#### Core Bindings (`raydb-python/src/lib.rs`)

```rust
use pyo3::prelude::*;
use pyo3::exceptions::PyValueError;
use raydb_core::{GraphDB, OpenOptions as CoreOptions};
use std::sync::Arc;
use parking_lot::RwLock;
use numpy::{PyArray1, PyReadonlyArray1};

mod db;
mod node;
mod edge;
mod query;
mod vector;
mod types;

/// RayDB Python module
#[pymodule]
fn raydb(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RayDB>()?;
    m.add_class::<OpenOptions>()?;
    m.add_class::<VectorSearchResult>()?;
    Ok(())
}
```

#### Database Handle (`raydb-python/src/db.rs`)

```rust
use pyo3::prelude::*;
use pyo3::exceptions::PyValueError;
use raydb_core::{GraphDB, OpenOptions as CoreOptions};
use std::sync::Arc;
use parking_lot::RwLock;
use numpy::{PyArray1, PyReadonlyArray1};

/// Database open options
#[pyclass]
#[derive(Clone, Default)]
pub struct OpenOptions {
    #[pyo3(get, set)]
    pub read_only: bool,
    #[pyo3(get, set)]
    pub create_if_missing: bool,
    #[pyo3(get, set)]
    pub mvcc: bool,
    #[pyo3(get, set)]
    pub mvcc_gc_interval_ms: u64,
    #[pyo3(get, set)]
    pub mvcc_retention_ms: u64,
    #[pyo3(get, set)]
    pub auto_checkpoint: bool,
    #[pyo3(get, set)]
    pub checkpoint_threshold: f64,
}

#[pymethods]
impl OpenOptions {
    #[new]
    fn new() -> Self {
        Self {
            read_only: false,
            create_if_missing: true,
            mvcc: false,
            mvcc_gc_interval_ms: 60000,
            mvcc_retention_ms: 300000,
            auto_checkpoint: true,
            checkpoint_threshold: 0.1,
        }
    }
}

/// RayDB database handle
#[pyclass]
pub struct RayDB {
    inner: Arc<RwLock<GraphDB>>,
}

#[pymethods]
impl RayDB {
    /// Open or create a database
    #[new]
    #[pyo3(signature = (path, options=None))]
    fn new(path: &str, options: Option<OpenOptions>) -> PyResult<Self> {
        let opts = options.map(|o| CoreOptions {
            read_only: o.read_only,
            create_if_missing: o.create_if_missing,
            mvcc: o.mvcc,
            mvcc_gc_interval_ms: o.mvcc_gc_interval_ms,
            mvcc_retention_ms: o.mvcc_retention_ms,
            auto_checkpoint: o.auto_checkpoint,
            checkpoint_threshold: o.checkpoint_threshold,
            ..Default::default()
        }).unwrap_or_default();

        let db = GraphDB::open(path, opts)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(RwLock::new(db)),
        })
    }

    /// Close the database
    fn close(&self) -> PyResult<()> {
        Ok(())
    }

    /// Create a node
    #[pyo3(signature = (key=None, props=None))]
    fn create_node(&self, key: Option<String>, props: Option<&PyDict>) -> PyResult<i64> {
        let mut db = self.inner.write();
        let mut tx = raydb_core::begin_tx(&db);
        
        let node_id = raydb_core::create_node(&mut tx, raydb_core::NodeOpts {
            key,
            props: props.map(|p| types::dict_to_props(&db, p)).transpose()?,
            ..Default::default()
        });
        
        raydb_core::commit(tx)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        
        Ok(node_id as i64)
    }

    /// Get node by key
    fn get_node_by_key(&self, key: &str) -> Option<i64> {
        let db = self.inner.read();
        raydb_core::get_node_by_key(&db, key).map(|id| id as i64)
    }

    /// Add an edge
    fn add_edge(&self, src: i64, edge_type: &str, dst: i64) -> PyResult<()> {
        let mut db = self.inner.write();
        let mut tx = raydb_core::begin_tx(&db);
        
        let etype = raydb_core::get_or_create_etype(&mut tx, edge_type);
        raydb_core::add_edge(&mut tx, src as u64, etype, dst as u64);
        
        raydb_core::commit(tx)
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// Get neighbors
    #[pyo3(signature = (node_id, direction, edge_type=None))]
    fn get_neighbors(&self, node_id: i64, direction: &str, edge_type: Option<&str>) -> PyResult<Vec<i64>> {
        let db = self.inner.read();
        let etype = edge_type.map(|et| raydb_core::get_etype_id(&db, et)).flatten();
        
        let neighbors: Vec<i64> = match direction {
            "out" => raydb_core::get_neighbors_out(&db, node_id as u64, etype)
                .map(|e| e.dst as i64)
                .collect(),
            "in" => raydb_core::get_neighbors_in(&db, node_id as u64, etype)
                .map(|e| e.src as i64)
                .collect(),
            "both" => {
                let mut result: Vec<i64> = raydb_core::get_neighbors_out(&db, node_id as u64, etype)
                    .map(|e| e.dst as i64)
                    .collect();
                result.extend(
                    raydb_core::get_neighbors_in(&db, node_id as u64, etype)
                        .map(|e| e.src as i64)
                );
                result
            }
            _ => return Err(PyValueError::new_err("Invalid direction: use 'in', 'out', or 'both'")),
        };
        
        Ok(neighbors)
    }

    /// Vector search with numpy array support
    #[pyo3(signature = (query, k, n_probe=None))]
    fn vector_search<'py>(
        &self,
        py: Python<'py>,
        query: PyReadonlyArray1<f32>,
        k: usize,
        n_probe: Option<usize>,
    ) -> PyResult<Vec<VectorSearchResult>> {
        let db = self.inner.read();
        let query_slice = query.as_slice()?;
        
        let results = raydb_core::vector_search(&db, query_slice, k, n_probe.map(|np| raydb_core::VectorSearchOpts { n_probe: np, ..Default::default() }))
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        
        Ok(results.into_iter().map(Into::into).collect())
    }

    /// Context manager support
    fn __enter__(slf: PyRef<Self>) -> PyRef<Self> {
        slf
    }

    fn __exit__(&self, _exc_type: Option<&PyAny>, _exc_val: Option<&PyAny>, _exc_tb: Option<&PyAny>) -> bool {
        let _ = self.close();
        false
    }
}

#[pyclass]
pub struct VectorSearchResult {
    #[pyo3(get)]
    pub node_id: i64,
    #[pyo3(get)]
    pub distance: f64,
    #[pyo3(get)]
    pub similarity: f64,
}
```

#### Python Type Stubs (`raydb-python/python/raydb/__init__.pyi`)

```python
from typing import Optional, Dict, Any, List, Literal
import numpy as np
import numpy.typing as npt

class OpenOptions:
    read_only: bool
    create_if_missing: bool
    mvcc: bool
    mvcc_gc_interval_ms: int
    mvcc_retention_ms: int
    auto_checkpoint: bool
    checkpoint_threshold: float
    
    def __init__(self) -> None: ...

class VectorSearchResult:
    node_id: int
    distance: float
    similarity: float

class RayDB:
    def __init__(self, path: str, options: Optional[OpenOptions] = None) -> None: ...
    def close(self) -> None: ...
    def create_node(self, key: Optional[str] = None, props: Optional[Dict[str, Any]] = None) -> int: ...
    def get_node_by_key(self, key: str) -> Optional[int]: ...
    def add_edge(self, src: int, edge_type: str, dst: int) -> None: ...
    def get_neighbors(
        self, 
        node_id: int, 
        direction: Literal["in", "out", "both"], 
        edge_type: Optional[str] = None
    ) -> List[int]: ...
    def vector_search(
        self,
        query: npt.NDArray[np.float32],
        k: int,
        n_probe: Optional[int] = None,
    ) -> List[VectorSearchResult]: ...
    
    def __enter__(self) -> "RayDB": ...
    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> bool: ...
```

### 8.5 Build & Distribution

#### Node.js/Bun (via npm)

```bash
# Install napi-rs CLI
npm install -g @napi-rs/cli

# Build for current platform
cd crates/raydb-napi
napi build --release

# Build for all platforms (CI)
napi build --release --platform
```

**Supported targets:**
- `x86_64-apple-darwin` (macOS Intel)
- `aarch64-apple-darwin` (macOS Apple Silicon)
- `x86_64-unknown-linux-gnu` (Linux x64)
- `aarch64-unknown-linux-gnu` (Linux ARM64)
- `x86_64-pc-windows-msvc` (Windows x64)

#### Python (via maturin)

```bash
# Install maturin
pip install maturin

# Build wheel for current platform
cd crates/raydb-python
maturin build --release

# Build and install locally
maturin develop

# Publish to PyPI
maturin publish
```

### 8.6 Implementation Timeline

| Task | Duration | Dependencies |
|------|----------|--------------|
| Setup workspace structure | 1 day | Rust implementation complete |
| NAPI-RS scaffolding | 2 days | - |
| Core JS bindings (open, close, CRUD) | 3 days | NAPI scaffolding |
| JS vector search bindings | 2 days | Core JS bindings |
| JS TypeScript declarations | 1 day | All JS bindings |
| PyO3 scaffolding | 2 days | - |
| Core Python bindings | 3 days | PyO3 scaffolding |
| Python numpy integration | 2 days | Core Python bindings |
| Python type stubs | 1 day | All Python bindings |
| CI/CD for multi-platform builds | 2 days | All bindings |
| Documentation & examples | 2 days | All bindings |
| **Total** | **~3 weeks** | |

### 8.7 Milestones Update

Add to the existing milestones:

#### Milestone 7: Language Bindings (3 weeks)

> **Prerequisites**: Milestones 1-6 complete and API stable

1. **Week 1**: NAPI-RS bindings
   - Project scaffolding
   - Core operations (open, create node, add edge)
   - Query and traversal
   - TypeScript declarations

2. **Week 2**: PyO3 bindings
   - Project scaffolding with maturin
   - Core operations
   - NumPy integration for vectors
   - Type stubs (`.pyi` files)

3. **Week 3**: Polish & Distribution
   - Multi-platform CI builds
   - npm/PyPI publishing
   - Documentation
   - Examples in both languages
