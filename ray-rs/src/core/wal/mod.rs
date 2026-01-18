//! Write-Ahead Log
//!
//! WAL for durability and crash recovery

pub mod buffer;
pub mod reader;
pub mod record;
pub mod writer;
