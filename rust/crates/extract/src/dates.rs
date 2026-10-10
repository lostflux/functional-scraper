//! Publication-date extraction helpers.

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

pub fn current_year() -> i32 {
  chrono::Utc::now().year()
}

fn plausible(d: NaiveDate) -> bool {
  (1995..=current_year() + 1).contains(&d.year())
}

static ISO: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^\s*(\d{4})-(\d{2})-(\d{2})").unwrap());
static MDY: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"(?i)\b(jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s+(\d{1,2})(?:st|nd|rd|th)?,?\s+(\d{4})\b").unwrap()
});
static DMY: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"(?i)\b(\d{1,2})\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?,?\s+(\d{4})\b").unwrap()
});
static URL_DATE: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(r"/((?:19|20)\d{2})/(\d{1,2})(?:/(\d{1,2}))?(?:/|$)").unwrap()
});
static YEAR_TOKEN: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"\b(20\d{2}|19\d{2})\b").unwrap());

fn month_num(name: &str) -> Option<u32> {
  let m = name.to_ascii_lowercase();
  let idx = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct",
    "nov", "dec",
  ]
  .iter()
  .position(|p| m.starts_with(p))?;
  Some(idx as u32 + 1)
}

/// Best-effort parse of a date string as found in meta tags, JSON-LD,
/// `<time datetime>`, RSS `pubDate` or visible bylines.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
  let s = s.trim();
  if s.is_empty() {
    return None;
  }
  if let Some(c) = ISO.captures(s) {
    let d = NaiveDate::from_ymd_opt(
      c[1].parse().ok()?,
      c[2].parse().ok()?,
      c[3].parse().ok()?,
    )?;
    return plausible(d).then_some(d);
  }
  if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(s) {
    let d = dt.date_naive();
    return plausible(d).then_some(d);
  }
  for fmt in ["%Y/%m/%d", "%m/%d/%Y", "%Y%m%d"] {
    if let Ok(d) = NaiveDate::parse_from_str(s, fmt)
      && plausible(d)
    {
      return Some(d);
    }
  }
  if let Some(c) = MDY.captures(s) {
    let d = NaiveDate::from_ymd_opt(
      c[3].parse().ok()?,
      month_num(&c[1])?,
      c[2].parse().ok()?,
    )?;
    return plausible(d).then_some(d);
  }
  if let Some(c) = DMY.captures(s) {
    let d = NaiveDate::from_ymd_opt(
      c[3].parse().ok()?,
      month_num(&c[2])?,
      c[1].parse().ok()?,
    )?;
    return plausible(d).then_some(d);
  }
  None
}

/// `/2024/03/01/slug` → (2024, Some(2024-03-01)); `/2024/03/slug` → (2024, None).
pub fn date_from_url_path(path: &str) -> Option<(i32, Option<NaiveDate>)> {
  let c = URL_DATE.captures(path)?;
  let year: i32 = c[1].parse().ok()?;
  if !(1995..=current_year() + 1).contains(&year) {
    return None;
  }
  let month: u32 = c[2].parse().ok()?;
  if !(1..=12).contains(&month) {
    return None;
  }
  let day = c
    .get(3)
    .and_then(|d| d.as_str().parse::<u32>().ok())
    .filter(|d| (1..=31).contains(d));
  let full = day.and_then(|d| NaiveDate::from_ymd_opt(year, month, d));
  Some((year, full))
}

static NAME_YM: LazyLock<Regex> = LazyLock::new(|| {
  Regex::new(
    r"(?:^|[^0-9])((?:19|20)\d{2})(?:[-_/.]?(0[1-9]|1[0-2]))?(?:[^0-9]|$)",
  )
  .unwrap()
});

/// Year and optional month embedded in a sitemap or archive name, e.g.
/// `sitemap-2024-03.xml` → (2024, Some(3)), `/entries/2026/10` → (2026, Some(10)).
pub fn year_month_in_name(s: &str) -> Option<(i32, Option<u32>)> {
  let c = NAME_YM.captures(s)?;
  let year: i32 = c[1].parse().ok()?;
  if !(1995..=current_year() + 1).contains(&year) {
    return None;
  }
  // `sitemap-posts-2001-3000.xml` is a numeric range, not a year.
  let after = &s[c.get(1).unwrap().end()..];
  if c.get(2).is_none()
    && after.starts_with(['-', '_'])
    && after[1..].chars().take(3).all(|ch| ch.is_ascii_digit())
    && after[1..].chars().count() >= 3
  {
    return None;
  }
  let month = c.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
  Some((year, month))
}

/// True if a sitemap named like `…2021-03…` is entirely before `min`.
pub fn name_is_before(name: &str, min: NaiveDate) -> bool {
  match year_month_in_name(name) {
    Some((y, Some(m))) => (y, m) < (min.year(), min.month()),
    Some((y, None)) => y < min.year(),
    None => false,
  }
}

/// Legacy heuristic: the largest standalone year token within [2000, now].
pub fn max_year_in_text(text: &str) -> Option<i32> {
  let max = current_year();
  YEAR_TOKEN
    .find_iter(text)
    .filter_map(|m| m.as_str().parse::<i32>().ok())
    .filter(|y| (2000..=max).contains(y))
    .max()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_common_formats() {
    let d = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
    assert_eq!(parse_date("2024-03-01T10:00:00Z"), Some(d));
    assert_eq!(parse_date("2024-03-01"), Some(d));
    assert_eq!(parse_date("Fri, 01 Mar 2024 10:00:00 GMT"), Some(d));
    assert_eq!(parse_date("March 1, 2024"), Some(d));
    assert_eq!(parse_date("Mar. 1st, 2024 at 9:00 am"), Some(d));
    assert_eq!(parse_date("1 March 2024"), Some(d));
    assert_eq!(parse_date("Published 1 Mar 2024"), Some(d));
    assert_eq!(parse_date("2024/03/01"), Some(d));
    assert_eq!(parse_date("garbage"), None);
    assert_eq!(parse_date("1066-01-01"), None);
  }

  #[test]
  fn url_dates() {
    assert_eq!(
      date_from_url_path("/2021/02/24/amazon-launches"),
      Some((2021, NaiveDate::from_ymd_opt(2021, 2, 24)))
    );
    assert_eq!(date_from_url_path("/2021/02/amazon"), Some((2021, None)));
    assert_eq!(date_from_url_path("/s/545106/human-animal"), None);
    assert_eq!(date_from_url_path("/2021/13/01/x"), None);
  }

  #[test]
  fn sitemap_names() {
    assert_eq!(
      year_month_in_name("https://x.com/sitemap-2024-03.xml"),
      Some((2024, Some(3)))
    );
    assert_eq!(
      year_month_in_name("https://x.com/sitemaps/entries/2026/10"),
      Some((2026, Some(10)))
    );
    assert_eq!(year_month_in_name("https://x.com/post-sitemap12.xml"), None);
    assert_eq!(
      year_month_in_name("https://x.com/sitemap-page-2019.xml"),
      Some((2019, None))
    );
    assert_eq!(
      year_month_in_name("https://x.com/sitemap-posts-2001-3000.xml"),
      None
    );
    let min = NaiveDate::from_ymd_opt(2023, 6, 1).unwrap();
    assert!(name_is_before("https://x.com/sitemap-2023-05.xml", min));
    assert!(!name_is_before("https://x.com/sitemap-2023-06.xml", min));
    assert!(name_is_before("https://x.com/sitemap-2019.xml", min));
    assert!(!name_is_before("https://x.com/post-sitemap12.xml", min));
  }

  #[test]
  fn year_fallback() {
    assert_eq!(
      max_year_in_text("founded in 2015, acquired 2019. © 1999"),
      Some(2019)
    );
    assert_eq!(max_year_in_text("no years here, 12345"), None);
  }
}
