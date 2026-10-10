//! # scraper
//!
//! Command-line front end for the crawler.
//!
//! * `scraper crawl` — run a (budgeted, resumable) crawl.
//! * `scraper check-url <url>` — fetch one page and show what would be
//!   extracted; handy for tuning the extractor against a new site.
//! * `scraper stats` — what is stored and what the last run did.
//!
//! `config.yml` and `data/` are looked up in the current directory and then
//! its parent, so the binary works from the repo root or from `rust/`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use scraper_core::{Config, Scope};
use scraper_crawl::CrawlOptions;
use std::{path::PathBuf, time::Duration};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "scraper", version, about)]
struct Cli {
  /// Path to config.yml (default: ./config.yml or ../config.yml).
  #[arg(long, global = true)]
  config: Option<PathBuf>,
  /// Data directory holding log/, articles/ and state/ (default: ./data or ../data).
  #[arg(long, global = true)]
  data_dir: Option<PathBuf>,
  #[command(subcommand)]
  cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum ScopeArg {
  Domains,
  Open,
}

#[derive(Subcommand)]
enum Cmd {
  /// Run a crawl.
  Crawl {
    /// Stop after this many fetch attempts.
    #[arg(long)]
    max_pages: Option<usize>,
    /// Stop after this many stored articles.
    #[arg(long)]
    max_hits: Option<usize>,
    /// Stop after this much time, e.g. 30m or 2h.
    #[arg(long, value_parser = humantime::parse_duration)]
    max_duration: Option<Duration>,
    /// Ignore sitemap/feed entries older than this (YYYY-MM-DD, or "auto"
    /// for the day before the previous run).
    #[arg(long)]
    min_lastmod: Option<String>,
    /// Continue from the queue left by the previous run.
    #[arg(long)]
    resume: bool,
    /// Override crawl.scope.
    #[arg(long, value_enum)]
    scope: Option<ScopeArg>,
    /// Additional seed URL, page, feed or sitemap (repeatable).
    #[arg(long = "seed")]
    seeds: Vec<String>,
  },
  /// Fetch one URL and print what the extractor sees.
  CheckUrl {
    url: String,
    /// Also list the outgoing links that would be followed.
    #[arg(long)]
    links: bool,
    /// How many lines of text to print.
    #[arg(long, default_value_t = 12)]
    lines: usize,
  },
  /// Summarise stored shards, state and the last run.
  Stats,
}

fn locate(explicit: Option<PathBuf>, name: &str) -> Result<PathBuf> {
  if let Some(p) = explicit {
    return Ok(p);
  }
  for candidate in [PathBuf::from(name), PathBuf::from("..").join(name)] {
    if candidate.exists() {
      return Ok(candidate);
    }
  }
  let flag = if name == "data" {
    "--data-dir"
  } else {
    "--config"
  };
  bail!("could not find {name} in . or ..; pass {flag}")
}

#[tokio::main]
async fn main() -> Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
      EnvFilter::new("info,hyper=warn,reqwest=warn,html5ever=error")
    }))
    .with_target(false)
    .compact()
    .init();

  let cli = Cli::parse();
  let config_path = locate(cli.config, "config.yml")?;
  let cfg = Config::load(&config_path)?;

  match cli.cmd {
    Cmd::Crawl {
      max_pages,
      max_hits,
      max_duration,
      min_lastmod,
      resume,
      scope,
      seeds,
    } => {
      let data_dir = locate(cli.data_dir, "data")?;
      let min_lastmod = match min_lastmod.as_deref() {
        None => None,
        Some("auto") => {
          scraper_store::Store::last_run(&data_dir).and_then(|r| {
            let d = scraper_extract::dates::parse_date(&r.started_at)?;
            Some(d - chrono::Duration::days(1))
          })
        }
        Some(s) => Some(
          scraper_extract::dates::parse_date(s)
            .with_context(|| format!("--min-lastmod {s:?} is not a date"))?,
        ),
      };
      let opts = CrawlOptions {
        max_pages,
        max_hits,
        max_duration,
        min_lastmod,
        resume,
        extra_seeds: seeds,
        scope: scope.map(|s| match s {
          ScopeArg::Domains => Scope::Domains,
          ScopeArg::Open => Scope::Open,
        }),
      };
      scraper_crawl::run(cfg, &data_dir, opts).await?;
    }
    Cmd::CheckUrl { url, links, lines } => {
      check_url(&cfg, &url, links, lines).await?
    }
    Cmd::Stats => stats(&locate(cli.data_dir, "data")?)?,
  }
  Ok(())
}

async fn check_url(
  cfg: &Config,
  url: &str,
  show_links: bool,
  lines: usize,
) -> Result<()> {
  use scraper_net::{Fetched, Fetcher};
  let url = url::Url::parse(url)?;
  let fetcher = Fetcher::new(&cfg.crawl)?;
  let robots = fetcher.robots(&url).await;
  println!(
    "robots.txt: allowed={} delay={:?} sitemaps={}",
    robots.allowed(url.as_str()),
    robots.delay,
    robots.sitemaps.len()
  );
  let scoper = scraper_core::Scoper::new(cfg);
  match fetcher.fetch(&url).await {
    Fetched::Html {
      url: final_url,
      body,
    } => {
      let page = scraper_extract::html::parse_html(
        &final_url,
        &body,
        cfg.crawl.min_text_chars,
      );
      let classifier = scraper_extract::classifier_for(cfg);
      let rel = classifier.relevance(&page.title, &page.text);
      let kw = rel.keywords.clone();
      println!("final url:  {final_url}");
      println!("canonical:  {}", page.url);
      println!("title:      {}", page.title);
      println!("lang:       {}", page.lang.as_deref().unwrap_or("-"));
      println!("og:type:    {}", page.og_type.as_deref().unwrap_or("-"));
      println!(
        "date:       {} (year {}, via {})",
        page.date.as_deref().unwrap_or("-"),
        page.year.map_or("-".into(), |y| y.to_string()),
        page.date_source.unwrap_or("-")
      );
      println!(
        "article:    {} (prose ratio {:.2}, min {:.2})",
        page.looks_like_article(),
        page.prose_ratio(),
        cfg.crawl.min_prose_ratio
      );
      println!(
        "text:       {} chars, {} blocks",
        page.text_len(),
        page.text.lines().count()
      );
      println!(
        "keywords:   {}/{} needed: {}",
        kw.len(),
        classifier.min(),
        kw.join(", ")
      );
      let in_scope: Vec<_> =
        page.links.iter().filter(|l| scoper.allows(l)).collect();
      println!(
        "links:      {} total, {} in scope",
        page.links.len(),
        in_scope.len()
      );
      println!(
        "hit:        {}",
        classifier.is_hit(&rel)
          && page.text_len() >= cfg.crawl.min_text_chars
          && page.looks_like_article()
          && page.prose_ratio() >= cfg.crawl.min_prose_ratio
      );
      println!(
        "strong:     {} mentions ({:.1}/1000 tokens), title={}, lede={}: {}",
        rel.strong_mentions,
        rel.strong_per_1000(),
        rel.strong_in_title,
        rel.strong_in_lede,
        rel.strong.join(", ")
      );
      println!("--- text ---");
      for l in page.text.lines().take(lines) {
        let shown: String = l.chars().take(160).collect();
        println!("{shown}{}", if l.chars().count() > 160 { "…" } else { "" });
      }
      if show_links {
        println!("--- links in scope ---");
        for l in in_scope {
          println!("{l}");
        }
      }
    }
    Fetched::Xml { body, .. } => {
      let links = scraper_extract::xml::extract_links(&body);
      println!("xml feed/sitemap with {} entries", links.len());
      for l in links.iter().take(lines) {
        println!(
          "{} {} {}",
          if l.nested { "[sitemap]" } else { "[url]" },
          l.url,
          l.date.as_deref().unwrap_or("")
        );
      }
    }
    Fetched::Skipped(ct) => println!("skipped: content-type {ct}"),
    Fetched::Status(s) => println!("http status {s}"),
    Fetched::Error(e) => println!("error: {e}"),
  }
  Ok(())
}

fn stats(data_dir: &std::path::Path) -> Result<()> {
  use scraper_store::{SeenSet, Store, shards};
  let legacy = shards::legacy_next_id(data_dir);
  println!("data dir:    {}", data_dir.display());
  println!("legacy docs: {legacy} (data/log)");
  let seen_path = data_dir.join("state/seen.bin");
  if seen_path.exists() {
    let seen = SeenSet::open(&seen_path)?;
    println!("seen urls:   {} ({} hits)", seen.len(), seen.hits());
  } else {
    println!("seen urls:   none yet");
  }
  let mut total = 0usize;
  let mut years = std::collections::BTreeMap::<String, usize>::new();
  let mut domains = std::collections::HashMap::<String, usize>::new();
  for shard in shards::shards(data_dir) {
    let mut n = 0;
    for rec in shards::read_shard(&shard)? {
      let rec = rec?;
      n += 1;
      *years
        .entry(rec.year.map_or("none".into(), |y| y.to_string()))
        .or_default() += 1;
      *domains.entry(rec.domain).or_default() += 1;
    }
    total += n;
    let size = std::fs::metadata(&shard)?.len();
    println!(
      "shard {:<40} {:>6} records {:>8} KB",
      shard.file_name().unwrap().to_string_lossy(),
      n,
      size / 1024
    );
  }
  println!("new records: {total}");
  if !years.is_empty() {
    println!(
      "by year:     {}",
      years
        .iter()
        .map(|(y, n)| format!("{y}:{n}"))
        .collect::<Vec<_>>()
        .join(" ")
    );
    let mut d: Vec<_> = domains.into_iter().collect();
    d.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!(
      "top domains: {}",
      d.iter()
        .take(15)
        .map(|(h, n)| format!("{h}:{n}"))
        .collect::<Vec<_>>()
        .join(" ")
    );
  }
  if let Some(run) = Store::last_run(data_dir) {
    println!(
      "{}",
      serde_json::to_string_pretty(&serde_json::json!({
        "last_run": run.run_id, "started_at": run.started_at, "duration_secs": run.duration_secs,
        "dispatched": run.dispatched, "fetched_ok": run.fetched_ok, "hits": run.hits,
        "errors": run.errors, "throttled": run.throttled, "queued_left": run.queued_left,
      }))?
    );
  }
  Ok(())
}
