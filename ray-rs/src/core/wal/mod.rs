//! Write-Ahead Log
//!
//! WAL for durability and crash recovery

pub mod buffer;
pub mod record;

#[cfg(test)]
mod b4_wal_perf_tests;
