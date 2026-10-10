//! # HTML Extraction
//!
//! Turns a fetched HTML document into a [`Page`]: title, article text,
//! publication date, language and outgoing links.
//!
//! Text is taken from block-level content elements (`p`, headings, list
//! items, table cells, …) inside the most article-like container
//! (`<article>`, then `<main>`, then `<body>`), skipping navigation,
//! headers, footers, scripts and the like. Each content element becomes one
//! line, which matches the one-paragraph-per-line layout of the legacy
//! `data/log/<id>.txt` files.

use crate::dates;
use ego_tree::{NodeRef, iter::Edge};
use scraper::{Html, Node, Selector};
use scraper_core::urls;
use std::sync::LazyLock;
use url::Url;

/// Everything we keep from one HTML page.
#[derive(Debug, Clone)]
pub struct Page {
  /// `<link rel=canonical>` if present and sane, else the fetched URL.
  pub url: Url,
  pub title: String,
  /// One content block per line.
  pub text: String,
  /// `YYYY-MM-DD` when a full date was found.
  pub date: Option<String>,
  pub year: Option<i32>,
  /// Which heuristic produced the date: `meta`, `jsonld`, `time`, `url`,
  /// `byline` or `text`.
  pub date_source: Option<&'static str>,
  /// `<html lang>`, lowercased.
  pub lang: Option<String>,
  /// `<meta property="og:type">`, lowercased (`article` on most news sites).
  pub og_type: Option<String>,
  /// A JSON-LD block declares an Article-like `@type`.
  pub jsonld_article: bool,
  /// Resolved, normalised outgoing links (not yet filtered for scope).
  pub links: Vec<Url>,
}

impl Page {
  /// Number of characters of extracted text.
  pub fn text_len(&self) -> usize {
    self.text.chars().count()
  }

  /// Share of the text that sits in paragraph-length blocks (120+ chars).
  /// Articles score high; home, section and listing pages are mostly short
  /// teasers and score low.
  pub fn prose_ratio(&self) -> f64 {
    let (mut long, mut total) = (0usize, 0usize);
    for line in self.text.lines() {
      let n = line.chars().count();
      total += n;
      if n >= 120 {
        long += n;
      }
    }
    if total == 0 {
      0.0
    } else {
      long as f64 / total as f64
    }
  }

  /// Does the page carry a structural signal that it is an article rather
  /// than a listing, home or tag page? Never for a site root; otherwise
  /// when `og:type` or JSON-LD says so, or when the publication date came
  /// from article-specific metadata or a dated URL path. Weak date tags
  /// (`dc.date`, `og:updated_time`, `<time>`, bylines) do not count.
  pub fn looks_like_article(&self) -> bool {
    if self.url.path().trim_end_matches('/').is_empty() {
      return false;
    }
    self
      .og_type
      .as_deref()
      .is_some_and(|t| t.starts_with("article"))
      || self.jsonld_article
      || matches!(self.date_source, Some("meta" | "jsonld" | "url"))
      // Blogs without article metadata (Hugging Face, Google Research):
      // a visible <time> plus a long, mostly-prose body is enough.
      || (self.date_source == Some("time")
        && self.prose_ratio() >= 0.8
        && self.text_len() >= 1500)
  }
}

macro_rules! sel {
  ($name:ident, $css:expr) => {
    static $name: LazyLock<Selector> =
      LazyLock::new(|| Selector::parse($css).unwrap());
  };
}

sel!(SEL_TITLE, "title");
sel!(SEL_OG_TITLE, r#"meta[property="og:title"]"#);
sel!(SEL_OG_TYPE, r#"meta[property="og:type"]"#);
sel!(SEL_CANONICAL, r#"link[rel="canonical"]"#);
sel!(SEL_BASE, "base[href]");
sel!(SEL_HTML, "html");
sel!(SEL_LINKS, "a[href]");
sel!(SEL_ARTICLE, "article");
sel!(SEL_MAIN, r#"main, [role="main"]"#);
sel!(SEL_BODY, "body");
sel!(SEL_JSONLD, r#"script[type="application/ld+json"]"#);
sel!(SEL_TIME, "time[datetime], time[pubdate]");
sel!(
  SEL_DATE_META_STRONG,
  concat!(
    r#"meta[property="article:published_time"],"#,
    r#"meta[name="article:published_time"],"#,
    r#"meta[property="og:article:published_time"],"#,
    r#"meta[itemprop="datePublished"],"#,
    r#"meta[name="pubdate"],meta[name="publishdate"],"#,
    r#"meta[name="publish-date"],meta[name="publish_date"],"#,
    r#"meta[name="publication_date"],"#,
    r#"meta[name="sailthru.date"],meta[name="parsely-pub-date"]"#
  )
);
sel!(
  SEL_DATE_META_WEAK,
  concat!(
    r#"meta[name="date"],meta[name="dc.date"],meta[name="DC.date"],"#,
    r#"meta[name="dc.date.issued"],meta[name="DC.date.issued"],"#,
    r#"meta[property="og:updated_time"]"#
  )
);
sel!(
  SEL_BYLINE,
  r#"[class*="date"], [class*="byline"], [class*="timestamp"], [class*="published"]"#
);

/// Elements whose whole subtree is ignored.
const SKIP: &[&str] = &[
  "script", "style", "noscript", "template", "svg", "iframe", "nav", "header",
  "footer", "aside", "form", "button", "select", "option", "textarea", "input",
  "label", "menu", "dialog", "canvas", "video", "audio", "object",
];

/// Elements whose text is taken as one block (subtree not descended).
const CONTENT: &[&str] = &[
  "p",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "li",
  "blockquote",
  "pre",
  "td",
  "th",
  "dd",
  "dt",
  "figcaption",
  "summary",
];

/// Minimum length for a bare text node (e.g. text directly inside a `div`)
/// to be kept.
const MIN_LOOSE_TEXT: usize = 80;

fn squash(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut space = true;
  for c in s.chars() {
    if c.is_whitespace() {
      if !space {
        out.push(' ');
        space = true;
      }
    } else {
      out.push(c);
      space = false;
    }
  }
  out.truncate(out.trim_end().len());
  out
}

/// Elements that imply a word boundary around their text. Inline elements
/// (`span`, `a`, `b`, …) do not, so `<span>T</span>he` stays one word.
const BLOCK: &[&str] = &[
  "p",
  "div",
  "section",
  "article",
  "header",
  "footer",
  "aside",
  "nav",
  "ul",
  "ol",
  "li",
  "table",
  "tr",
  "td",
  "th",
  "thead",
  "tbody",
  "tfoot",
  "caption",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "blockquote",
  "pre",
  "figure",
  "figcaption",
  "dl",
  "dt",
  "dd",
  "br",
  "hr",
  "main",
  "form",
  "details",
  "summary",
  "address",
];

fn is_hidden(el: &scraper::node::Element) -> bool {
  el.attr("hidden").is_some()
    || el.attr("aria-hidden") == Some("true")
    || el
      .attr("style")
      .is_some_and(|s| s.replace(' ', "").contains("display:none"))
}

fn skippable(el: &scraper::node::Element) -> bool {
  SKIP.contains(&el.name()) || is_hidden(el)
}

/// Text of `node`'s subtree with ignored elements pruned. Iterative (an
/// explicit traversal with a skip counter), so pathological nesting cannot
/// overflow the stack.
fn text_of(node: NodeRef<'_, Node>, out: &mut String) {
  let mut skip = 0usize;
  for edge in node.traverse() {
    match edge {
      Edge::Open(n) => {
        if skip > 0 {
          skip += 1;
          continue;
        }
        match n.value() {
          Node::Element(el) if n.id() != node.id() && skippable(el) => skip = 1,
          Node::Element(el) if BLOCK.contains(&el.name()) => out.push(' '),
          Node::Text(t) => out.push_str(&t.text),
          _ => {}
        }
      }
      Edge::Close(n) => {
        if skip > 0 {
          skip -= 1;
          continue;
        }
        if let Node::Element(el) = n.value()
          && BLOCK.contains(&el.name())
        {
          out.push(' ');
        }
      }
    }
  }
}

/// Collects one block per content element under `root`, in document
/// order, plus any long bare text node. Iterative for the same reason as
/// [`text_of`].
fn collect(root: NodeRef<'_, Node>, blocks: &mut Vec<String>) {
  let mut skip = 0usize;
  for edge in root.traverse() {
    match edge {
      Edge::Open(n) => {
        if skip > 0 {
          skip += 1;
          continue;
        }
        if n.id() == root.id() {
          continue;
        }
        match n.value() {
          Node::Element(el) => {
            if skippable(el) {
              skip = 1;
            } else if CONTENT.contains(&el.name()) {
              let mut buf = String::new();
              text_of(n, &mut buf);
              let s = squash(&buf);
              if s.chars().count() >= 2 {
                blocks.push(s);
              }
              skip = 1;
            }
          }
          Node::Text(t) => {
            let s = squash(&t.text);
            if s.chars().count() >= MIN_LOOSE_TEXT {
              blocks.push(s);
            }
          }
          _ => {}
        }
      }
      Edge::Close(_) => {
        skip = skip.saturating_sub(1);
      }
    }
  }
}

fn blocks_in(root: scraper::ElementRef<'_>) -> Vec<String> {
  let mut blocks = Vec::new();
  collect(*root, &mut blocks);
  blocks
}

/// Picks the container most likely to hold the article body.
fn article_root(doc: &Html) -> Option<scraper::ElementRef<'_>> {
  let longest = |sel: &Selector| {
    doc.select(sel).max_by_key(|el| {
      let mut buf = String::new();
      text_of(**el, &mut buf);
      buf.len()
    })
  };
  longest(&SEL_ARTICLE)
    .or_else(|| longest(&SEL_MAIN))
    .or_else(|| doc.select(&SEL_BODY).next())
}

fn extract_text(doc: &Html, min_chars: usize) -> String {
  let mut blocks = article_root(doc).map(blocks_in).unwrap_or_default();
  let len: usize = blocks.iter().map(|b| b.chars().count()).sum();
  if len < min_chars
    && let Some(body) = doc.select(&SEL_BODY).next()
  {
    let wider = blocks_in(body);
    if wider.iter().map(|b| b.chars().count()).sum::<usize>() > len {
      blocks = wider;
    }
  }
  blocks.join("\n")
}

fn extract_title(doc: &Html) -> String {
  let og = doc
    .select(&SEL_OG_TITLE)
    .next()
    .and_then(|m| m.attr("content"))
    .map(squash)
    .filter(|s| !s.is_empty());
  og.unwrap_or_else(|| {
    doc
      .select(&SEL_TITLE)
      .next()
      .map(|t| squash(&t.text().collect::<String>()))
      .unwrap_or_default()
  })
}

static JSONLD_DATE: LazyLock<regex::Regex> = LazyLock::new(|| {
  regex::Regex::new(r#""datePublished"\s*:\s*"([^"]+)""#).unwrap()
});
static JSONLD_ARTICLE: LazyLock<regex::Regex> = LazyLock::new(|| {
  regex::Regex::new(
    r#""@type"\s*:\s*\[?\s*"(?:News|Blog|Tech|Scholarly|Analysis|Reportage|Opinion|Review|Background|LiveBlog)?(?:Article|Posting)""#,
  )
  .unwrap()
});

/// Text of the JSON-LD blocks that describe an article.
fn jsonld_articles(doc: &Html) -> Vec<String> {
  doc
    .select(&SEL_JSONLD)
    .map(|s| s.text().collect::<String>())
    .filter(|body| JSONLD_ARTICLE.is_match(body))
    .collect()
}

/// Returns `(date, year, source)` using the first heuristic that works.
fn extract_date(
  doc: &Html,
  url: &Url,
  text: &str,
  jsonld: &[String],
) -> (Option<String>, Option<i32>, Option<&'static str>) {
  use chrono::Datelike;
  let full = |d: chrono::NaiveDate, src: &'static str| {
    (
      Some(d.format("%Y-%m-%d").to_string()),
      Some(d.year()),
      Some(src),
    )
  };
  for m in doc.select(&SEL_DATE_META_STRONG) {
    if let Some(d) = m.attr("content").and_then(dates::parse_date) {
      return full(d, "meta");
    }
  }
  for body in jsonld {
    if let Some(c) = JSONLD_DATE.captures(body)
      && let Some(d) = dates::parse_date(&c[1])
    {
      return full(d, "jsonld");
    }
  }
  if let Some((year, d)) = dates::date_from_url_path(url.path()) {
    return match d {
      Some(d) => full(d, "url"),
      None => (None, Some(year), Some("url")),
    };
  }
  for m in doc.select(&SEL_DATE_META_WEAK) {
    if let Some(d) = m.attr("content").and_then(dates::parse_date) {
      return full(d, "meta:weak");
    }
  }
  for t in doc.select(&SEL_TIME) {
    let cand = t
      .attr("datetime")
      .map(str::to_string)
      .unwrap_or_else(|| t.text().collect());
    if let Some(d) = dates::parse_date(&cand) {
      return full(d, "time");
    }
  }
  for el in doc.select(&SEL_BYLINE).take(20) {
    let s: String = el.text().collect();
    if let Some(d) = dates::parse_date(&s) {
      return full(d, "byline");
    }
  }
  match dates::max_year_in_text(text) {
    Some(y) => (None, Some(y), Some("text")),
    None => (None, None, None),
  }
}

/// Parses `body` fetched from `fetched_url`.
///
/// # Arguments
///
/// * `fetched_url` — The final URL after redirects; relative links are
///   resolved against it.
/// * `body` — The HTML source.
/// * `min_text_chars` — Below this, the extractor widens its search from
///   the article container to the whole body.
pub fn parse_html(
  fetched_url: &Url,
  body: &str,
  min_text_chars: usize,
) -> Page {
  let doc = Html::parse_document(body);

  let base = doc
    .select(&SEL_BASE)
    .next()
    .and_then(|b| b.attr("href"))
    .and_then(|h| fetched_url.join(h).ok())
    .unwrap_or_else(|| fetched_url.clone());
  let fetched_norm =
    urls::normalize(fetched_url.clone()).unwrap_or_else(|| fetched_url.clone());
  let is_root = |u: &Url| u.path().trim_end_matches('/').is_empty();
  let canonical = doc
    .select(&SEL_CANONICAL)
    .next()
    .and_then(|l| l.attr("href"))
    .and_then(|h| urls::resolve(&base, h))
    .filter(|c| urls::bare_host(c) == urls::bare_host(fetched_url))
    .filter(|c| !is_root(c) || is_root(&fetched_norm));
  let url = canonical.unwrap_or(fetched_norm);

  let lang = doc
    .select(&SEL_HTML)
    .next()
    .and_then(|h| h.attr("lang"))
    .map(|l| l.trim().to_ascii_lowercase())
    .filter(|l| !l.is_empty());

  let og_type = doc
    .select(&SEL_OG_TYPE)
    .next()
    .and_then(|m| m.attr("content"))
    .map(|t| t.trim().to_ascii_lowercase())
    .filter(|t| !t.is_empty());

  let title = extract_title(&doc);
  let text = extract_text(&doc, min_text_chars);
  let jsonld = jsonld_articles(&doc);
  let (date, year, date_source) = extract_date(&doc, &url, &text, &jsonld);

  let mut seen = std::collections::HashSet::new();
  let mut links = Vec::new();
  for a in doc.select(&SEL_LINKS) {
    if let Some(href) = a.attr("href")
      && let Some(u) = urls::resolve(&base, href)
      && seen.insert(urls::canonical(&u))
    {
      links.push(u);
    }
  }

  Page {
    url,
    title,
    text,
    date,
    year,
    date_source,
    lang,
    og_type,
    jsonld_article: !jsonld.is_empty(),
    links,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const DOC: &str = r#"<!doctype html><html lang="en-US"><head>
    <title>Robots learn | Example</title>
    <meta property="og:title" content="Robots learn to fold laundry">
    <meta property="article:published_time" content="2024-03-01T09:00:00Z">
    <meta property="og:type" content="Article">
    <link rel="canonical" href="https://example.com/2024/03/01/robots?utm=1">
    </head><body>
    <nav><ul><li><a href="/about">About</a></li></ul></nav>
    <article>
      <h1>Robots learn to fold laundry</h1>
      <p>Machine learning <span>lets</span> robots fold.</p>
      <p>Second   paragraph.</p>
      <div>This bare text is long enough to count as a paragraph on its own, honest, with room to spare.</div>
      <script>var x = "ignore me";</script>
      <a href="related.html">Related</a>
      <a href="mailto:x@y.z">mail</a>
    </article>
    <footer><p>© 2019 Example</p></footer>
    </body></html>"#;

  #[test]
  fn extracts_everything() {
    let url = Url::parse("https://example.com/2024/03/01/robots").unwrap();
    let p = parse_html(&url, DOC, 10);
    assert_eq!(p.title, "Robots learn to fold laundry");
    assert_eq!(p.url.as_str(), "https://example.com/2024/03/01/robots");
    assert_eq!(p.lang.as_deref(), Some("en-us"));
    assert_eq!(p.date.as_deref(), Some("2024-03-01"));
    assert_eq!(p.year, Some(2024));
    assert_eq!(p.date_source, Some("meta"));
    assert_eq!(p.og_type.as_deref(), Some("article"));
    assert!(p.looks_like_article());
    let lines: Vec<&str> = p.text.lines().collect();
    assert_eq!(
      lines,
      vec![
        "Robots learn to fold laundry",
        "Machine learning lets robots fold.",
        "Second paragraph.",
        "This bare text is long enough to count as a paragraph on its own, honest, with room to spare.",
      ]
    );
    let links: Vec<String> = p.links.iter().map(|u| u.to_string()).collect();
    assert_eq!(
      links,
      vec![
        "https://example.com/about",
        "https://example.com/2024/03/01/related.html"
      ]
    );
  }

  #[test]
  fn falls_back_to_url_and_text_years() {
    let url = Url::parse("https://example.com/2021/02/story").unwrap();
    let p =
      parse_html(&url, "<html><body><p>In 2018 we saw.</p></body></html>", 0);
    assert_eq!(p.year, Some(2021));
    assert_eq!(p.date, None);
    assert_eq!(p.date_source, Some("url"));
    let url = Url::parse("https://example.com/story").unwrap();
    let p =
      parse_html(&url, "<html><body><p>In 2018 we saw.</p></body></html>", 0);
    assert_eq!(p.year, Some(2018));
    assert_eq!(p.date_source, Some("text"));
    assert!(!p.looks_like_article());
  }

  #[test]
  fn homepages_and_weak_dates_are_not_articles() {
    let home = Url::parse("https://example.com/").unwrap();
    let html = r#"<html><head><meta name="dc.date" content="2024-04-16">
      <script type="application/ld+json">{"@type":"WebSite","datePublished":"2024-01-02"}</script>
      </head><body><p>Lots of AI news on the front page, machine learning and more.</p></body></html>"#;
    let p = parse_html(&home, html, 0);
    assert_eq!(p.date_source, Some("meta:weak"));
    assert!(!p.jsonld_article);
    assert!(!p.looks_like_article());
    let section = Url::parse("https://example.com/category/ai").unwrap();
    let p = parse_html(&section, html, 0);
    assert!(
      !p.looks_like_article(),
      "weak date on a non-root page is still not enough"
    );
    let post = Url::parse("https://example.com/posts/robots").unwrap();
    let html = r#"<html><head><script type="application/ld+json">
      {"@context":"https://schema.org","@type":"NewsArticle","datePublished":"2024-05-06T10:00:00Z"}
      </script></head><body><p>Body.</p></body></html>"#;
    let p = parse_html(&post, html, 0);
    assert!(p.jsonld_article);
    assert_eq!(p.date.as_deref(), Some("2024-05-06"));
    assert_eq!(p.date_source, Some("jsonld"));
    assert!(p.looks_like_article());
  }

  #[test]
  fn inline_elements_do_not_split_words_but_blocks_do() {
    let url = Url::parse("https://example.com/x").unwrap();
    let html = "<html><body><article><p><span class=dropcap>T</span>he <b>Chat</b>GPT model<br>next line</p><ul><li>one</li><li>two</li></ul></article></body></html>";
    let p = parse_html(&url, html, 0);
    assert_eq!(p.text, "The ChatGPT model next line\none\ntwo");
  }

  #[test]
  fn survives_pathological_nesting() {
    let url = Url::parse("https://example.com/x").unwrap();
    let mut html = String::from("<html><body><article>");
    for _ in 0..12_000 {
      html.push_str("<div>");
    }
    html.push_str("<p>deep text</p>");
    let p = parse_html(&url, &html, 0);
    assert!(p.text.contains("deep text"));
  }

  #[test]
  fn normalises_redirect_targets_and_ignores_home_canonicals() {
    let fetched =
      Url::parse("https://www.example.com/a/?utm_source=x").unwrap();
    let p = parse_html(&fetched, "<html><body><p>hi</p></body></html>", 0);
    assert_eq!(p.url.as_str(), "https://www.example.com/a");
    let html = r#"<html><head><link rel="canonical" href="https://example.com/"><base href="https://www.example.com/sub/"></head><body><a href="rel">r</a></body></html>"#;
    let p = parse_html(&fetched, html, 0);
    assert_eq!(
      p.url.as_str(),
      "https://www.example.com/a",
      "root canonical ignored"
    );
    assert_eq!(
      p.links[0].as_str(),
      "https://www.example.com/sub/rel",
      "base href honoured"
    );
  }

  #[test]
  fn widens_to_body_when_article_is_thin() {
    let url = Url::parse("https://example.com/x").unwrap();
    let html = "<html><body><article><p>Teaser.</p></article><div><p>The real body lives outside the article element for some reason, and it is long.</p></div></body></html>";
    let p = parse_html(&url, html, 50);
    assert!(p.text.contains("real body"));
  }
}
