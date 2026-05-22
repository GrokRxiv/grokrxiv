//! Rule-based tone verifier. MVP-grade: any artifact whose stringified
//! representation contains a word from the embedded ban-list is flagged as
//! `Warn`. The list is intentionally short and conservative; we expect to
//! swap this for a local classifier in a later milestone.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::json;

use crate::{Verifier, VerifierContext};

const WORDLIST: &str = include_str!("tone_wordlist.txt");

static BANNED_RE: Lazy<Regex> = Lazy::new(|| {
    let words: Vec<&str> = WORDLIST
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if words.is_empty() {
        // Pattern that matches nothing: a literal NUL byte between word
        // boundaries (the input is never NUL).
        return Regex::new("\u{0}").unwrap();
    }
    let escaped: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    Regex::new(&format!("(?i)\\b({})\\b", escaped.join("|"))).expect("tone wordlist regex")
});

/// Tone verifier; flags banned-word matches as `Warn`.
#[derive(Default)]
pub struct ToneVerifier;

impl ToneVerifier {
    /// Construct a tone verifier.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Verifier for ToneVerifier {
    fn name(&self) -> &'static str {
        "tone"
    }

    async fn verify(
        &self,
        artifact: &serde_json::Value,
        _ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        let text = artifact.to_string();
        let hits: Vec<String> = BANNED_RE
            .find_iter(&text)
            .map(|m| m.as_str().to_lowercase())
            .collect();
        if hits.is_empty() {
            VerifierResult {
                status: VerifierStatus::Pass,
                notes: json!({}),
            }
        } else {
            VerifierResult {
                status: VerifierStatus::Warn,
                notes: json!({ "matches": hits }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::PaperExtract;

    fn ctx_paper() -> PaperExtract {
        PaperExtract {
            arxiv_id: "x".into(),
            title: "t".into(),
            authors: vec![],
            abstract_: "a".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    #[tokio::test]
    async fn neutral_text_passes() {
        let v = ToneVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v
            .verify(
                &json!({ "summary": "Clear, well-structured argument." }),
                &ctx,
            )
            .await;
        assert!(matches!(r.status, VerifierStatus::Pass));
    }

    #[tokio::test]
    async fn flags_banned_word() {
        let v = ToneVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext::for_paper(&paper, &http);
        let r = v
            .verify(
                &json!({ "summary": "The author is stupid and the work is garbage." }),
                &ctx,
            )
            .await;
        assert!(matches!(r.status, VerifierStatus::Warn));
        let matches = r.notes["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
    }
}
