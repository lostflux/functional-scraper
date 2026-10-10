//! # Store
//!
//! Everything the crawler persists under the data directory:
//!
//! * [`seen`] — visited URLs as 64-bit hashes in an append-only file.
//! * [`shards`] — article records in per-run `jsonl.zst` shards, plus the
//!   id counter, run summaries and the resumable frontier.

pub mod seen;
pub mod shards;

pub use seen::SeenSet;
pub use shards::{HostStats, Progress, Record, RunSummary, SavedJob, Store};
