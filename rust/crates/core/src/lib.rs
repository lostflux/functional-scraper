//! # Core
//!
//! Types every other crate agrees on: the crawl [`config::Config`] and the
//! URL normalisation, scoping and hashing rules in [`urls`].

pub mod config;
pub mod urls;

pub use config::{Config, CrawlConfig, Scope};
pub use urls::Scoper;
