//! Persistent set of visited URLs, keyed by 64-bit hash.
//!
//! On disk this is an append-only file of fixed 16-byte records:
//! `u64 hash | u32 day-since-epoch | u32 flags` (little endian). Later
//! records for the same hash override earlier ones, so a run only ever
//! appends. 17k URLs cost 270 KB; a million cost 16 MB.

use anyhow::{Context, Result};
use std::{
  collections::HashMap,
  fs::{File, OpenOptions},
  io::{BufWriter, Read, Write},
  path::Path,
};

pub const FLAG_HIT: u32 = 1;

#[derive(Debug, Clone, Copy)]
pub struct Entry {
  pub day: u32,
  pub flags: u32,
}

impl Entry {
  pub fn is_hit(&self) -> bool {
    self.flags & FLAG_HIT != 0
  }
}

pub struct SeenSet {
  map: HashMap<u64, Entry>,
  writer: Option<BufWriter<File>>,
  pending: usize,
}

pub fn today() -> u32 {
  (chrono::Utc::now().timestamp() / 86_400) as u32
}

impl SeenSet {
  /// In-memory only (tests, `check-url`).
  pub fn ephemeral() -> Self {
    Self {
      map: HashMap::new(),
      writer: None,
      pending: 0,
    }
  }

  pub fn open(path: &Path) -> Result<Self> {
    let mut map = HashMap::new();
    if path.exists() {
      let mut buf = Vec::new();
      File::open(path)?
        .read_to_end(&mut buf)
        .with_context(|| format!("reading {}", path.display()))?;
      map.reserve(buf.len() / 16);
      for rec in buf.chunks_exact(16) {
        let hash = u64::from_le_bytes(rec[0..8].try_into().unwrap());
        let day = u32::from_le_bytes(rec[8..12].try_into().unwrap());
        let flags = u32::from_le_bytes(rec[12..16].try_into().unwrap());
        map.insert(hash, Entry { day, flags });
      }
    } else if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    // A torn write would misalign every later record; trim it first.
    {
      let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
      let len = f.metadata()?.len();
      if len % 16 != 0 {
        f.set_len(len / 16 * 16)?;
      }
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(Self {
      map,
      writer: Some(BufWriter::new(file)),
      pending: 0,
    })
  }

  pub fn len(&self) -> usize {
    self.map.len()
  }

  pub fn is_empty(&self) -> bool {
    self.map.is_empty()
  }

  pub fn hits(&self) -> usize {
    self.map.values().filter(|e| e.is_hit()).count()
  }

  pub fn get(&self, hash: u64) -> Option<Entry> {
    self.map.get(&hash).copied()
  }

  /// Hits are never refetched; non-hits only after `revisit_after_days`.
  pub fn should_skip(&self, hash: u64, revisit_after_days: u32) -> bool {
    match self.map.get(&hash) {
      None => false,
      Some(e) if e.is_hit() => true,
      Some(e) => today().saturating_sub(e.day) < revisit_after_days,
    }
  }

  pub fn record(&mut self, hash: u64, hit: bool) -> Result<()> {
    let entry = Entry {
      day: today(),
      flags: if hit { FLAG_HIT } else { 0 },
    };
    if let Some(prev) = self.map.get(&hash)
      && prev.is_hit()
      && !hit
    {
      return Ok(());
    }
    self.map.insert(hash, entry);
    if let Some(w) = self.writer.as_mut() {
      w.write_all(&hash.to_le_bytes())?;
      w.write_all(&entry.day.to_le_bytes())?;
      w.write_all(&entry.flags.to_le_bytes())?;
      self.pending += 1;
      if self.pending >= 256 {
        w.flush()?;
        self.pending = 0;
      }
    }
    Ok(())
  }

  pub fn flush(&mut self) -> Result<()> {
    if let Some(w) = self.writer.as_mut() {
      w.flush()?;
      self.pending = 0;
    }
    Ok(())
  }
}

impl Drop for SeenSet {
  fn drop(&mut self) {
    let _ = self.flush();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn round_trips_through_file() {
    let dir =
      std::env::temp_dir().join(format!("seen-test-{}", std::process::id()));
    let path = dir.join("seen.bin");
    {
      let mut s = SeenSet::open(&path).unwrap();
      s.record(1, true).unwrap();
      s.record(2, false).unwrap();
      s.record(2, true).unwrap();
      s.record(3, false).unwrap();
      s.record(1, false).unwrap();
    }
    let s = SeenSet::open(&path).unwrap();
    assert_eq!(s.len(), 3);
    assert_eq!(s.hits(), 2);
    assert!(s.should_skip(1, 7));
    assert!(s.should_skip(2, 7));
    assert!(s.should_skip(3, 7));
    assert!(!s.should_skip(3, 0));
    assert!(!s.should_skip(4, 7));
    std::fs::remove_dir_all(dir).unwrap();
  }
}
