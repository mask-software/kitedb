//! Caching layer

pub mod lru;
pub mod manager;
pub mod property;
pub mod query;
pub mod traversal;

/// b4 cache lane: shows the key cache is never consulted (failing).
#[cfg(test)]
#[path = "b4_cache_tests.rs"]
mod b4_cache_tests;
