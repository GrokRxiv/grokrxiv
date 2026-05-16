//! arXiv OAI-PMH bulk listing.
//!
//! Used by the scheduler in the orchestrator to discover new papers each
//! day. We never publish or expose the resulting metadata as a public
//! PDF/source URL; everything is for internal pipeline routing only.
//!
//! Endpoint: `https://export.arxiv.org/oai2?verb=ListRecords&metadataPrefix=arXiv&set=<group>&from=YYYY-MM-DD&until=YYYY-MM-DD`.
//! Results may include a `<resumptionToken>` element; we follow it until
//! empty. Every request goes through the same 3s-spacing semaphore used by
//! [`crate::download::rate_limited_get`] / [`crate::arxiv::fetch_metadata`].

use std::collections::HashSet;

use anyhow::Context;
use chrono::NaiveDate;
use quick_xml::events::Event;
use quick_xml::Reader;
use thiserror::Error;
use tracing::warn;

use crate::arxiv::ArxivMeta;
use crate::download::rate_limited_get;
use crate::types::Author;

const OAI_BASE: &str = "https://export.arxiv.org/oai2";

/// Every arXiv top-level group the GrokRxiv pipeline knows how to ingest.
/// The `INGEST_CATEGORIES` env can pick any subset of this list. We refuse
/// to ingest groups outside the allow-list.
pub const ALL_CATEGORIES: &[&str] = &[
    // Computer Science
    "cs",   // Mathematics
    "math", // Physics (arXiv splits this across multiple OAI sets)
    "physics", "astro-ph", "cond-mat", "gr-qc", "hep-ex", "hep-lat", "hep-ph", "hep-th", "nucl-ex",
    "nucl-th", "quant-ph", "nlin",  // Quantitative Biology
    "q-bio", // Quantitative Finance
    "q-fin", // Statistics
    "stat",  // Electrical Engineering and Systems Science
    "eess",  // Economics
    "econ",
];

/// Active-by-default for the MVP pipeline test: CS + Math + Physics
/// (the three you'd pick in the arXiv subject dropdown). Other groups in
/// [`ALL_CATEGORIES`] are activated via the `INGEST_CATEGORIES` env var.
pub const DEFAULT_ACTIVE_CATEGORIES: &[&str] = &[
    "cs", "math", "physics", "astro-ph", "cond-mat", "gr-qc", "hep-ex", "hep-lat", "hep-ph",
    "hep-th", "nucl-ex", "nucl-th", "quant-ph", "nlin",
];

/// Errors specific to the ingest crate's public API.
#[derive(Debug, Error)]
pub enum IngestError {
    /// The supplied category is not in [`ALL_CATEGORIES`].
    #[error("unknown arXiv category: {0}")]
    UnknownCategory(String),
    /// Network failure.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    /// Generic failure (XML parse, etc).
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Fetch a deduplicated [`ArxivMeta`] list across one or more categories for
/// the date range `from..=until`. Requests are issued serially through the
/// shared arXiv rate-limit gate.
pub async fn fetch_listing(
    categories: &[&str],
    from: NaiveDate,
    until: NaiveDate,
    user_agent: &str,
) -> Result<Vec<ArxivMeta>, IngestError> {
    for c in categories {
        if !ALL_CATEGORIES.contains(c) {
            return Err(IngestError::UnknownCategory((*c).to_string()));
        }
    }
    // Honour caller-supplied UA by writing it into the env that
    // `rate_limited_get` reads. (We don't mutate the global semaphore.)
    if !user_agent.is_empty() {
        std::env::set_var("ARXIV_USER_AGENT", user_agent);
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<ArxivMeta> = Vec::new();

    for cat in categories {
        let mut url = format!(
            "{OAI_BASE}?verb=ListRecords&metadataPrefix=arXiv&set={cat}&from={from}&until={until}"
        );
        loop {
            let bytes = match rate_limited_get(&url).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(set = cat, error = %e, "OAI request failed");
                    break;
                }
            };
            let xml = std::str::from_utf8(&bytes).context("OAI body utf8")?;
            let (records, token) = parse_oai(xml)?;
            for r in records {
                if seen.insert(r.arxiv_id.clone()) {
                    out.push(r);
                }
            }
            let Some(tok) = token else { break };
            if tok.is_empty() {
                break;
            }
            url = format!("{OAI_BASE}?verb=ListRecords&resumptionToken={tok}");
        }
    }
    Ok(out)
}

/// Parse an OAI-PMH response. Returns `(records, resumption_token)`.
pub(crate) fn parse_oai(xml: &str) -> Result<(Vec<ArxivMeta>, Option<String>), anyhow::Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut records: Vec<ArxivMeta> = Vec::new();
    let mut token: Option<String> = None;

    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();
    let mut current: Option<ArxivMeta> = None;
    let mut current_author: Option<Author> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let local = local_name(&name);
                if local == "arXiv" {
                    current = Some(ArxivMeta::default());
                } else if local == "author" && current.is_some() {
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
                let local = local_name(&name);
                if local == "arXiv" {
                    if let Some(meta) = current.take() {
                        if !meta.arxiv_id.is_empty() {
                            records.push(meta);
                        }
                    }
                } else if local == "author" {
                    if let (Some(a), Some(meta)) = (current_author.take(), current.as_mut()) {
                        if !a.name.is_empty() {
                            meta.authors.push(a);
                        }
                    }
                }
                path.pop();
            }
            Ok(Event::Text(t)) => {
                let text = t.unescape().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                let Some(tag) = path.last().cloned() else {
                    continue;
                };
                let local_tag = local_name(&tag);
                if local_tag == "resumptionToken" {
                    token = Some(text);
                    continue;
                }
                let Some(meta) = current.as_mut() else {
                    continue;
                };
                let in_author = path.iter().any(|p| local_name(p) == "author");
                match local_tag.as_str() {
                    "id" if !in_author => {
                        if meta.arxiv_id.is_empty() {
                            meta.arxiv_id = text.clone();
                            meta.pdf_url = Some(format!("https://arxiv.org/pdf/{text}.pdf"));
                            meta.source_url = Some(format!("https://arxiv.org/abs/{text}"));
                        }
                    }
                    "title" => meta.title = collapse_ws(&format!("{} {}", meta.title, text)),
                    "abstract" => {
                        meta.abstract_text =
                            collapse_ws(&format!("{} {}", meta.abstract_text, text))
                    }
                    "created" => {
                        meta.submitted_date = NaiveDate::parse_from_str(&text, "%Y-%m-%d").ok();
                    }
                    "categories" => {
                        for c in text.split_whitespace() {
                            if !meta.categories.iter().any(|x| x == c) {
                                meta.categories.push(c.to_string());
                            }
                        }
                    }
                    "keyname" if in_author => {
                        if let Some(a) = current_author.as_mut() {
                            a.name = if a.name.is_empty() {
                                text
                            } else {
                                format!("{}, {}", text, a.name)
                            };
                        }
                    }
                    "forenames" if in_author => {
                        if let Some(a) = current_author.as_mut() {
                            a.name = if a.name.is_empty() {
                                text
                            } else {
                                format!("{} {}", a.name, text)
                            };
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(anyhow::anyhow!("xml parse error: {e}")),
        }
        buf.clear();
    }

    Ok((records, token))
}

fn local_name(s: &str) -> String {
    s.rsplit(':').next().unwrap_or(s).to_string()
}

fn collapse_ws(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/">
  <ListRecords>
    <record>
      <header>
        <identifier>oai:arXiv.org:2601.00001</identifier>
        <datestamp>2026-05-01</datestamp>
      </header>
      <metadata>
        <arXiv xmlns="http://arxiv.org/OAI/arXiv/">
          <id>2601.00001</id>
          <created>2026-05-01</created>
          <title>A new modular result</title>
          <authors>
            <author>
              <keyname>Researcher</keyname>
              <forenames>Alice</forenames>
            </author>
            <author>
              <keyname>Scientist</keyname>
              <forenames>Bob</forenames>
            </author>
          </authors>
          <abstract>We show a new modular result.</abstract>
          <categories>cs.LG stat.ML</categories>
        </arXiv>
      </metadata>
    </record>
  </ListRecords>
</OAI-PMH>"#;

    #[test]
    fn parses_single_record() {
        let (records, token) = parse_oai(FIXTURE).expect("parse");
        assert!(token.is_none());
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.arxiv_id, "2601.00001");
        assert_eq!(r.title, "A new modular result");
        assert_eq!(r.categories, vec!["cs.LG", "stat.ML"]);
        assert_eq!(r.authors.len(), 2);
        assert_eq!(
            r.pdf_url.as_deref(),
            Some("https://arxiv.org/pdf/2601.00001.pdf")
        );
    }

    #[tokio::test]
    async fn fetch_listing_rejects_unknown_category() {
        let err = fetch_listing(
            &["definitely-not-a-cat"],
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
            "test/1.0",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IngestError::UnknownCategory(_)));
    }
}
