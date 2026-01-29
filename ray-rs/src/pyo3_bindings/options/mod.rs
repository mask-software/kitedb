//! Configuration options for Python bindings
//!
//! This module contains all option structs for various database operations:
//! - Opening databases
//! - Maintenance (optimize, vacuum, compression)
//! - Streaming and pagination
//! - Backup and restore
//! - Export and import

pub mod backup;
pub mod export;
pub mod maintenance;
pub mod open;
pub mod streaming;

// Re-export all options for convenience
pub use backup::{BackupOptions, BackupResult, OfflineBackupOptions, RestoreOptions};
pub use export::{ExportOptions, ExportResult, ImportOptions, ImportResult};
pub use maintenance::{CompressionOptions, SingleFileOptimizeOptions, VacuumOptions};
pub use open::{OpenOptions, SyncMode};
pub use streaming::{PaginationOptions, StreamOptions};
