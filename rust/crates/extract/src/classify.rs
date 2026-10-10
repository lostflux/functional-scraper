//! # Classify
//!
//! Keyword relevance. Two tiers of targets:
//!
//! * `targets` — the broad vocabulary (the legacy "wordcount" rule): a page
//!   needs at least `min` distinct ones.
//! * `strong_targets` — terms that only an AI story uses (`ChatGPT`,
//!   `machine learning`, …). The story must be *about* them: a strong term
//!   in the title or the lede (first `lede_chars` of text), or a dense body
//!   (many mentions per thousand words), plus a minimum number of mentions.
//!   This keeps general tech articles whose site chrome mentions "AI" out.
//!
//! Targets may be single words or phrases; matching is on lowercased
//! alphanumeric tokens, so `GPT-4` yields `gpt` and `4`, and `A.I.` is
//! folded to `ai`.

use std::collections::{BTreeSet, HashSet};

/// Thresholds for the strong-term rule.
#[derive(Debug, Clone)]
pub struct StrongRule {
  /// Mentions of strong terms needed (title and lede included).
  pub min_mentions: usize,
  /// Require a strong term in the title or lede (or a dense body).
  pub require_lede: bool,
  /// How much of the text counts as the lede, in characters.
  pub lede_chars: usize,
  /// Dense-body fallback: at least this many mentions …
  pub dense_mentions: usize,
  /// … and this many per 1000 tokens …
  pub dense_per_1000: f64,
  /// … across at least this many distinct strong terms.
  pub dense_distinct: usize,
}

impl Default for StrongRule {
  fn default() -> Self {
    Self {
      min_mentions: 3,
      require_lede: true,
      lede_chars: 600,
      dense_mentions: 12,
      dense_per_1000: 5.0,
      dense_distinct: 2,
    }
  }
}

/// What the classifier found on one page.
#[derive(Debug, Clone, Default)]
pub struct Relevance {
  /// Distinct broad targets, sorted.
  pub keywords: Vec<String>,
  /// Distinct strong targets, sorted.
  pub strong: Vec<String>,
  pub strong_mentions: usize,
  pub tokens: usize,
  pub strong_in_title: bool,
  pub strong_in_lede: bool,
}

impl Relevance {
  pub fn strong_per_1000(&self) -> f64 {
    1000.0 * self.strong_mentions as f64 / self.tokens.max(1) as f64
  }
}

struct Vocab {
  words: HashSet<String>,
  phrases: Vec<String>,
}

impl Vocab {
  fn new(targets: &[String]) -> Self {
    let mut words = HashSet::new();
    let mut phrases = Vec::new();
    for t in targets {
      let norm: Vec<String> = tokens(t).collect();
      match norm.len() {
        0 => {}
        1 => {
          words.insert(norm[0].clone());
        }
        _ => phrases.push(norm.join(" ")),
      }
    }
    Self { words, phrases }
  }

  fn is_empty(&self) -> bool {
    self.words.is_empty() && self.phrases.is_empty()
  }

  /// (distinct matches, total mentions, token count).
  fn scan(&self, text: &str) -> (BTreeSet<String>, usize, usize) {
    let mut found = BTreeSet::new();
    let mut mentions = 0;
    let mut n = 0;
    let mut stream = String::with_capacity(if self.phrases.is_empty() {
      0
    } else {
      text.len()
    });
    stream.push(' ');
    for tok in tokens(text) {
      n += 1;
      if let Some(w) = self.words.get(&tok) {
        found.insert(w.clone());
        mentions += 1;
      }
      if !self.phrases.is_empty() {
        stream.push_str(&tok);
        stream.push(' ');
      }
    }
    for p in &self.phrases {
      let c = stream.matches(&format!(" {p} ")).count();
      if c > 0 {
        found.insert(p.clone());
        mentions += c;
      }
    }
    (found, mentions, n)
  }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
  let lower = text.to_lowercase().replace("a.i.", " ai ");
  lower
    .split(|c: char| !c.is_alphanumeric())
    .filter(|t| !t.is_empty() && t.len() <= 40)
    .map(String::from)
    .collect::<Vec<_>>()
    .into_iter()
}

pub struct Classifier {
  broad: Vocab,
  strong: Vocab,
  min: usize,
  rule: StrongRule,
}

impl Classifier {
  pub fn new(targets: &[String], min: usize) -> Self {
    Self::with_strong(targets, min, &[], StrongRule::default())
  }

  pub fn with_strong(
    targets: &[String],
    min: usize,
    strong_targets: &[String],
    rule: StrongRule,
  ) -> Self {
    Self {
      broad: Vocab::new(targets),
      strong: Vocab::new(strong_targets),
      min: min.max(1),
      rule,
    }
  }

  pub fn min(&self) -> usize {
    self.min
  }

  /// Distinct broad targets present in `text`, sorted.
  pub fn keywords(&self, text: &str) -> Vec<String> {
    self.broad.scan(text).0.into_iter().collect()
  }

  /// Full relevance analysis of a page.
  pub fn relevance(&self, title: &str, text: &str) -> Relevance {
    let (keywords, _, tokens_n) = self.broad.scan(text);
    if self.strong.is_empty() {
      return Relevance {
        keywords: keywords.into_iter().collect(),
        tokens: tokens_n,
        ..Default::default()
      };
    }
    let (strong, mentions, _) = self.strong.scan(text);
    let (title_hits, title_mentions, _) = self.strong.scan(title);
    let lede: String = text.chars().take(self.rule.lede_chars).collect();
    let (lede_hits, _, _) = self.strong.scan(&lede);
    Relevance {
      keywords: keywords.into_iter().collect(),
      strong: strong.into_iter().collect(),
      strong_mentions: mentions + title_mentions,
      tokens: tokens_n,
      strong_in_title: !title_hits.is_empty(),
      strong_in_lede: !lede_hits.is_empty(),
    }
  }

  pub fn is_hit(&self, r: &Relevance) -> bool {
    if r.keywords.len() < self.min {
      return false;
    }
    if self.strong.is_empty() {
      return true;
    }
    if r.strong_mentions < self.rule.min_mentions {
      return false;
    }
    if !self.rule.require_lede {
      return true;
    }
    r.strong_in_title
      || r.strong_in_lede
      || (r.strong_mentions >= self.rule.dense_mentions
        && r.strong_per_1000() >= self.rule.dense_per_1000
        && r.strong.len() >= self.rule.dense_distinct)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn counts_distinct_targets_case_insensitively() {
    let c = Classifier::new(&strs(&["ai", "machine", "learning"]), 2);
    let kw = c.keywords(
      "Machine learning, MACHINE learning, and more machine-learning. Said.",
    );
    assert_eq!(kw, vec!["learning", "machine"]);
    let r = c.relevance("t", "Machine learning rocks");
    assert!(c.is_hit(&r));
    let r = c.relevance("t", "The AI said hello");
    assert_eq!(r.keywords, vec!["ai"]);
    assert!(!c.is_hit(&r));
  }

  #[test]
  fn matches_phrases_across_punctuation() {
    let c = Classifier::new(
      &strs(&["Machine Learning", "large language model", "ai"]),
      1,
    );
    let kw =
      c.keywords("Uses machine-learning and a Large Language Model (LLM).");
    assert_eq!(kw, vec!["large language model", "machine learning"]);
    assert_eq!(c.keywords("learning machine"), Vec::<String>::new());
  }

  #[test]
  fn strong_rule_needs_the_story_to_be_about_ai() {
    let broad =
      strs(&["ai", "data", "computer", "model", "training", "software"]);
    let strong = strs(&["ai", "chatgpt", "machine learning"]);
    let c = Classifier::with_strong(&broad, 3, &strong, StrongRule::default());
    let chrome = "Samsung's new TV is cheaper. The model has better software and a computer inside. Data data training. Topics: AI, Gadgets.";
    let r = c.relevance("Samsung's new Frame Pro is cheaper", chrome);
    assert!(r.keywords.len() >= 3);
    assert!(!c.is_hit(&r), "one chrome mention is not an AI story");
    let body = "Hollywood keeps getting A.I. wrong. ChatGPT and machine learning are not magic; the AI model needs training data and computer time.";
    let r = c.relevance("What Hollywood Is Missing About A.I.", body);
    assert!(r.strong_in_title);
    assert!(r.strong_mentions >= 3);
    assert!(c.is_hit(&r));
    let dense = "ai chatgpt ".repeat(8) + "model data computer training";
    let lede_free = format!("{} {}", "x ".repeat(400), dense);
    let r = c.relevance("Opaque headline", &lede_free);
    assert!(!r.strong_in_lede);
    assert!(c.is_hit(&r), "dense body: {} per 1000", r.strong_per_1000());
  }
}
