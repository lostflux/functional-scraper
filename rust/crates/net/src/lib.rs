//! # Net
//!
//! The HTTP side of the crawler: a shared client, `robots.txt` lookups and
//! body classification. See [`fetch::Fetcher`].

pub mod fetch;

pub use fetch::{Fetched, Fetcher, RobotsInfo};
