//! Crawl configuration, loaded from the shared `config.yml`.
//!
//! The top-level keys `targets`, `domains`, `limit` and `wordcount` are the
//! ones the legacy Haskell implementation reads, so the same file drives both.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
  /// Keywords; a page is a hit when at least `wordcount` distinct ones appear.
  pub targets: Vec<String>,
  /// Allowed sites (`scope: domains`) and default seeds. Each entry is a URL
  /// whose host (and optional path prefix) defines what is in scope.
  pub domains: Vec<String>,
  /// Extra seed URLs: pages, RSS/Atom feeds or sitemaps. Seeds are always
  /// fetched, even if seen before.
  #[serde(default)]
  pub seeds: Vec<String>,
  /// Minimum number of distinct `targets` for a page to count as a hit.
  #[serde(default = "default_wordcount")]
  pub wordcount: usize,
  /// Terms only an AI story uses. When non-empty a page must also satisfy
  /// the strong rule (see `crawl.min_strong_mentions` and friends).
  #[serde(default)]
  pub strong_targets: Vec<String>,
  /// Legacy alias for `crawl.max_pages`.
  #[serde(default)]
  pub limit: Option<usize>,
  #[serde(default)]
  pub crawl: CrawlConfig,
  /// Substrings; a URL containing any of them is never followed.
  #[serde(default = "default_excludes")]
  pub exclude_patterns: Vec<String>,
  /// Hosts (suffix match) never followed in `scope: open` mode.
  #[serde(default = "default_deny_hosts")]
  pub deny_hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
  /// Only follow links whose host matches an entry of `domains`.
  Domains,
  /// Follow links to any host; relevance is enforced by the focus policy.
  Open,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CrawlConfig {
  pub scope: Scope,
  /// Total concurrent fetches.
  pub concurrency: usize,
  /// Concurrent fetches per host.
  pub per_host_concurrency: usize,
  /// Minimum gap between two requests to the same host.
  pub per_host_delay_ms: u64,
  /// Links deeper than this are not followed.
  pub max_depth: u32,
  /// Stop after this many fetch attempts in one run. Falls back to the
  /// legacy top-level `limit`, then 20000.
  pub max_pages: Option<usize>,
  /// Stop after this many hits in one run.
  pub max_hits: Option<usize>,
  /// Stop after this much wall-clock time.
  pub max_duration_secs: Option<u64>,
  /// Non-hit pages are refetched once this many days have passed.
  pub revisit_after_days: u32,
  /// Queue cap per host; further links are dropped.
  pub max_queue_per_host: usize,
  /// Fetch cap per host per run (keeps one site from dominating).
  pub max_pages_per_host: usize,
  /// In open scope: after `host_probation_pages` fetches a host needs at
  /// least `host_min_hit_ratio` hits/fetches or it is dropped.
  pub host_probation_pages: usize,
  pub host_min_hit_ratio: f64,
  /// In open scope: only follow links from pages that are hits.
  pub follow_only_from_hits: bool,
  /// Also enqueue sitemaps advertised in robots.txt of seed hosts.
  pub follow_robots_sitemaps: bool,
  /// Fetch the seeds (feeds, sitemaps, home pages) again this often during
  /// a run, so long crawls keep finding fresh articles. `null` disables.
  pub reseed_every_secs: Option<u64>,
  /// Give hosts turns in proportion to their recent hit ratio.
  pub yield_weighting: bool,
  /// Fetches a host gets before yield weighting applies to it.
  pub yield_warmup_pages: usize,
  /// Smallest share of turns a low-yield host keeps (0..1).
  pub min_host_weight: f64,
  /// Sitemap entries with a `lastmod` before this date (YYYY-MM-DD) are skipped.
  pub min_lastmod: Option<String>,
  /// From a sitemap index, follow at most this many child sitemaps (newest
  /// by `lastmod` first). Keeps paged indexes with thousands of entries from
  /// eating a host's page budget.
  pub max_sitemaps_per_index: usize,
  pub respect_robots: bool,
  pub user_agent: String,
  pub timeout_secs: u64,
  pub max_body_bytes: usize,
  /// Pages with less extracted text than this are never hits.
  pub min_text_chars: usize,
  /// Skip pages whose `<html lang>` is set and is not English.
  pub english_only: bool,
  /// Only store pages with an article signal (`og:type`, dated metadata
  /// or a dated URL), so listing and tag pages are followed but not kept.
  pub require_article_signal: bool,
  /// Pages whose text is less than this fraction paragraph-length blocks
  /// are treated as listings, not articles (see `Page::prose_ratio`).
  pub min_prose_ratio: f64,
  /// Strong-term rule: mentions needed across title and text.
  pub min_strong_mentions: usize,
  /// Strong-term rule: a strong term must appear in the title or the lede
  /// (first `lede_chars`), unless the body is dense with them.
  pub require_strong_in_lede: bool,
  pub lede_chars: usize,
  pub dense_strong_mentions: usize,
  pub dense_strong_per_1000: f64,
  pub dense_strong_distinct: usize,
  /// Do not follow links from pages published before `min_lastmod`, so old
  /// archives do not pull the crawl into the past.
  pub follow_links_from_old_pages: bool,
}

impl Default for CrawlConfig {
  fn default() -> Self {
    Self {
      scope: Scope::Domains,
      concurrency: 32,
      per_host_concurrency: 2,
      per_host_delay_ms: 750,
      max_depth: 4,
      max_pages: None,
      max_hits: None,
      max_duration_secs: None,
      revisit_after_days: 7,
      max_queue_per_host: 50_000,
      max_pages_per_host: 5_000,
      host_probation_pages: 40,
      host_min_hit_ratio: 0.05,
      follow_only_from_hits: true,
      follow_robots_sitemaps: true,
      reseed_every_secs: Some(3600),
      yield_weighting: true,
      yield_warmup_pages: 30,
      min_host_weight: 0.1,
      min_lastmod: None,
      max_sitemaps_per_index: 50,
      respect_robots: true,
      user_agent: DEFAULT_USER_AGENT.into(),
      timeout_secs: 20,
      max_body_bytes: 5 * 1024 * 1024,
      min_text_chars: 400,
      english_only: true,
      require_article_signal: true,
      min_prose_ratio: 0.55,
      min_strong_mentions: 3,
      require_strong_in_lede: true,
      lede_chars: 600,
      dense_strong_mentions: 12,
      dense_strong_per_1000: 5.0,
      dense_strong_distinct: 2,
      follow_links_from_old_pages: false,
    }
  }
}

/// A current desktop browser string; override with `crawl.user_agent`.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

fn default_wordcount() -> usize {
  5
}

fn default_excludes() -> Vec<String> {
  [
    "/author/",
    "/tag/",
    "/editor/",
    "/wp-json/",
    "/login",
    "/signin",
    "/sign-in",
    "/account",
    "/cart",
    "/checkout",
    "/search",
    "/newsletter",
    "/subscribe",
  ]
  .into_iter()
  .map(String::from)
  .collect()
}

fn default_deny_hosts() -> Vec<String> {
  [
    "facebook.com",
    "fb.com",
    "twitter.com",
    "x.com",
    "t.co",
    "instagram.com",
    "linkedin.com",
    "youtube.com",
    "youtu.be",
    "tiktok.com",
    "pinterest.com",
    "reddit.com",
    "snapchat.com",
    "threads.net",
    "whatsapp.com",
    "google.com",
    "googleapis.com",
    "gstatic.com",
    "doubleclick.net",
    "bing.com",
    "yahoo.com",
    "duckduckgo.com",
    "amazon.com",
    "amzn.to",
    "apple.com",
    "itunes.apple.com",
    "play.google.com",
    "ebay.com",
    "shopify.com",
    "github.com",
    "gitlab.com",
    "wikipedia.org",
    "wikimedia.org",
    "archive.org",
    "cloudflare.com",
    "wp.com",
    "wordpress.com",
    "disqus.com",
    "outbrain.com",
    "taboola.com",
    "spotify.com",
    "soundcloud.com",
    "vimeo.com",
    "flickr.com",
    "imgur.com",
    "tumblr.com",
    "medium.com",
    "substack.com",
    "paypal.com",
    "stripe.com",
  ]
  .into_iter()
  .map(String::from)
  .collect()
}

impl Config {
  /// Fetch budget per run: `crawl.max_pages`, else legacy `limit`, else 20000.
  pub fn max_pages(&self) -> usize {
    self.crawl.max_pages.or(self.limit).unwrap_or(20_000)
  }

  pub fn load(path: &Path) -> Result<Self> {
    let raw = std::fs::read_to_string(path)
      .with_context(|| format!("reading {}", path.display()))?;
    let mut cfg: Config = serde_yaml::from_str(&raw)
      .with_context(|| format!("parsing {}", path.display()))?;
    let norm = |v: &[String]| -> Vec<String> {
      v.iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
    };
    cfg.targets = norm(&cfg.targets);
    cfg.strong_targets = norm(&cfg.strong_targets);
    Ok(cfg)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn legacy_keys_still_parse() {
    let cfg: Config = serde_yaml::from_str(
            "targets: [ai, Machine]\ndomains: [https://example.com]\nlimit: 10\nwordcount: 2\n",
        )
        .unwrap();
    assert_eq!(cfg.wordcount, 2);
    assert_eq!(cfg.limit, Some(10));
    assert_eq!(cfg.max_pages(), 10);
    assert_eq!(cfg.crawl.scope, Scope::Domains);
    assert_eq!(cfg.crawl.concurrency, 32);
  }

  #[test]
  fn nested_crawl_block() {
    let cfg: Config = serde_yaml::from_str(
      "targets: [ai]\ndomains: []\ncrawl:\n  scope: open\n  concurrency: 4\n",
    )
    .unwrap();
    assert_eq!(cfg.crawl.scope, Scope::Open);
    assert_eq!(cfg.crawl.concurrency, 4);
    assert_eq!(cfg.crawl.per_host_concurrency, 2);
  }
}
