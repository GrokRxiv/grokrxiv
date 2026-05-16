//! arXiv metadata fetcher.
//!
//! Talks to `http://export.arxiv.org/api/query?id_list=<id>`, parses the Atom XML
//! response with `quick-xml`, and returns a normalised [`ArxivMeta`].

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

use crate::download::rate_limited_get;
use crate::types::Author;

// arXiv started returning 301 redirects to HTTPS in May 2026; the HTTP variant
// no longer resolves cleanly through Varnish. Use HTTPS directly.
const ARXIV_API: &str = "https://export.arxiv.org/api/query?id_list=";
/// The HTML "abs" page is on a separate Fastly/Cloudflare pool from the
/// `export.arxiv.org` API; we use it as the primary metadata source because
/// the API is aggressively rate-limited under any non-trivial load.
const ARXIV_ABS: &str = "https://arxiv.org/abs/";

/// Metadata pulled from the arXiv Atom feed.
///
/// This is a strictly larger structure than `PaperExtract`: it carries
/// `pdf_url` / `source_url` / `categories` that the pipeline uses internally
/// but that aren't part of the persisted artifact schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArxivMeta {
    /// arXiv id as supplied by the caller.
    pub arxiv_id: String,
    /// Paper title.
    pub title: String,
    /// Author list with affiliations where available.
    pub authors: Vec<Author>,
    /// Abstract text.
    pub abstract_text: String,
    /// All categories returned by arXiv (in document order).
    pub categories: Vec<String>,
    /// Direct PDF URL.
    pub pdf_url: Option<String>,
    /// HTML "abs" page URL on arXiv.org.
    pub source_url: Option<String>,
    /// First submission date when available from Atom/OAI metadata.
    pub submitted_date: Option<NaiveDate>,
}

impl ArxivMeta {
    /// Primary category (first listed).
    pub fn primary_category(&self) -> Option<String> {
        self.categories.first().cloned()
    }
}

/// Fetch metadata for the supplied arXiv id (e.g. `2401.12345` or `2401.12345v2`).
///
/// **Source preference**: the HTML "abs" page is tried first because
/// `export.arxiv.org/api/query` is aggressively rate-limited (returns
/// HTTP 429 "Rate exceeded" after a handful of requests per IP). The abs
/// page is on a separate Fastly pool and returns the same metadata in
/// `<meta name="citation_*">` tags. The API stays as a fallback so
/// existing fixtures + the parse_atom() public function keep working.
pub async fn fetch_metadata(arxiv_id: &str) -> Result<ArxivMeta> {
    // Try the HTML abs page first.
    match fetch_metadata_from_abs(arxiv_id).await {
        Ok(m) => return Ok(m),
        Err(e) => tracing::info!(arxiv_id, err = %e, "abs metadata failed; falling back to API"),
    }
    // Fallback: the legacy API path.
    let url = format!("{ARXIV_API}{arxiv_id}");
    let body = rate_limited_get(&url)
        .await
        .with_context(|| format!("fetch arxiv metadata for {arxiv_id}"))?;
    parse_atom(
        arxiv_id,
        std::str::from_utf8(&body).context("arxiv body utf8")?,
    )
}

/// Fetch the `arxiv.org/abs/<id>` page and parse the embedded citation_*
/// meta tags into an [`ArxivMeta`].
pub async fn fetch_metadata_from_abs(arxiv_id: &str) -> Result<ArxivMeta> {
    let url = format!("{ARXIV_ABS}{arxiv_id}");
    let body = rate_limited_get(&url)
        .await
        .with_context(|| format!("fetch arxiv abs page for {arxiv_id}"))?;
    let html = std::str::from_utf8(&body).context("abs page utf8")?;
    parse_abs_html(arxiv_id, html)
}

/// Parse the citation_* meta tags + primary-subject span out of the abs
/// page HTML into an [`ArxivMeta`]. Exposed for unit tests with captured
/// fixtures.
pub fn parse_abs_html(arxiv_id: &str, html: &str) -> Result<ArxivMeta> {
    let title = scrape_meta(html, "citation_title").unwrap_or_default();
    let abstract_text = scrape_meta(html, "citation_abstract").unwrap_or_default();
    let pdf_url = scrape_meta(html, "citation_pdf_url");
    let date_str = scrape_meta(html, "citation_date").or_else(|| scrape_meta(html, "citation_online_date"));
    let submitted_date = date_str.as_deref().and_then(|d| {
        // Format is "YYYY/MM/DD".
        NaiveDate::parse_from_str(d, "%Y/%m/%d").ok()
    });
    let authors: Vec<Author> = scrape_meta_all(html, "citation_author")
        .into_iter()
        .map(|name| Author {
            name: normalize_author_name(&name),
            affiliation: None,
            email: None,
        })
        .collect();
    // Primary subject lives inside <span class="primary-subject">Mathematical Physics (math-ph)</span>
    let category = scrape_primary_subject(html);
    let categories: Vec<String> = category.map(|c| vec![c]).unwrap_or_default();
    let source_url = Some(format!("https://arxiv.org/abs/{arxiv_id}"));

    if title.is_empty() {
        return Err(anyhow!("abs page: no citation_title meta found for {arxiv_id}"));
    }

    Ok(ArxivMeta {
        arxiv_id: arxiv_id.to_string(),
        title: compact_whitespace(&decode_html_entities(&title)),
        authors,
        abstract_text: compact_whitespace(&decode_html_entities(&abstract_text)),
        categories,
        pdf_url,
        source_url,
        submitted_date,
    })
}

fn scrape_meta(html: &str, name: &str) -> Option<String> {
    // Match either <meta name="X" content="Y"> or <meta content="Y" name="X">.
    let pat_a = format!(r#"<meta\s+name="{name}"\s+content="([^"]*)""#);
    let pat_b = format!(r#"<meta\s+content="([^"]*)"\s+name="{name}""#);
    if let Some(c) = regex_find(&pat_a, html) {
        return Some(c);
    }
    regex_find(&pat_b, html)
}

fn scrape_meta_all(html: &str, name: &str) -> Vec<String> {
    let pat = format!(r#"<meta\s+name="{name}"\s+content="([^"]*)""#);
    let re = match regex::Regex::new(&pat) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    re.captures_iter(html)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

fn scrape_primary_subject(html: &str) -> Option<String> {
    // <span class="primary-subject">Mathematical Physics (math-ph)</span>
    let re = regex::Regex::new(
        r#"<span class="primary-subject">[^<]*\(([^)]+)\)\s*</span>"#,
    )
    .ok()?;
    re.captures(html)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn regex_find(pattern: &str, hay: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    re.captures(hay)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// "Doe, Jane" → "Jane Doe". Many arXiv abs pages use last-first comma order.
fn normalize_author_name(raw: &str) -> String {
    let s = raw.trim();
    if let Some((last, first)) = s.split_once(',') {
        let last = last.trim();
        let first = first.trim();
        if !first.is_empty() && !last.is_empty() {
            return format!("{first} {last}");
        }
    }
    s.to_string()
}

/// Parse an arXiv Atom feed (used by tests with a captured fixture).
pub fn parse_atom(arxiv_id: &str, xml: &str) -> Result<ArxivMeta> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut meta = ArxivMeta {
        arxiv_id: arxiv_id.to_string(),
        ..ArxivMeta::default()
    };

    let mut in_entry = false;
    let mut path: Vec<String> = Vec::new();
    let mut current_author: Option<Author> = None;

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "entry" {
                    in_entry = true;
                }
                if in_entry && name == "author" {
                    current_author = Some(Author {
                        name: String::new(),
                        affiliation: None,
                        email: None,
                    });
                }
                path.push(name);
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if in_entry && name == "author" {
                    if let Some(a) = current_author.take() {
                        if !a.name.is_empty() {
                            meta.authors.push(a);
                        }
                    }
                }
                if name == "entry" {
                    in_entry = false;
                }
                path.pop();
            }
            Ok(Event::Empty(e)) => {
                if !in_entry {
                    continue;
                }
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let attrs: Vec<(String, String)> = e
                    .attributes()
                    .filter_map(|a| a.ok())
                    .map(|a| {
                        (
                            String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                            String::from_utf8_lossy(&a.value).into_owned(),
                        )
                    })
                    .collect();
                let attr = |k: &str| attrs.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
                match name.as_str() {
                    "link" => {
                        let title = attr("title").unwrap_or_default();
                        let rel = attr("rel").unwrap_or_default();
                        let href = attr("href").unwrap_or_default();
                        if title == "pdf" {
                            meta.pdf_url = Some(href);
                        } else if rel == "alternate" && meta.source_url.is_none() {
                            meta.source_url = Some(href);
                        }
                    }
                    "category" => {
                        if let Some(term) = attr("term") {
                            if !meta.categories.contains(&term) {
                                meta.categories.push(term);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if !in_entry {
                    continue;
                }
                let text = t.unescape().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let Some(full_tag) = path.last().cloned() else {
                    continue;
                };
                // Strip any XML namespace prefix (e.g. `arxiv:affiliation` → `affiliation`).
                let tag = full_tag
                    .rsplit(':')
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or(full_tag);
                let in_author = path.iter().any(|p| p.rsplit(':').next() == Some("author"));
                match tag.as_str() {
                    "title" if !in_author => {
                        push_text(&mut meta.title, &text);
                    }
                    "summary" => {
                        push_text(&mut meta.abstract_text, &text);
                    }
                    "published" => {
                        meta.submitted_date = parse_date_prefix(&text);
                    }
                    "name" if in_author => {
                        if let Some(a) = current_author.as_mut() {
                            push_text(&mut a.name, &text);
                        }
                    }
                    "affiliation" if in_author => {
                        if let Some(a) = current_author.as_mut() {
                            let mut s = a.affiliation.clone().unwrap_or_default();
                            push_text(&mut s, &text);
                            a.affiliation = Some(s);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(anyhow!("xml parse error: {e}")),
        }
        buf.clear();
    }

    meta.title = compact_whitespace(&meta.title);
    meta.abstract_text = compact_whitespace(&meta.abstract_text);
    for a in &mut meta.authors {
        a.name = compact_whitespace(&a.name);
    }
    Ok(meta)
}

fn push_text(dst: &mut String, text: &str) {
    if !dst.is_empty() && !dst.ends_with(' ') {
        dst.push(' ');
    }
    dst.push_str(text);
}

fn compact_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_ws && !out.is_empty() {
                out.push(' ');
            }
            last_ws = true;
        } else {
            out.push(ch);
            last_ws = false;
        }
    }
    out.trim().to_string()
}

fn parse_date_prefix(text: &str) -> Option<NaiveDate> {
    text.split('T')
        .next()
        .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
}
