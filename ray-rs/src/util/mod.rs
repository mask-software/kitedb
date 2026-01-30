//! Utility modules for RayDB
//!
//! Contains binary encoding, hashing, compression, and other helpers.

pub mod binary;
pub mod compression;
pub mod crc;
pub mod hash;
pub mod heap;
pub mod lock;
pub mod mmap;

// Re-export commonly used items
pub use binary::{align_up, padding_for, BufferBuilder};
pub use compression::{compress, decompress, CompressionType};
pub use crc::crc32c;
pub use hash::{xxhash64, xxhash64_string};
