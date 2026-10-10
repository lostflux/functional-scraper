//! # Extract
//!
//! Pure functions from fetched bytes to structured data:
//!
//! * [`html`] — title, article text, date, language and links of a page.
//! * [`xml`] — URLs listed in sitemaps and RSS/Atom feeds.
//! * [`dates`] — lenient date parsing shared by both.
//! * [`classify`] — the keyword rule that decides whether a page is a hit.

pub mod classify;
pub mod dates;
pub mod html;
pub mod xml;

pub use classify::{Classifier, Relevance, StrongRule};

/// Builds the classifier described by a [`scraper_core::Config`].
pub fn classifier_for(cfg: &scraper_core::Config) -> Classifier {
  Classifier::with_strong(
    &cfg.targets,
    cfg.wordcount,
    &cfg.strong_targets,
    StrongRule {
      min_mentions: cfg.crawl.min_strong_mentions,
      require_lede: cfg.crawl.require_strong_in_lede,
      lede_chars: cfg.crawl.lede_chars,
      dense_mentions: cfg.crawl.dense_strong_mentions,
      dense_per_1000: cfg.crawl.dense_strong_per_1000,
      dense_distinct: cfg.crawl.dense_strong_distinct,
    },
  )
}
pub use html::Page;
pub use xml::FeedLink;
