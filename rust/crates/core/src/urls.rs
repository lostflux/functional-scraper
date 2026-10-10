//! URL normalisation, scoping and hashing.
//!
//! Every URL that enters the frontier or the seen-set goes through
//! [`normalize`], so `https://www.x.com/a/?utm=1#top` and
//! `https://www.x.com:443/a` collapse to one key. Scheme and `www.` are
//! kept (they are different URLs as far as servers are concerned); pages
//! reached through both are deduplicated later by their canonical URL.

use crate::config::{Config, Scope};
use url::Url;
use xxhash_rust::xxh3::xxh3_64;

/// 64-bit key for a normalised URL string; this is what the seen-set stores
/// instead of the URL itself.
pub fn hash(canonical: &str) -> u64 {
  xxh3_64(canonical.as_bytes())
}

/// Host without a leading `www.`.
pub fn bare_host(url: &Url) -> Option<String> {
  url
    .host_str()
    .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
}

/// Parse `href` relative to `base` and normalise it. Returns `None` for
/// anything that is not an `http(s)` document link.
pub fn resolve(base: &Url, href: &str) -> Option<Url> {
  resolve_with(base, href, false)
}

/// [`resolve`] with control over the query string. Seeds (feeds, sitemaps,
/// search URLs) need their query kept; ordinary links do not.
pub fn resolve_with(base: &Url, href: &str, keep_query: bool) -> Option<Url> {
  let href = href.trim();
  if href.is_empty() {
    return None;
  }
  let lower = href.to_ascii_lowercase();
  if lower.starts_with("javascript:")
    || lower.starts_with("mailto:")
    || lower.starts_with("tel:")
    || lower.starts_with("data:")
  {
    return None;
  }
  let joined = base.join(href).ok()?;
  normalize_with(joined, keep_query)
}

/// Canonical form: lowercase scheme/host, no fragment, no query, no default
/// port, no trailing slash, no credentials.
pub fn normalize(url: Url) -> Option<Url> {
  normalize_with(url, false)
}

/// [`normalize`], optionally keeping the query string.
pub fn normalize_with(mut url: Url, keep_query: bool) -> Option<Url> {
  match url.scheme() {
    "http" | "https" => {}
    _ => return None,
  }
  url.host_str()?;
  url.set_fragment(None);
  if !keep_query || url.query().is_some_and(str::is_empty) {
    url.set_query(None);
  }
  let _ = url.set_username("");
  let _ = url.set_password(None);
  if let Some(p) = url.port() {
    let default = if url.scheme() == "https" { 443 } else { 80 };
    if p == default {
      let _ = url.set_port(None);
    }
  }
  let path = url.path().to_string();
  if path.len() > 1 && path.ends_with('/') {
    url.set_path(path.trim_end_matches('/'));
  }
  Some(url)
}

/// String form used for hashing and storage: like `Url::as_str` but with
/// the root path's trailing slash removed (`https://x.com` not `https://x.com/`).
pub fn canonical(url: &Url) -> String {
  let s = url.as_str();
  if url.path() == "/" && url.query().is_none() {
    s.trim_end_matches('/').to_string()
  } else {
    s.to_string()
  }
}

const BINARY_EXT: &[&str] = &[
  ".pdf", ".zip", ".gz", ".tar", ".jpg", ".jpeg", ".png", ".gif", ".svg",
  ".webp", ".ico", ".mp3", ".mp4", ".m4a", ".mov", ".avi", ".webm", ".css",
  ".js", ".json", ".woff", ".woff2", ".ttf", ".exe", ".dmg", ".pkg", ".apk",
];

/// A scope rule derived from one `domains` entry: host (sans `www.`) plus an
/// optional path prefix, e.g. `https://www.ibm.com/think` → (`ibm.com`, `/think`).
/// Hosts match exactly: `shop.theverge.com` is not covered by
/// `theverge.com`; list subdomains you want explicitly.
#[derive(Debug, Clone)]
pub struct DomainRule {
  pub host: String,
  pub path_prefix: String,
}

impl DomainRule {
  pub fn parse(entry: &str) -> Option<Self> {
    let url = Url::parse(entry.trim()).ok()?;
    let host = bare_host(&url)?;
    let path_prefix = url.path().trim_end_matches('/').to_string();
    Some(Self { host, path_prefix })
  }

  pub fn matches(&self, url: &Url) -> bool {
    let Some(host) = bare_host(url) else {
      return false;
    };
    let path = url.path();
    host == self.host
      && (self.path_prefix.is_empty()
        || path == self.path_prefix
        || path.starts_with(&format!("{}/", self.path_prefix)))
  }
}

/// Decides which links are worth following.
#[derive(Debug, Clone)]
pub struct Scoper {
  scope: Scope,
  rules: Vec<DomainRule>,
  excludes: Vec<String>,
  deny_hosts: Vec<String>,
}

impl Scoper {
  pub fn new(cfg: &Config) -> Self {
    Self {
      scope: cfg.crawl.scope,
      rules: cfg
        .domains
        .iter()
        .filter_map(|d| DomainRule::parse(d))
        .collect(),
      excludes: cfg.exclude_patterns.clone(),
      deny_hosts: cfg
        .deny_hosts
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect(),
    }
  }

  pub fn scope(&self) -> Scope {
    self.scope
  }

  /// True if the URL is in one of the configured `domains`.
  pub fn in_domains(&self, url: &Url) -> bool {
    self.rules.iter().any(|r| r.matches(url))
  }

  pub fn denied_host(&self, url: &Url) -> bool {
    let Some(host) = bare_host(url) else {
      return true;
    };
    self
      .deny_hosts
      .iter()
      .any(|d| host == *d || host.ends_with(&format!(".{d}")))
  }

  fn looks_binary(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    BINARY_EXT.iter().any(|ext| path.ends_with(ext))
  }

  /// A pattern ending in `/` (`/tag/`) matches anywhere in the URL; any
  /// other pattern (`/search`) must be a whole path segment, so it does not
  /// catch slugs like `/searchgpt-review`.
  fn excluded_by(url: &Url, pattern: &str) -> bool {
    if pattern.ends_with('/') || !pattern.starts_with('/') {
      return url.as_str().contains(pattern);
    }
    let seg = &pattern[1..];
    url.path().split('/').any(|s| s == seg)
  }

  /// Should this (already normalised) URL be followed?
  pub fn allows(&self, url: &Url) -> bool {
    if Self::looks_binary(url) {
      return false;
    }
    if self.excludes.iter().any(|p| Self::excluded_by(url, p)) {
      return false;
    }
    match self.scope {
      Scope::Domains => self.in_domains(url),
      Scope::Open => !self.denied_host(url),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn cfg(scope: &str) -> Config {
    serde_yaml::from_str(&format!(
            "targets: [ai]\ndomains: [https://venturebeat.com, https://www.ibm.com/think]\ncrawl:\n  scope: {scope}\n"
        ))
        .unwrap()
  }

  #[test]
  fn normalises_variants_to_one_key() {
    let base = Url::parse("https://www.example.com/news/").unwrap();
    let a = resolve(&base, "/a/b/?utm_source=x#frag").unwrap();
    let b = resolve(&base, "https://www.example.com:443/a/b").unwrap();
    assert_eq!(canonical(&a), "https://www.example.com/a/b");
    assert_eq!(canonical(&a), canonical(&b));
    assert_eq!(hash(&canonical(&a)), hash(&canonical(&b)));
    let root = resolve(&base, "https://example.com/").unwrap();
    assert_eq!(canonical(&root), "https://example.com");
    let feed =
      resolve_with(&base, "https://news.example.com/rss?q=ai&hl=en", true)
        .unwrap();
    assert_eq!(canonical(&feed), "https://news.example.com/rss?q=ai&hl=en");
  }

  #[test]
  fn rejects_non_http() {
    let base = Url::parse("https://example.com").unwrap();
    assert!(resolve(&base, "mailto:a@b.c").is_none());
    assert!(resolve(&base, "javascript:void(0)").is_none());
    assert!(resolve(&base, "ftp://example.com/x").is_none());
  }

  #[test]
  fn domain_rules_handle_www_and_path_prefix() {
    let s = Scoper::new(&cfg("domains"));
    let ok = |u: &str| s.allows(&Url::parse(u).unwrap());
    assert!(ok("https://venturebeat.com/ai/foo"));
    assert!(ok("https://www.venturebeat.com/ai/foo"));
    assert!(ok("https://www.ibm.com/think/topics/ai"));
    assert!(ok("https://www.ibm.com/think"));
    assert!(!ok("https://www.ibm.com/thinkpad"));
    assert!(!ok("https://www.ibm.com/products"));
    assert!(!ok("https://venturebeat.com/author/kyle"));
    assert!(!ok("https://venturebeat.com/report.pdf"));
    assert!(!ok("https://other.com/ai"));
    assert!(
      !ok("https://shop.venturebeat.com/ai"),
      "subdomains are not implied"
    );
  }

  #[test]
  fn open_scope_uses_deny_list() {
    let s = Scoper::new(&cfg("open"));
    let ok = |u: &str| s.allows(&Url::parse(u).unwrap());
    assert!(ok("https://other.com/ai"));
    assert!(!ok("https://www.youtube.com/watch"));
    assert!(!ok("https://m.facebook.com/x"));
    assert!(!ok("https://other.com/tag/ai"));
    assert!(!ok("https://other.com/search"));
    assert!(!ok("https://other.com/en/search/ai"));
    assert!(ok("https://other.com/2025/searchgpt-review"));
  }
}
