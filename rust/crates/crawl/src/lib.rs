//! # Crawl
//!
//! Ties the other crates together: the [`frontier`] decides which URL is
//! fetched next and the [`engine`] runs fetch/extract/classify workers
//! against it and streams hits into the store.

pub mod engine;
pub mod frontier;

pub use engine::{CrawlOptions, run};
