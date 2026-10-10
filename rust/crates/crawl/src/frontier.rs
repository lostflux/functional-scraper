//! # Frontier
//!
//! The URL queue and politeness scheduler. URLs are grouped per host; hosts
//! are served round-robin (so the crawl is breadth-first across sites
//! rather than a deep dive into one), and each host has its own
//! concurrency limit, minimum request gap and page budget. Within a host,
//! links found on relevant pages (`hot`) are served before the rest.
//!
//! Two feedback rules keep the crawl on topic:
//!
//! * **Yield weighting.** Every host keeps an exponential moving average
//!   of its recent hit ratio. Once a host has had a warm-up number of
//!   fetches, its turn is taken with a probability proportional to its
//!   yield relative to the best host (never below `min_host_weight`), so
//!   budget flows to the sites that are producing articles. The rule is
//!   work-conserving: if nothing better is ready, a low-yield host is
//!   served anyway.
//! * **Probation** (open scope). A host outside the configured sources that
//!   stays below `min_hit_ratio` after `probation_pages` fetches is dropped.

use scraper_store::shards::{HostStats, SavedJob};
use std::{
  collections::{BTreeMap, HashMap, HashSet, VecDeque},
  time::{Duration, Instant},
};
use url::Url;

/// One URL waiting to be fetched.
#[derive(Debug, Clone)]
pub struct Job {
  pub url: Url,
  /// Canonical string form (see [`scraper_core::urls::canonical`]).
  pub canon: String,
  pub hash: u64,
  pub depth: u32,
  /// Seeds (and sitemaps found through them) bypass the seen-set and may
  /// be queued again (re-seeding).
  pub seed: bool,
  /// Found on a relevant page or in a feed: served first.
  pub hot: bool,
  pub retries: u8,
}

impl From<&Job> for SavedJob {
  fn from(j: &Job) -> Self {
    Self {
      url: j.canon.clone(),
      depth: j.depth,
      seed: j.seed,
      hot: j.hot,
    }
  }
}

/// How a dispatched job ended, for host bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
  Hit,
  Miss,
  Error,
  /// 429 / 503: back off this host.
  Throttled,
  /// Blocked by robots.txt; does not count against the host.
  Disallowed,
  /// A feed or sitemap: neither a hit nor a miss for yield purposes.
  Neutral,
}

pub enum Next {
  Job(Job),
  /// Nothing is ready before this instant.
  Wait(Instant),
  /// Nothing queued (or everything queued is at its host's limit).
  Empty,
}

#[derive(Debug, Clone)]
pub struct Limits {
  pub per_host_concurrency: usize,
  pub per_host_delay: Duration,
  pub max_queue_per_host: usize,
  pub max_pages_per_host: usize,
  pub probation_pages: usize,
  pub min_hit_ratio: f64,
  pub max_errors_in_row: u32,
  pub yield_weighting: bool,
  /// Fetches before yield weighting applies to a host.
  pub yield_warmup: usize,
  /// Lowest share of turns a host can be reduced to.
  pub min_host_weight: f64,
}

/// Smoothing factor of the per-host yield average (~10-fetch window).
const YIELD_ALPHA: f64 = 0.1;

struct HostState {
  hot: VecDeque<Job>,
  cold: VecDeque<Job>,
  in_flight: usize,
  next_allowed: Instant,
  delay: Duration,
  fetched: usize,
  hits: usize,
  errors_in_row: u32,
  dropped: Option<&'static str>,
  /// Configured sources are exempt from probation.
  trusted: bool,
  in_order: bool,
  /// Recent hit ratio (EMA).
  yield_ema: f64,
  /// Accumulated share of a turn; served when it reaches 1.
  credit: f64,
}

impl HostState {
  fn queued(&self) -> usize {
    self.hot.len() + self.cold.len()
  }

  fn clear(&mut self, reason: &'static str) -> usize {
    let n = self.queued();
    self.hot.clear();
    self.cold.clear();
    self.dropped = Some(reason);
    n
  }
}

pub struct Frontier {
  hosts: HashMap<String, HostState>,
  order: VecDeque<String>,
  enqueued: HashSet<u64>,
  queued: usize,
  in_flight: usize,
  limits: Limits,
  /// Best yield EMA among warmed-up hosts; weights are relative to it.
  best_yield: f64,
}

impl Frontier {
  pub fn new(limits: Limits) -> Self {
    Self {
      hosts: HashMap::new(),
      order: VecDeque::new(),
      enqueued: HashSet::new(),
      queued: 0,
      in_flight: 0,
      limits,
      best_yield: 0.0,
    }
  }

  pub fn queued(&self) -> usize {
    self.queued
  }

  pub fn in_flight(&self) -> usize {
    self.in_flight
  }

  pub fn is_idle(&self) -> bool {
    self.queued == 0 && self.in_flight == 0
  }

  /// Hosts with something queued or in flight.
  pub fn active_hosts(&self) -> usize {
    self
      .hosts
      .values()
      .filter(|h| h.queued() > 0 || h.in_flight > 0)
      .count()
  }

  fn host_mut(&mut self, host: &str, trusted: bool) -> &mut HostState {
    let limits = &self.limits;
    self
      .hosts
      .entry(host.to_string())
      .or_insert_with(|| HostState {
        hot: VecDeque::new(),
        cold: VecDeque::new(),
        in_flight: 0,
        next_allowed: Instant::now(),
        delay: limits.per_host_delay,
        fetched: 0,
        hits: 0,
        errors_in_row: 0,
        dropped: None,
        trusted,
        in_order: false,
        yield_ema: 0.5,
        credit: 0.0,
      })
  }

  /// Weight of a host's turn: its yield relative to the best host, floored.
  fn weight(&self, st: &HostState) -> f64 {
    if !self.limits.yield_weighting
      || st.fetched < self.limits.yield_warmup
      || self.best_yield <= 0.0
    {
      return 1.0;
    }
    (st.yield_ema / self.best_yield).clamp(self.limits.min_host_weight, 1.0)
  }

  /// Queues `job` unless its hash was already queued this run (seeds are
  /// exempt), its host was dropped, or the host queue is full. Returns
  /// whether it was added.
  pub fn push(&mut self, job: Job, host: &str, trusted: bool) -> bool {
    if !job.seed && self.enqueued.contains(&job.hash) {
      return false;
    }
    let max_queue = self.limits.max_queue_per_host;
    let hash = job.hash;
    let st = self.host_mut(host, trusted);
    st.trusted |= trusted;
    if st.dropped.is_some() || st.queued() >= max_queue {
      return false;
    }
    if job.hot {
      st.hot.push_back(job);
    } else {
      st.cold.push_back(job);
    }
    let newly_ordered = !st.in_order;
    st.in_order = true;
    self.queued += 1;
    self.enqueued.insert(hash);
    if newly_ordered {
      self.order.push_back(host.to_string());
    }
    true
  }

  /// Re-queues a job that was throttled, at the front of its host queue.
  pub fn retry(&mut self, mut job: Job, host: &str) {
    job.retries += 1;
    let st = self.host_mut(host, false);
    if st.dropped.is_some() {
      return;
    }
    st.hot.push_front(job);
    let newly_ordered = !st.in_order;
    st.in_order = true;
    self.queued += 1;
    if newly_ordered {
      self.order.push_back(host.to_string());
    }
  }

  fn take(&mut self, host: &str, now: Instant) -> Job {
    let st = self.hosts.get_mut(host).expect("host exists");
    let job = st
      .hot
      .pop_front()
      .or_else(|| st.cold.pop_front())
      .expect("host has a queued job");
    st.in_flight += 1;
    st.next_allowed = now + st.delay;
    self.queued -= 1;
    self.in_flight += 1;
    job
  }

  /// The next job whose host is ready, rotating across hosts and favouring
  /// the ones that have been yielding articles.
  pub fn next(&mut self, now: Instant) -> Next {
    let mut wait: Option<Instant> = None;
    let mut deferred: Option<String> = None;
    let mut chosen: Option<String> = None;
    for _ in 0..self.order.len() {
      let Some(host) = self.order.pop_front() else {
        break;
      };
      let limits = self.limits.clone();
      let best = self.best_yield;
      let st = self.hosts.get_mut(&host).expect("host in order has state");
      if st.queued() == 0 || st.dropped.is_some() {
        st.in_order = false;
        continue;
      }
      self.order.push_back(host.clone());
      if st.fetched + st.in_flight >= limits.max_pages_per_host {
        self.queued -= st.clear("host page budget");
        continue;
      }
      if st.in_flight >= limits.per_host_concurrency {
        continue;
      }
      if st.next_allowed > now {
        wait = Some(wait.map_or(st.next_allowed, |w| w.min(st.next_allowed)));
        continue;
      }
      let weight = if limits.yield_weighting
        && st.fetched >= limits.yield_warmup
        && best > 0.0
      {
        (st.yield_ema / best).clamp(limits.min_host_weight, 1.0)
      } else {
        1.0
      };
      if weight < 1.0 {
        st.credit += weight;
        if st.credit < 1.0 {
          deferred.get_or_insert(host);
          continue;
        }
        st.credit -= 1.0;
      }
      chosen = Some(host);
      break;
    }
    if let Some(host) = chosen.or(deferred) {
      return Next::Job(self.take(&host, now));
    }
    match wait {
      Some(w) => Next::Wait(w),
      None => Next::Empty,
    }
  }

  /// Records the outcome of a dispatched job.
  pub fn complete(
    &mut self,
    host: &str,
    outcome: Outcome,
    delay_hint: Option<Duration>,
  ) {
    let limits = self.limits.clone();
    let Some(st) = self.hosts.get_mut(host) else {
      return;
    };
    st.in_flight = st.in_flight.saturating_sub(1);
    self.in_flight = self.in_flight.saturating_sub(1);
    if let Some(d) = delay_hint {
      st.delay = st.delay.max(d);
    }
    let sample = match outcome {
      Outcome::Hit => {
        st.fetched += 1;
        st.hits += 1;
        st.errors_in_row = 0;
        Some(1.0)
      }
      Outcome::Miss => {
        st.fetched += 1;
        st.errors_in_row = 0;
        Some(0.0)
      }
      Outcome::Error => {
        st.fetched += 1;
        st.errors_in_row += 1;
        Some(0.0)
      }
      Outcome::Throttled => {
        st.errors_in_row += 1;
        let backoff =
          Duration::from_secs(15 * u64::from(st.errors_in_row.min(8)));
        st.next_allowed = Instant::now() + backoff;
        None
      }
      Outcome::Disallowed => None,
      Outcome::Neutral => {
        st.errors_in_row = 0;
        None
      }
    };
    if let Some(x) = sample {
      st.yield_ema += YIELD_ALPHA * (x - st.yield_ema);
    }
    let reason = if st.errors_in_row >= limits.max_errors_in_row {
      Some("too many consecutive errors")
    } else if !st.trusted
      && st.fetched >= limits.probation_pages
      && (st.hits as f64) / (st.fetched as f64) < limits.min_hit_ratio
    {
      Some("off topic")
    } else {
      None
    };
    if let Some(reason) = reason {
      self.queued -= st.clear(reason);
    }
    if sample.is_some() {
      self.best_yield = self
        .hosts
        .values()
        .filter(|h| h.fetched >= limits.yield_warmup && h.dropped.is_none())
        .map(|h| h.yield_ema)
        .fold(0.0, f64::max);
    }
  }

  /// Removes and returns everything still queued (for persisting).
  pub fn drain(&mut self) -> Vec<Job> {
    let mut out = Vec::with_capacity(self.queued);
    for st in self.hosts.values_mut() {
      out.extend(st.hot.drain(..));
      out.extend(st.cold.drain(..));
    }
    self.queued = 0;
    out
  }

  pub fn host_stats(&self) -> BTreeMap<String, HostStats> {
    self
      .hosts
      .iter()
      .filter(|(_, h)| {
        h.fetched > 0
          || h.queued() > 0
          || h.in_flight > 0
          || h.dropped.is_some()
      })
      .map(|(name, h)| {
        (
          name.clone(),
          HostStats {
            fetched: h.fetched,
            hits: h.hits,
            queued: h.queued(),
            in_flight: h.in_flight,
            recent_hit_ratio: (h.yield_ema * 1000.0).round() / 1000.0,
            weight: (self.weight(h) * 100.0).round() / 100.0,
            dropped: h.dropped.map(String::from),
          },
        )
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn limits() -> Limits {
    Limits {
      per_host_concurrency: 1,
      per_host_delay: Duration::from_millis(100),
      max_queue_per_host: 10,
      max_pages_per_host: 100,
      probation_pages: 2,
      min_hit_ratio: 0.5,
      max_errors_in_row: 3,
      yield_weighting: false,
      yield_warmup: 20,
      min_host_weight: 0.1,
    }
  }

  fn job(u: &str, hot: bool) -> Job {
    let url = Url::parse(u).unwrap();
    let canon = scraper_core::urls::canonical(&url);
    let hash = scraper_core::urls::hash(&canon);
    Job {
      url,
      canon,
      hash,
      depth: 0,
      seed: false,
      hot,
      retries: 0,
    }
  }

  #[test]
  fn round_robins_hosts_and_serves_hot_first() {
    let mut f = Frontier::new(limits());
    assert!(f.push(job("https://a.com/1", false), "a.com", true));
    assert!(f.push(job("https://a.com/2", true), "a.com", true));
    assert!(f.push(job("https://b.com/1", false), "b.com", true));
    assert!(
      !f.push(job("https://a.com/1", false), "a.com", true),
      "dedupes"
    );
    let now = Instant::now();
    let Next::Job(j1) = f.next(now) else { panic!() };
    assert_eq!(j1.canon, "https://a.com/2", "hot before cold");
    let Next::Job(j2) = f.next(now) else { panic!() };
    assert_eq!(j2.canon, "https://b.com/1", "other host next");
    assert!(
      matches!(f.next(now), Next::Empty),
      "a.com at concurrency limit"
    );
    f.complete("a.com", Outcome::Hit, None);
    assert!(
      matches!(f.next(now), Next::Wait(_)),
      "a.com inside delay window"
    );
    let later = now + Duration::from_millis(200);
    let Next::Job(j3) = f.next(later) else {
      panic!()
    };
    assert_eq!(j3.canon, "https://a.com/1");
  }

  #[test]
  fn drops_off_topic_hosts_but_not_trusted_ones() {
    let mut f = Frontier::new(limits());
    for i in 0..4 {
      f.push(job(&format!("https://x.com/{i}"), false), "x.com", false);
      f.push(job(&format!("https://t.com/{i}"), false), "t.com", true);
    }
    f.complete("x.com", Outcome::Miss, None);
    f.complete("x.com", Outcome::Miss, None);
    f.complete("t.com", Outcome::Miss, None);
    f.complete("t.com", Outcome::Miss, None);
    let stats = f.host_stats();
    assert_eq!(stats["x.com"].dropped.as_deref(), Some("off topic"));
    assert_eq!(stats["x.com"].queued, 0);
    assert_eq!(stats["t.com"].dropped, None);
    assert_eq!(stats["t.com"].queued, 4);
    assert_eq!(f.queued(), 4);
  }

  #[test]
  fn seeds_can_be_queued_again() {
    let mut f = Frontier::new(limits());
    let mut s = job("https://a.com/feed", true);
    s.seed = true;
    assert!(f.push(s.clone(), "a.com", true));
    assert!(f.push(s, "a.com", true), "re-seeding bypasses the dedupe");
    assert_eq!(f.queued(), 2);
  }

  #[test]
  fn low_yield_hosts_get_fewer_turns_but_never_starve() {
    let mut l = limits();
    l.yield_weighting = true;
    l.yield_warmup = 2;
    l.per_host_concurrency = 10;
    l.per_host_delay = Duration::ZERO;
    l.min_host_weight = 0.25;
    l.max_queue_per_host = 100;
    let mut f = Frontier::new(l);
    for i in 0..40 {
      f.push(
        job(&format!("https://good.com/{i}"), false),
        "good.com",
        true,
      );
      f.push(job(&format!("https://bad.com/{i}"), false), "bad.com", true);
    }
    for _ in 0..10 {
      f.complete("good.com", Outcome::Hit, None);
      f.complete("bad.com", Outcome::Miss, None);
    }
    let mut served = std::collections::HashMap::new();
    for _ in 0..40 {
      let Next::Job(j) = f.next(Instant::now()) else {
        panic!()
      };
      let host = j.url.host_str().unwrap().to_string();
      *served.entry(host.clone()).or_insert(0) += 1;
      let outcome = if host == "good.com" {
        Outcome::Hit
      } else {
        Outcome::Miss
      };
      f.complete(&host, outcome, None);
    }
    let good = served["good.com"];
    let bad = served["bad.com"];
    assert!(
      good >= 25,
      "good host should dominate: good={good} bad={bad}"
    );
    assert!(bad >= 6, "bad host still gets its floor share: bad={bad}");
    let mut f2 = Frontier::new(limits());
    f2.push(job("https://only.com/1", false), "only.com", true);
    assert!(matches!(f2.next(Instant::now()), Next::Job(_)));
  }
}
