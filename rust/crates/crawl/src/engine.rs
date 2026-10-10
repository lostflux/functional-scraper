//! # Engine
//!
//! The crawl loop. The main task owns the [`Frontier`], the [`SeenSet`] and
//! the [`Store`]; it dispatches jobs to spawned workers (at most
//! `crawl.concurrency` at once) and folds their results back in:
//!
//! ```text
//! frontier ──job──▶ worker: robots? → fetch → extract → classify
//!     ▲                                                  │
//!     └── enqueue links ◀── store hit ◀── seen.record ◀──┘
//! ```
//!
//! Stop conditions: empty frontier, page/hit/time budget, or Ctrl-C (the
//! first interrupt drains in-flight work, a second abandons it). Whatever
//! is still queued is written to `state/frontier.jsonl` for `--resume`.

use crate::frontier::{Frontier, Job, Limits, Next, Outcome};
use anyhow::{Context, Result};
use futures_util::FutureExt;
use scraper_core::{Config, Scope, Scoper, urls};
use scraper_extract::{
  Classifier, FeedLink, Page, Relevance, dates, html, xml,
};
use scraper_net::{Fetched, Fetcher};
use scraper_store::{Progress, Record, RunSummary, SavedJob, SeenSet, Store};
use std::{
  panic::AssertUnwindSafe,
  path::Path,
  sync::Arc,
  time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use url::Url;

/// Command-line overrides for one run.
#[derive(Debug, Clone, Default)]
pub struct CrawlOptions {
  pub max_pages: Option<usize>,
  pub max_hits: Option<usize>,
  pub max_duration: Option<Duration>,
  /// Skip sitemap/feed entries older than this.
  pub min_lastmod: Option<chrono::NaiveDate>,
  /// Start from `state/frontier.jsonl` instead of the seeds.
  pub resume: bool,
  pub extra_seeds: Vec<String>,
  pub scope: Option<Scope>,
}

/// Shared, read-only state for workers.
struct Ctx {
  cfg: Config,
  fetcher: Fetcher,
  classifier: Classifier,
}

enum Kind {
  Page { page: Box<Page>, rel: Relevance },
  Feed { links: Vec<FeedLink> },
  Status(u16),
  Error(String),
  Disallowed,
  Skipped(String),
}

struct Done {
  job: Job,
  kind: Kind,
  /// Sitemaps advertised by robots.txt (seeds only).
  sitemaps: Vec<String>,
  delay_hint: Option<Duration>,
}

#[derive(Default)]
struct Stats {
  dispatched: usize,
  fetched_ok: usize,
  hits: usize,
  errors: usize,
  throttled: usize,
  disallowed: usize,
}

async fn work(ctx: Arc<Ctx>, job: Job) -> Done {
  let cfg = &ctx.cfg.crawl;
  let robots = ctx.fetcher.robots(&job.url).await;
  let delay_hint = robots.delay;
  let sitemaps = if job.seed && job.depth == 0 && cfg.follow_robots_sitemaps {
    robots.sitemaps.clone()
  } else {
    Vec::new()
  };
  if cfg.respect_robots && !robots.allowed(job.url.as_str()) {
    return Done {
      job,
      kind: Kind::Disallowed,
      sitemaps,
      delay_hint,
    };
  }
  let kind = match ctx.fetcher.fetch(&job.url).await {
    Fetched::Html { url, body } => {
      let min = cfg.min_text_chars;
      let ctx2 = ctx.clone();
      let parsed = tokio::task::spawn_blocking(move || {
        let page = html::parse_html(&url, &body, min);
        let rel = ctx2.classifier.relevance(&page.title, &page.text);
        (page, rel)
      })
      .await;
      match parsed {
        Ok((page, rel)) => Kind::Page {
          page: Box::new(page),
          rel,
        },
        Err(e) => Kind::Error(format!("parse failed: {e}")),
      }
    }
    Fetched::Xml { body, .. } => {
      match tokio::task::spawn_blocking(move || xml::extract_links(&body)).await
      {
        Ok(links) => Kind::Feed { links },
        Err(e) => Kind::Error(format!("feed parse failed: {e}")),
      }
    }
    Fetched::Skipped(ct) => Kind::Skipped(ct),
    Fetched::Status(s) => Kind::Status(s),
    Fetched::Error(e) => Kind::Error(e),
  };
  Done {
    job,
    kind,
    sitemaps,
    delay_hint,
  }
}

struct Engine {
  cfg: Config,
  scoper: Scoper,
  classifier: Classifier,
  store: Store,
  seen: SeenSet,
  frontier: Frontier,
  stats: Stats,
  min_lastmod: Option<chrono::NaiveDate>,
}

impl Engine {
  /// Normalises, scopes and dedupes `url`, then queues it. Returns whether
  /// it was queued.
  fn enqueue(&mut self, url: Url, depth: u32, seed: bool, hot: bool) -> bool {
    let Some(url) = urls::normalize_with(url, seed) else {
      return false;
    };
    if !seed && !self.scoper.allows(&url) {
      return false;
    }
    let canon = urls::canonical(&url);
    let hash = urls::hash(&canon);
    if !seed
      && self
        .seen
        .should_skip(hash, self.cfg.crawl.revisit_after_days)
    {
      return false;
    }
    let host = urls::bare_host(&url).unwrap_or_default();
    let trusted = self.scoper.in_domains(&url);
    let job = Job {
      url,
      canon,
      hash,
      depth,
      seed,
      hot,
      retries: 0,
    };
    self.frontier.push(job, &host, trusted)
  }

  /// Queues the configured seeds (hot, so they are fetched promptly).
  fn enqueue_seeds(&mut self, seeds: &[String]) -> usize {
    let mut n = 0;
    for s in seeds {
      match Url::parse(s.trim()) {
        Ok(u) => {
          if self.enqueue(u, 0, true, true) {
            n += 1;
          }
        }
        Err(e) => warn!("bad seed {s:?}: {e}"),
      }
    }
    n
  }

  fn progress(
    &self,
    elapsed: Duration,
    active: usize,
    finished: bool,
  ) -> Progress {
    Progress {
      run_id: self.store.run_id().to_string(),
      elapsed_secs: elapsed.as_secs(),
      dispatched: self.stats.dispatched,
      fetched_ok: self.stats.fetched_ok,
      hits: self.stats.hits,
      errors: self.stats.errors,
      throttled: self.stats.throttled,
      queued: self.frontier.queued(),
      active,
      finished,
      hosts: self.frontier.host_stats(),
    }
  }

  fn handle(&mut self, done: Done) -> Result<()> {
    let Done {
      job,
      kind,
      sitemaps,
      delay_hint,
    } = done;
    let host = urls::bare_host(&job.url).unwrap_or_default();
    for sm in sitemaps {
      if let Ok(u) = Url::parse(&sm)
        && self.enqueue(u, 1, true, true)
      {
        debug!("sitemap from robots.txt: {sm}");
      }
    }
    let outcome = match kind {
      Kind::Page { page, rel } => self.on_page(&job, *page, rel)?,
      Kind::Feed { links } => self.on_feed(&job, links)?,
      Kind::Status(s @ (429 | 503)) => {
        self.stats.throttled += 1;
        if job.retries < 2 {
          debug!("{s} from {host}, backing off and retrying {}", job.canon);
          self.frontier.retry(job.clone(), &host);
        } else {
          warn!("{s} from {host}, giving up on {}", job.canon);
        }
        Outcome::Throttled
      }
      Kind::Status(s) => {
        self.stats.errors += 1;
        if (400..500).contains(&s) && !matches!(s, 403 | 408 | 425 | 429) {
          self.seen.record(job.hash, false)?;
        }
        debug!("http {s}: {}", job.canon);
        Outcome::Error
      }
      Kind::Error(e) => {
        self.stats.errors += 1;
        debug!("error ({e}): {}", job.canon);
        Outcome::Error
      }
      Kind::Disallowed => {
        self.stats.disallowed += 1;
        if !job.seed {
          self.seen.record(job.hash, false)?;
        }
        debug!("robots.txt disallows {}", job.canon);
        Outcome::Disallowed
      }
      Kind::Skipped(ct) => {
        self.stats.fetched_ok += 1;
        self.seen.record(job.hash, false)?;
        debug!("skipped ({ct}): {}", job.canon);
        Outcome::Miss
      }
    };
    self.frontier.complete(&host, outcome, delay_hint);
    Ok(())
  }

  fn on_page(
    &mut self,
    job: &Job,
    page: Page,
    rel: Relevance,
  ) -> Result<Outcome> {
    let keywords = rel.keywords.clone();
    self.stats.fetched_ok += 1;
    let cfg = &self.cfg.crawl;
    let english = !cfg.english_only
      || page.lang.as_deref().is_none_or(|l| l.starts_with("en"));
    let article = !cfg.require_article_signal
      || (page.looks_like_article()
        && page.prose_ratio() >= cfg.min_prose_ratio);
    let relevant = self.classifier.is_hit(&rel)
      && page.text_len() >= cfg.min_text_chars
      && english
      && article;

    let canon = urls::canonical(&page.url);
    let chash = urls::hash(&canon);
    let dup = self.seen.get(chash).is_some_and(|e| e.is_hit());
    let hit = relevant && !dup;

    if hit {
      let id = self.store.alloc_id();
      let rec = Record {
        id,
        url: canon.clone(),
        domain: urls::bare_host(&page.url).unwrap_or_default(),
        title: page.title.clone(),
        date: page.date.clone(),
        date_source: page.date_source.map(String::from),
        year: page.year,
        text: page.text.clone(),
        keywords: keywords.clone(),
        strong_keywords: rel.strong.clone(),
        strong_mentions: rel.strong_mentions,
        fetched_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
      };
      // Record first, then mark seen: a crash between the two loses at
      // most a refetch, never an article.
      self.store.write(&rec)?;
      self.stats.hits += 1;
      info!(
        "hit #{id} [{}] {} | {}",
        page
          .date
          .as_deref()
          .unwrap_or(&page.year.map_or("----".into(), |y| y.to_string())),
        canon,
        page.title
      );
    } else {
      debug!(
        "miss ({} kw, {} strong/{} mentions, {} chars, lang={}, article={}, prose={:.2}, dup={}): {}",
        keywords.len(),
        rel.strong.len(),
        rel.strong_mentions,
        page.text_len(),
        page.lang.as_deref().unwrap_or("?"),
        page.looks_like_article(),
        page.prose_ratio(),
        dup,
        canon
      );
    }

    self.seen.record(job.hash, hit)?;
    if chash != job.hash {
      self.seen.record(chash, hit)?;
    }

    let scope = self.scoper.scope();
    let trusted = self.scoper.in_domains(&job.url);
    let old = !cfg.follow_links_from_old_pages
      && !job.seed
      && self.min_lastmod.is_some_and(|m| {
        use chrono::Datelike;
        page.year.is_some_and(|y| y < m.year())
      });
    let follow = !old
      && (scope == Scope::Domains
        || trusted
        || !cfg.follow_only_from_hits
        || hit
        || job.seed);
    if follow {
      let child_depth = if scope == Scope::Open && hit {
        job.depth
      } else {
        job.depth + 1
      };
      if child_depth <= cfg.max_depth {
        let mut added = 0;
        let total = page.links.len();
        for link in page.links {
          if self.enqueue(link, child_depth, false, hit) {
            added += 1;
          }
        }
        debug!("queued {added}/{total} links from {canon}");
      }
    }
    Ok(if hit { Outcome::Hit } else { Outcome::Miss })
  }

  fn on_feed(&mut self, job: &Job, links: Vec<FeedLink>) -> Result<Outcome> {
    self.stats.fetched_ok += 1;
    if !job.seed {
      self.seen.record(job.hash, false)?;
    }
    let total = links.len();
    let min = self.min_lastmod;
    let is_stale = |l: &FeedLink| {
      let Some(min) = min else { return false };
      match l.date.as_deref().and_then(dates::parse_date) {
        Some(d) => d < min,
        None => l.nested && dates::name_is_before(&l.url, min),
      }
    };
    let (mut nested, pages): (Vec<FeedLink>, Vec<FeedLink>) = links
      .into_iter()
      .filter(|l| !is_stale(l))
      .partition(|l| l.nested);
    let stale = total - nested.len() - pages.len();
    nested.sort_by(|a, b| {
      let da = a.date.as_deref().and_then(dates::parse_date);
      let db = b.date.as_deref().and_then(dates::parse_date);
      db.cmp(&da)
    });
    let cap = self.cfg.crawl.max_sitemaps_per_index;
    let over_cap = nested.len().saturating_sub(cap);
    nested.truncate(cap);

    let mut added = 0;
    for l in nested.into_iter().chain(pages) {
      let Ok(u) = Url::parse(&l.url) else { continue };
      if self.enqueue(u, job.depth + 1, l.nested, true) {
        added += 1;
      }
    }
    info!(
      "feed {}: queued {added}/{total} ({stale} before cutoff, {over_cap} child sitemaps over cap)",
      job.canon
    );
    Ok(Outcome::Neutral)
  }

  fn summary(
    &self,
    run_id: &str,
    started: chrono::DateTime<chrono::Utc>,
    elapsed: Duration,
    shard: Option<String>,
  ) -> RunSummary {
    RunSummary {
      run_id: run_id.to_string(),
      started_at: started.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
      finished_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
      duration_secs: elapsed.as_secs(),
      dispatched: self.stats.dispatched,
      fetched_ok: self.stats.fetched_ok,
      hits: self.stats.hits,
      errors: self.stats.errors,
      throttled: self.stats.throttled,
      disallowed: self.stats.disallowed,
      queued_left: self.frontier.queued(),
      shard,
      hosts: self.frontier.host_stats(),
    }
  }
}

/// Runs one crawl and returns its summary (also written to
/// `state/last_run.json`).
pub async fn run(
  mut cfg: Config,
  data_dir: &Path,
  opts: CrawlOptions,
) -> Result<RunSummary> {
  if let Some(s) = opts.scope {
    cfg.crawl.scope = s;
  }
  if let Some(n) = opts.max_pages {
    cfg.crawl.max_pages = Some(n);
  }
  if let Some(n) = opts.max_hits {
    cfg.crawl.max_hits = Some(n);
  }
  if let Some(d) = opts.max_duration {
    cfg.crawl.max_duration_secs = Some(d.as_secs());
  }
  let min_lastmod = opts
    .min_lastmod
    .or_else(|| cfg.crawl.min_lastmod.as_deref().and_then(dates::parse_date));

  let store = Store::open(data_dir).context("opening data dir")?;
  let seen = SeenSet::open(&store.seen_path()).context("opening seen-set")?;
  info!(
    "run {} | scope={:?} concurrency={} seen={} (hits {}) next_id={} min_lastmod={}",
    store.run_id(),
    cfg.crawl.scope,
    cfg.crawl.concurrency,
    seen.len(),
    seen.hits(),
    store.next_id(),
    min_lastmod.map_or("none".into(), |d| d.to_string())
  );

  let limits = Limits {
    per_host_concurrency: cfg.crawl.per_host_concurrency.max(1),
    per_host_delay: Duration::from_millis(cfg.crawl.per_host_delay_ms),
    max_queue_per_host: cfg.crawl.max_queue_per_host,
    max_pages_per_host: cfg.crawl.max_pages_per_host,
    probation_pages: cfg.crawl.host_probation_pages,
    min_hit_ratio: cfg.crawl.host_min_hit_ratio,
    max_errors_in_row: 25,
    yield_weighting: cfg.crawl.yield_weighting,
    yield_warmup: cfg.crawl.yield_warmup_pages,
    min_host_weight: cfg.crawl.min_host_weight.clamp(0.01, 1.0),
  };
  let classifier = scraper_extract::classifier_for(&cfg);
  let ctx = Arc::new(Ctx {
    fetcher: Fetcher::new(&cfg.crawl)?,
    classifier: scraper_extract::classifier_for(&cfg),
    cfg: cfg.clone(),
  });
  let mut engine = Engine {
    scoper: Scoper::new(&cfg),
    classifier,
    store,
    seen,
    frontier: Frontier::new(limits),
    stats: Stats::default(),
    min_lastmod,
    cfg: cfg.clone(),
  };

  if opts.resume {
    let saved = engine.store.load_frontier()?;
    let mut n = 0;
    for s in &saved {
      if let Ok(u) = Url::parse(&s.url)
        && engine.enqueue(u, s.depth, s.seed, s.hot)
      {
        n += 1;
      }
    }
    info!("resumed {n} of {} queued URLs", saved.len());
  }
  let seeds: Vec<String> = cfg
    .domains
    .iter()
    .chain(&cfg.seeds)
    .chain(&opts.extra_seeds)
    .cloned()
    .collect();
  let n = engine.enqueue_seeds(&seeds);
  info!("queued {n} of {} seeds", seeds.len());
  let reseed_every = cfg.crawl.reseed_every_secs.map(Duration::from_secs);
  let mut last_seeded = Instant::now();

  let concurrency = cfg.crawl.concurrency.max(1);
  let max_pages = cfg.max_pages();
  let max_hits = cfg.crawl.max_hits.unwrap_or(usize::MAX);
  let max_duration = cfg.crawl.max_duration_secs.map(Duration::from_secs);
  let (tx, mut rx) = mpsc::channel::<Done>(concurrency * 2);
  let started_at = chrono::Utc::now();
  let started = Instant::now();
  let mut active = 0usize;
  let mut stopping = false;
  let mut last_progress = Instant::now();
  let ctrl_c = tokio::signal::ctrl_c();
  tokio::pin!(ctrl_c);

  // An I/O error while handling a result (disk full, …) stops dispatching
  // but still runs the cleanup below, so the shard and state stay sane.
  let mut fatal: Option<anyhow::Error> = None;
  loop {
    if !stopping
      && let Some(every) = reseed_every
      && last_seeded.elapsed() >= every
    {
      last_seeded = Instant::now();
      let n = engine.enqueue_seeds(&seeds);
      info!("re-seeded {n} of {} seeds", seeds.len());
    }
    while let Ok(d) = rx.try_recv() {
      active -= 1;
      if let Err(e) = engine.handle(d) {
        error!("stopping: {e:#}");
        fatal.get_or_insert(e);
        stopping = true;
      }
    }
    let out_of_budget = engine.stats.dispatched >= max_pages
      || engine.stats.hits >= max_hits
      || max_duration.is_some_and(|d| started.elapsed() >= d);
    if out_of_budget && !stopping {
      info!("budget reached, finishing in-flight fetches");
      stopping = true;
    }
    let mut wake = Instant::now() + Duration::from_millis(250);
    if !stopping {
      while active < concurrency && engine.stats.dispatched < max_pages {
        match engine.frontier.next(Instant::now()) {
          Next::Job(job) => {
            active += 1;
            engine.stats.dispatched += 1;
            let tx = tx.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move {
              let fallback = job.clone();
              let done =
                match AssertUnwindSafe(work(ctx, job)).catch_unwind().await {
                  Ok(done) => done,
                  Err(_) => Done {
                    job: fallback,
                    kind: Kind::Error("worker panicked".into()),
                    sitemaps: Vec::new(),
                    delay_hint: None,
                  },
                };
              let _ = tx.send(done).await;
            });
          }
          Next::Wait(t) => {
            wake = wake.min(t);
            break;
          }
          Next::Empty => break,
        }
      }
    }
    if active == 0 && (stopping || engine.frontier.queued() == 0) {
      break;
    }
    if last_progress.elapsed() >= Duration::from_secs(10) {
      last_progress = Instant::now();
      let s = &engine.stats;
      info!(
        "progress: fetched={} hits={} errors={} throttled={} queued={} active={} hosts={} {:.1} pages/s",
        s.fetched_ok,
        s.hits,
        s.errors,
        s.throttled,
        engine.frontier.queued(),
        active,
        engine.frontier.active_hosts(),
        s.dispatched as f64 / started.elapsed().as_secs_f64().max(0.001)
      );
      if let Err(e) = engine.store.write_progress(&engine.progress(
        started.elapsed(),
        active,
        false,
      )) {
        warn!("could not write progress.json: {e}");
      }
    }
    tokio::select! {
      Some(d) = rx.recv() => {
        active -= 1;
        if let Err(e) = engine.handle(d) {
          error!("stopping: {e:#}");
          fatal.get_or_insert(e);
          stopping = true;
        }
      }
      _ = tokio::time::sleep_until(wake.into()) => {}
      _ = &mut ctrl_c => {
        if stopping {
          warn!("second interrupt: abandoning {active} in-flight fetches");
          break;
        }
        info!("interrupted: finishing {active} in-flight fetches (Ctrl-C again to abandon)");
        stopping = true;
        ctrl_c.set(tokio::signal::ctrl_c());
      }
    }
  }

  let leftover: Vec<SavedJob> =
    engine.frontier.drain().iter().map(SavedJob::from).collect();
  engine.store.save_frontier(&leftover)?;
  let shard = engine.store.finish()?.map(|p| p.display().to_string());
  engine.seen.flush()?;
  let elapsed = started.elapsed();
  let mut summary =
    engine.summary(engine.store.run_id(), started_at, elapsed, shard.clone());
  summary.queued_left = leftover.len();
  engine.store.write_summary(&summary)?;
  let _ = engine
    .store
    .write_progress(&engine.progress(elapsed, 0, true));
  if let Some(e) = fatal {
    return Err(e);
  }

  info!(
    "done in {}s: dispatched={} fetched={} hits={} errors={} throttled={} disallowed={} leftover={} seen={} shard={}",
    elapsed.as_secs(),
    summary.dispatched,
    summary.fetched_ok,
    summary.hits,
    summary.errors,
    summary.throttled,
    summary.disallowed,
    summary.queued_left,
    engine.seen.len(),
    shard.as_deref().unwrap_or("none")
  );
  let mut hosts: Vec<_> = summary.hosts.iter().collect();
  hosts.sort_by_key(|(_, h)| std::cmp::Reverse(h.fetched));
  for (host, h) in hosts.iter().take(25) {
    info!(
      "  {:<32} fetched={:<6} hits={:<6} queued={:<6} {}",
      host,
      h.fetched,
      h.hits,
      h.queued,
      h.dropped.as_deref().unwrap_or("")
    );
  }
  Ok(summary)
}
