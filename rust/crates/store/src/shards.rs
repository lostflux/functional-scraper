//! # Storage
//!
//! Where crawl output and state live under the data directory:
//!
//! * `articles/<run-id>.jsonl.zst` — one shard per run, one JSON record per
//!   line, zstd-compressed and append-only. Shards are immutable once a run
//!   ends, so they diff and sync cleanly.
//! * `state/seen.bin` — the hashed seen-set (see [`crate::seen`]).
//! * `state/next_id` — the next record id; continues the legacy numbering.
//! * `state/last_run.json` — summary of the most recent run.
//! * `state/frontier.jsonl` — leftover queue for `--resume` (not committed).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeMap,
  fs::{self, File},
  io::{BufRead, BufReader, BufWriter, Write},
  path::{Path, PathBuf},
};

/// Serialisable form for `state/frontier.jsonl`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SavedJob {
  pub url: String,
  pub depth: u32,
  pub seed: bool,
  pub hot: bool,
}

/// Per-host counters reported at the end of a run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostStats {
  pub fetched: usize,
  pub hits: usize,
  pub queued: usize,
  #[serde(default)]
  pub in_flight: usize,
  /// Exponential moving average of recent hits per fetch.
  #[serde(default)]
  pub recent_hit_ratio: f64,
  /// Share of turns the scheduler currently gives this host (0..1).
  #[serde(default = "one")]
  pub weight: f64,
  pub dropped: Option<String>,
}

fn one() -> f64 {
  1.0
}

/// Written to `state/progress.json` every few seconds while a crawl runs,
/// for live dashboards.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Progress {
  pub run_id: String,
  pub elapsed_secs: u64,
  pub dispatched: usize,
  pub fetched_ok: usize,
  pub hits: usize,
  pub errors: usize,
  pub throttled: usize,
  pub queued: usize,
  pub active: usize,
  pub finished: bool,
  pub hosts: BTreeMap<String, HostStats>,
}

/// One stored article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
  pub id: u64,
  pub url: String,
  pub domain: String,
  pub title: String,
  pub date: Option<String>,
  pub date_source: Option<String>,
  pub year: Option<i32>,
  pub text: String,
  pub keywords: Vec<String>,
  /// Strong (AI-specific) terms found, and how often, in title + text.
  #[serde(default)]
  pub strong_keywords: Vec<String>,
  #[serde(default)]
  pub strong_mentions: usize,
  pub fetched_at: String,
}

/// Written to `state/last_run.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunSummary {
  pub run_id: String,
  pub started_at: String,
  pub finished_at: String,
  pub duration_secs: u64,
  pub dispatched: usize,
  pub fetched_ok: usize,
  pub hits: usize,
  pub errors: usize,
  pub throttled: usize,
  pub disallowed: usize,
  pub queued_left: usize,
  pub shard: Option<String>,
  pub hosts: BTreeMap<String, HostStats>,
}

pub struct Store {
  data_dir: PathBuf,
  run_id: String,
  writer: Option<zstd::Encoder<'static, BufWriter<File>>>,
  next_id: u64,
  written: usize,
}

impl Store {
  pub fn open(data_dir: &Path) -> Result<Self> {
    fs::create_dir_all(data_dir.join("articles"))?;
    fs::create_dir_all(data_dir.join("state"))?;
    let next_id = match fs::read_to_string(data_dir.join("state/next_id")) {
      Ok(s) => s.trim().parse().context("state/next_id is not a number")?,
      Err(_) => legacy_next_id(data_dir),
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut run_id = stamp.clone();
    let mut n = 1;
    while data_dir
      .join(format!("articles/{run_id}.jsonl.zst"))
      .exists()
    {
      n += 1;
      run_id = format!("{stamp}-{n}");
    }
    Ok(Self {
      data_dir: data_dir.to_path_buf(),
      run_id,
      writer: None,
      next_id,
      written: 0,
    })
  }

  pub fn run_id(&self) -> &str {
    &self.run_id
  }

  pub fn next_id(&self) -> u64 {
    self.next_id
  }

  pub fn written(&self) -> usize {
    self.written
  }

  pub fn seen_path(&self) -> PathBuf {
    self.data_dir.join("state/seen.bin")
  }

  pub fn shard_path(&self) -> PathBuf {
    self
      .data_dir
      .join(format!("articles/{}.jsonl.zst", self.run_id))
  }

  /// Hands out the next id.
  pub fn alloc_id(&mut self) -> u64 {
    let id = self.next_id;
    self.next_id += 1;
    id
  }

  /// Appends one record to this run's shard (created on first write).
  pub fn write(&mut self, rec: &Record) -> Result<()> {
    if self.writer.is_none() {
      let file = File::create(self.shard_path())?;
      let enc = zstd::Encoder::new(BufWriter::new(file), 6)?;
      self.writer = Some(enc);
    }
    let w = self.writer.as_mut().expect("writer created above");
    serde_json::to_writer(&mut *w, rec)?;
    w.write_all(b"\n")?;
    self.written += 1;
    if self.written.is_multiple_of(50) {
      w.flush()?;
      self.persist_next_id()?;
    }
    Ok(())
  }

  /// Writes `state/next_id`. Called at every shard flush so a crash cannot
  /// hand out an id that is already on disk.
  fn persist_next_id(&self) -> Result<()> {
    fs::write(
      self.data_dir.join("state/next_id"),
      format!("{}\n", self.next_id),
    )?;
    Ok(())
  }

  /// Closes the shard and persists `next_id`. Returns the shard path if any
  /// record was written.
  pub fn finish(&mut self) -> Result<Option<PathBuf>> {
    self.persist_next_id()?;
    if let Some(enc) = self.writer.take() {
      let mut inner = enc.finish()?;
      inner.flush()?;
      return Ok(Some(self.shard_path()));
    }
    Ok(None)
  }

  pub fn save_frontier(&self, jobs: &[SavedJob]) -> Result<()> {
    let path = self.data_dir.join("state/frontier.jsonl");
    if jobs.is_empty() {
      let _ = fs::remove_file(&path);
      return Ok(());
    }
    let mut w = BufWriter::new(File::create(&path)?);
    for j in jobs {
      serde_json::to_writer(&mut w, j)?;
      w.write_all(b"\n")?;
    }
    w.flush()?;
    Ok(())
  }

  pub fn load_frontier(&self) -> Result<Vec<SavedJob>> {
    let path = self.data_dir.join("state/frontier.jsonl");
    if !path.exists() {
      return Ok(vec![]);
    }
    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
      let line = line?;
      if !line.trim().is_empty() {
        out.push(serde_json::from_str(&line)?);
      }
    }
    Ok(out)
  }

  /// Atomically replaces `state/progress.json`.
  pub fn write_progress(&self, progress: &Progress) -> Result<()> {
    let path = self.data_dir.join("state/progress.json");
    let tmp = self.data_dir.join("state/.progress.json.tmp");
    fs::write(&tmp, serde_json::to_vec(progress)?)?;
    fs::rename(tmp, path)?;
    Ok(())
  }

  pub fn write_summary(&self, summary: &RunSummary) -> Result<()> {
    let path = self.data_dir.join("state/last_run.json");
    fs::write(path, serde_json::to_string_pretty(summary)?)?;
    Ok(())
  }

  pub fn last_run(data_dir: &Path) -> Option<RunSummary> {
    let s = fs::read_to_string(data_dir.join("state/last_run.json")).ok()?;
    serde_json::from_str(&s).ok()
  }
}

/// `max(numeric file name in data/log) + 1`, or 0 if there is no legacy data.
pub fn legacy_next_id(data_dir: &Path) -> u64 {
  let Ok(entries) = fs::read_dir(data_dir.join("log")) else {
    return 0;
  };
  entries
    .flatten()
    .filter_map(|e| e.file_name().to_str()?.parse::<u64>().ok())
    .max()
    .map_or(0, |m| m + 1)
}

/// All shard files, oldest first.
pub fn shards(data_dir: &Path) -> Vec<PathBuf> {
  let mut v: Vec<PathBuf> = fs::read_dir(data_dir.join("articles"))
    .map(|rd| {
      rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.zst"))
        .collect()
    })
    .unwrap_or_default();
  v.sort();
  v
}

/// Streams the records of one shard. A shard whose zstd frame was never
/// finished (crash, or a run still in progress) yields the records that
/// were flushed and then stops instead of failing.
pub fn read_shard(path: &Path) -> Result<impl Iterator<Item = Result<Record>>> {
  let dec = zstd::Decoder::new(File::open(path)?)?;
  Ok(
    BufReader::new(dec)
      .lines()
      .map_while(|l| match l {
        Ok(l) => Some(Ok(l)),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => None,
        Err(e) => Some(Err(anyhow::Error::from(e))),
      })
      .filter_map(|l| match l {
        Ok(l) if l.trim().is_empty() => None,
        Ok(l) => Some(serde_json::from_str(&l).map_err(Into::into)),
        Err(e) => Some(Err(e)),
      }),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn writes_and_reads_a_shard() {
    let dir =
      std::env::temp_dir().join(format!("store-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("log")).unwrap();
    fs::write(dir.join("log/41"), "x").unwrap();
    fs::write(dir.join("log/41.txt"), "x").unwrap();
    let mut store = Store::open(&dir).unwrap();
    assert_eq!(store.next_id(), 42, "continues legacy numbering");
    let id = store.alloc_id();
    let rec = Record {
      id,
      url: "https://x.com/a".into(),
      domain: "x.com".into(),
      title: "t".into(),
      date: Some("2024-01-01".into()),
      date_source: Some("meta".into()),
      year: Some(2024),
      text: "hello\nworld".into(),
      keywords: vec!["ai".into()],
      strong_keywords: vec![],
      strong_mentions: 0,
      fetched_at: "now".into(),
    };
    store.write(&rec).unwrap();
    let shard = store.finish().unwrap().unwrap();
    assert_eq!(
      fs::read_to_string(dir.join("state/next_id"))
        .unwrap()
        .trim(),
      "43"
    );
    let recs: Vec<Record> =
      read_shard(&shard).unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].id, 42);
    assert_eq!(recs[0].text, "hello\nworld");
    assert_eq!(shards(&dir), vec![shard]);
    let store2 = Store::open(&dir).unwrap();
    assert_eq!(store2.next_id(), 43);
    fs::remove_dir_all(&dir).unwrap();
  }
}
