//! Link extraction from sitemaps (plain and index) and RSS/Atom feeds.

use quick_xml::{Reader, events::Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLink {
  pub url: String,
  /// `lastmod` / `pubDate` / `published`, raw.
  pub date: Option<String>,
  /// True for `<sitemap><loc>` entries of a sitemap index.
  pub nested: bool,
}

/// True if the body looks like XML we know how to read.
pub fn looks_like_xml(content_type: &str, body: &[u8]) -> bool {
  let ct = content_type.to_ascii_lowercase();
  if ct.contains("html") {
    return false;
  }
  if ct.contains("rss") || ct.contains("atom") {
    return true;
  }
  let head: String = String::from_utf8_lossy(&body[..body.len().min(1024)])
    .trim_start()
    .to_ascii_lowercase();
  let mut rest = head.as_str();
  loop {
    rest = rest.trim_start();
    if let Some(r) = rest.strip_prefix("<?xml") {
      rest = r.split_once("?>").map_or("", |(_, r)| r);
    } else if let Some(r) = rest.strip_prefix("<!--") {
      rest = r.split_once("-->").map_or("", |(_, r)| r);
    } else {
      break;
    }
  }
  ["<urlset", "<sitemapindex", "<rss", "<feed", "<rdf:rdf"]
    .iter()
    .any(|root| rest.starts_with(root))
    || (ct.contains("xml")
      && !rest.starts_with("<html")
      && !rest.starts_with("<!doctype"))
}

fn local(name: &[u8]) -> &[u8] {
  name.rsplit(|&b| b == b':').next().unwrap_or(name)
}

fn apply_text(
  target: Option<&'static str>,
  current: Option<&mut FeedLink>,
  s: &str,
) {
  let (Some(target), Some(c)) = (target, current) else {
    return;
  };
  let s = s.trim().to_string();
  match target {
    "url" if c.url.is_empty() && s.starts_with("http") => c.url = s,
    "date" if c.date.is_none() && !s.is_empty() => c.date = Some(s),
    _ => {}
  }
}

pub fn extract_links(body: &[u8]) -> Vec<FeedLink> {
  let mut reader = Reader::from_reader(body);
  reader.config_mut().trim_text(true);
  reader.config_mut().check_end_names = false;
  let mut out = Vec::new();
  let mut stack: Vec<Vec<u8>> = Vec::new();
  let mut current: Option<FeedLink> = None;
  let mut text_target: Option<&'static str> = None;
  let mut buf = Vec::new();

  loop {
    match reader.read_event_into(&mut buf) {
      Ok(Event::Start(e)) => {
        let name = local(e.name().as_ref()).to_ascii_lowercase();
        let parent = stack.last().map(|p| p.as_slice()).unwrap_or(b"");
        match name.as_slice() {
          b"url" | b"item" | b"entry" => {
            current = Some(FeedLink {
              url: String::new(),
              date: None,
              nested: false,
            })
          }
          b"sitemap" => {
            current = Some(FeedLink {
              url: String::new(),
              date: None,
              nested: true,
            })
          }
          b"loc" | b"guid" => text_target = Some("url"),
          b"link" if parent == b"item" => text_target = Some("url"),
          b"link" if parent == b"entry" => {
            let rel = e
              .attributes()
              .flatten()
              .find(|a| a.key.as_ref() == b"rel")
              .map(|a| a.value.to_vec());
            let href = e
              .attributes()
              .flatten()
              .find(|a| a.key.as_ref() == b"href")
              .map(|a| {
                a.unescape_value().map(|v| v.into_owned()).unwrap_or_else(
                  |_| String::from_utf8_lossy(&a.value).into_owned(),
                )
              });
            if let (Some(c), Some(h)) = (current.as_mut(), href)
              && rel.as_deref().is_none_or(|r| r == b"alternate")
              && c.url.is_empty()
            {
              c.url = h;
            }
          }
          b"lastmod" | b"pubdate" | b"published" | b"updated" | b"date" => {
            text_target = Some("date")
          }
          _ => {}
        }
        stack.push(name);
      }
      Ok(Event::Empty(e)) => {
        let name = local(e.name().as_ref()).to_ascii_lowercase();
        let parent = stack.last().map(|p| p.as_slice()).unwrap_or(b"");
        if name == b"link" && parent == b"entry" {
          let rel = e
            .attributes()
            .flatten()
            .find(|a| a.key.as_ref() == b"rel")
            .map(|a| a.value.to_vec());
          let href =
            e.attributes()
              .flatten()
              .find(|a| a.key.as_ref() == b"href")
              .map(|a| {
                a.unescape_value().map(|v| v.into_owned()).unwrap_or_else(
                  |_| String::from_utf8_lossy(&a.value).into_owned(),
                )
              });
          if let (Some(c), Some(h)) = (current.as_mut(), href)
            && rel.as_deref().is_none_or(|r| r == b"alternate")
            && c.url.is_empty()
          {
            c.url = h;
          }
        }
      }
      Ok(Event::Text(t)) => {
        let s = t
          .unescape()
          .map(|c| c.into_owned())
          .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
        apply_text(text_target, current.as_mut(), &s);
      }
      Ok(Event::CData(t)) => {
        apply_text(text_target, current.as_mut(), &String::from_utf8_lossy(&t));
      }
      Ok(Event::End(e)) => {
        let name = local(e.name().as_ref()).to_ascii_lowercase();
        stack.pop();
        text_target = None;
        if matches!(name.as_slice(), b"url" | b"item" | b"entry" | b"sitemap")
          && let Some(c) = current.take()
          && !c.url.is_empty()
        {
          out.push(c);
        }
      }
      Ok(Event::Eof) => break,
      Err(_) => break,
      _ => {}
    }
    buf.clear();
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sitemap_and_index() {
    let idx = br#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <sitemap><loc>https://x.com/sitemap-2024.xml</loc><lastmod>2024-05-01</lastmod></sitemap></sitemapindex>"#;
    let l = extract_links(idx);
    assert_eq!(
      l,
      vec![FeedLink {
        url: "https://x.com/sitemap-2024.xml".into(),
        date: Some("2024-05-01".into()),
        nested: true
      }]
    );
    let sm = br#"<urlset><url><loc>https://x.com/a</loc><lastmod>2024-01-02T00:00:00+00:00</lastmod></url><url><loc>https://x.com/b</loc></url></urlset>"#;
    let l = extract_links(sm);
    assert_eq!(l.len(), 2);
    assert!(!l[0].nested);
    assert_eq!(l[1].url, "https://x.com/b");
    assert_eq!(l[1].date, None);
  }

  #[test]
  fn rss_and_atom() {
    let rss = br#"<rss><channel><link>https://x.com</link><item><title>t</title><link>https://x.com/post</link><pubDate>Fri, 01 Mar 2024 10:00:00 GMT</pubDate><guid>https://x.com/post?p=1</guid></item></channel></rss>"#;
    let l = extract_links(rss);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].url, "https://x.com/post");
    assert_eq!(l[0].date.as_deref(), Some("Fri, 01 Mar 2024 10:00:00 GMT"));
    let atom = br#"<feed xmlns="http://www.w3.org/2005/Atom"><link href="https://x.com/"/><entry><link rel="self" href="https://x.com/self"/><link rel="alternate" href="https://x.com/entry"/><updated>2024-02-02T00:00:00Z</updated></entry></feed>"#;
    let l = extract_links(atom);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].url, "https://x.com/entry");
    assert_eq!(l[0].date.as_deref(), Some("2024-02-02T00:00:00Z"));
    assert!(looks_like_xml("application/rss+xml", b""));
    assert!(!looks_like_xml(
      "application/xhtml+xml",
      b"<?xml version=\"1.0\"?><html>"
    ));
    assert!(!looks_like_xml(
      "text/html",
      b"<?xml version=\"1.0\"?><!DOCTYPE html><html>"
    ));
    assert!(looks_like_xml(
      "text/xml",
      b"<?xml version=\"1.0\"?>\n<!-- c --><urlset>"
    ));
    let escaped = br#"<sitemapindex><sitemap><loc>https://x.com/sm.xml?a=1&amp;b=2</loc></sitemap></sitemapindex>"#;
    assert_eq!(
      extract_links(escaped)[0].url,
      "https://x.com/sm.xml?a=1&b=2"
    );
    let atom = br#"<feed><entry><link href="https://x.com/e?a=1&amp;b=2"/></entry></feed>"#;
    assert_eq!(extract_links(atom)[0].url, "https://x.com/e?a=1&b=2");
    assert!(looks_like_xml(
      "text/plain",
      b"  <?xml version=\"1.0\"?><rss>"
    ));
    assert!(!looks_like_xml("text/html", b"<!doctype html>"));
  }
}
