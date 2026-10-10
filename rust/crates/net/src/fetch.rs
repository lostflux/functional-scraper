//! # Fetching
//!
//! A shared HTTP client with per-host `robots.txt` caching, content-type
//! sniffing and a hard cap on body size. Pages come back as decoded HTML
//! text or raw XML bytes (sitemaps and feeds); everything else is skipped.

use anyhow::Result;
use futures_util::StreamExt;
use reqwest::{Client, header, redirect::Policy};
use scraper_core::config::CrawlConfig;
use std::{collections::HashMap, io::Read, sync::Arc, time::Duration};
use texting_robots::Robot;
use tokio::sync::Mutex;
use url::Url;

/// What `robots.txt` told us about one host.
pub struct RobotsInfo {
  robot: Option<Robot>,
  /// Sitemaps advertised by `Sitemap:` lines.
  pub sitemaps: Vec<String>,
  /// `Crawl-delay`, if any.
  pub delay: Option<Duration>,
}

impl RobotsInfo {
  pub fn allowed(&self, url: &str) -> bool {
    self.robot.as_ref().is_none_or(|r| r.allowed(url))
  }
}

/// Result of one fetch.
pub enum Fetched {
  Html {
    url: Url,
    body: String,
  },
  Xml {
    url: Url,
    body: Vec<u8>,
  },
  /// 2xx with a content type we do not read.
  Skipped(String),
  /// Non-2xx status.
  Status(u16),
  Error(String),
}

pub struct Fetcher {
  client: Client,
  cfg: CrawlConfig,
  robots: Mutex<HashMap<String, Arc<RobotsInfo>>>,
}

impl Fetcher {
  pub fn new(cfg: &CrawlConfig) -> Result<Self> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
      header::ACCEPT,
      "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        .parse()?,
    );
    headers.insert(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9".parse()?);
    let client = Client::builder()
      .user_agent(&cfg.user_agent)
      .default_headers(headers)
      .timeout(Duration::from_secs(cfg.timeout_secs))
      .connect_timeout(Duration::from_secs(10))
      .redirect(Policy::limited(10))
      .gzip(true)
      .brotli(true)
      .deflate(true)
      .pool_max_idle_per_host(cfg.per_host_concurrency.max(1))
      .build()?;
    Ok(Self {
      client,
      cfg: cfg.clone(),
      robots: Mutex::new(HashMap::new()),
    })
  }

  /// `robots.txt` for the host of `url`, fetched once per host.
  pub async fn robots(&self, url: &Url) -> Arc<RobotsInfo> {
    let key = format!(
      "{}://{}",
      url.scheme(),
      url.host_str().unwrap_or_default().to_ascii_lowercase()
    );
    if let Some(info) = self.robots.lock().await.get(&key) {
      return info.clone();
    }
    let info = Arc::new(self.load_robots(&key).await);
    self.robots.lock().await.insert(key, info.clone());
    info
  }

  async fn load_robots(&self, origin: &str) -> RobotsInfo {
    let empty = RobotsInfo {
      robot: None,
      sitemaps: vec![],
      delay: None,
    };
    let resp = match self
      .client
      .get(format!("{origin}/robots.txt"))
      .timeout(Duration::from_secs(10))
      .send()
      .await
    {
      Ok(r) if r.status().is_success() => r,
      _ => return empty,
    };
    let Ok(body) = resp.bytes().await else {
      return empty;
    };
    if body.len() > 512 * 1024 {
      return empty;
    }
    match Robot::new(&self.cfg.user_agent, &body) {
      Ok(robot) => RobotsInfo {
        sitemaps: robot.sitemaps.clone(),
        delay: robot
          .delay
          .map(|s| Duration::from_secs_f32(s.clamp(0.0, 30.0))),
        robot: Some(robot),
      },
      Err(_) => empty,
    }
  }

  /// Fetches `url`, following redirects, and classifies the response.
  pub async fn fetch(&self, url: &Url) -> Fetched {
    let resp = match self.client.get(url.clone()).send().await {
      Ok(r) => r,
      Err(e) => return Fetched::Error(short_err(&e)),
    };
    let status = resp.status();
    if !status.is_success() {
      return Fetched::Status(status.as_u16());
    }
    let final_url = resp.url().clone();
    let content_type = resp
      .headers()
      .get(header::CONTENT_TYPE)
      .and_then(|v| v.to_str().ok())
      .unwrap_or("")
      .to_string();
    if let Some(len) = resp.content_length()
      && len as usize > self.cfg.max_body_bytes
    {
      return Fetched::Skipped(format!("{len} bytes"));
    }
    let ct = content_type.to_ascii_lowercase();
    let gz_sitemap = final_url.path().ends_with(".gz");
    let is_xml =
      ct.contains("xml") || ct.contains("rss") || ct.contains("atom");
    let is_html = ct.contains("html") || ct.is_empty();
    if !(is_xml || is_html || gz_sitemap || ct.contains("gzip")) {
      return Fetched::Skipped(content_type);
    }

    let mut body: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
      match chunk {
        Ok(c) => body.extend_from_slice(&c),
        Err(e) => return Fetched::Error(short_err(&e)),
      }
      if body.len() > self.cfg.max_body_bytes {
        break;
      }
    }
    if body.starts_with(&[0x1f, 0x8b]) {
      let mut out = Vec::new();
      let mut dec = flate2::read::GzDecoder::new(&body[..])
        .take(self.cfg.max_body_bytes as u64);
      if dec.read_to_end(&mut out).is_err() {
        return Fetched::Error("bad gzip".into());
      }
      body = out;
    }
    if scraper_extract::xml::looks_like_xml(&content_type, &body) {
      return Fetched::Xml {
        url: final_url,
        body,
      };
    }
    if !is_html {
      return Fetched::Skipped(content_type);
    }
    Fetched::Html {
      url: final_url,
      body: decode_html(&content_type, &body),
    }
  }
}

/// Decodes HTML bytes using the header charset, then a `<meta charset>`,
/// then UTF-8.
fn decode_html(content_type: &str, body: &[u8]) -> String {
  let label = charset_param(content_type).or_else(|| meta_charset(body));
  let enc = label
    .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
    // Per the HTML spec a declared UTF-16 on a byte stream means UTF-8.
    .filter(|e| *e != encoding_rs::UTF_16LE && *e != encoding_rs::UTF_16BE)
    .unwrap_or(encoding_rs::UTF_8);
  enc.decode(body).0.into_owned()
}

fn charset_param(content_type: &str) -> Option<String> {
  content_type.split(';').skip(1).find_map(|p| {
    let p = p.trim();
    p.strip_prefix("charset=")
      .or_else(|| p.strip_prefix("CHARSET="))
      .map(|c| c.trim_matches('"').to_string())
  })
}

fn meta_charset(body: &[u8]) -> Option<String> {
  let head =
    String::from_utf8_lossy(&body[..body.len().min(4096)]).to_ascii_lowercase();
  let idx = head.find("charset=")?;
  let rest = head[idx + 8..].trim_start_matches(['"', '\'', ' ']);
  let end = rest.find(|c: char| {
    c == '"' || c == '\'' || c == ';' || c == '>' || c.is_whitespace()
  })?;
  Some(rest[..end].trim_matches('"').to_string())
}

fn short_err(e: &reqwest::Error) -> String {
  if e.is_timeout() {
    "timeout".into()
  } else if e.is_connect() {
    "connect".into()
  } else if e.is_redirect() {
    "redirect loop".into()
  } else {
    let s = e.to_string();
    s.split(':').next_back().unwrap_or(&s).trim().to_string()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn charset_detection() {
    assert_eq!(
      charset_param("text/html; charset=ISO-8859-1").as_deref(),
      Some("ISO-8859-1")
    );
    assert_eq!(charset_param("text/html"), None);
    assert_eq!(
      meta_charset(
        br#"<html><head><meta charset="windows-1252"><title>x</title>"#
      )
      .as_deref(),
      Some("windows-1252")
    );
    let latin1 = b"<p>caf\xe9</p>";
    assert_eq!(
      decode_html("text/html; charset=iso-8859-1", latin1),
      "<p>café</p>"
    );
  }
}
