//! Render-output verifier.
//!
//! The artifact is expected to carry an `html` key (string) and/or a `tex` key
//! (string). For HTML we parse with `lol_html` and ensure:
//!
//! * an `<h1>` element exists,
//! * no `<script>` tags exist outside the KaTeX-allowed range (we currently
//!   permit no `<script>` tags at all; KaTeX rendering happens client-side
//!   on the Next.js app, not in the artifact itself).
//!
//! For LaTeX we count `\begin{}` and `\end{}` and fail on imbalance.

use async_trait::async_trait;
use grokrxiv_schemas::{VerifierResult, VerifierStatus};
use lol_html::{element, HtmlRewriter, Settings};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use crate::{Verifier, VerifierContext};

/// Render verifier.
#[derive(Default)]
pub struct RenderVerifier;

impl RenderVerifier {
    /// Construct a render verifier.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Verifier for RenderVerifier {
    fn name(&self) -> &'static str {
        "render"
    }

    async fn verify(
        &self,
        artifact: &serde_json::Value,
        _ctx: &VerifierContext<'_>,
    ) -> VerifierResult {
        let mut notes = serde_json::Map::new();
        let mut overall_ok = true;

        if let Some(html) = artifact.get("html").and_then(|v| v.as_str()) {
            let (ok, h1_count, script_count) = inspect_html(html);
            notes.insert("html_h1_count".into(), json!(h1_count));
            notes.insert("html_script_count".into(), json!(script_count));
            if !ok {
                overall_ok = false;
                notes.insert(
                    "html_error".into(),
                    json!("missing <h1> or contains <script>"),
                );
            }
        }

        if let Some(tex) = artifact.get("tex").and_then(|v| v.as_str()) {
            let begins = tex.matches("\\begin{").count();
            let ends = tex.matches("\\end{").count();
            notes.insert("tex_begin_count".into(), json!(begins));
            notes.insert("tex_end_count".into(), json!(ends));
            if begins != ends {
                overall_ok = false;
                notes.insert(
                    "tex_error".into(),
                    json!(format!("unbalanced begin/end: {begins} vs {ends}")),
                );
            }
        }

        let status = if overall_ok {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        };
        VerifierResult {
            status,
            notes: serde_json::Value::Object(notes),
        }
    }
}

/// Inspect an HTML string. Returns `(ok, h1_count, script_count)`.
fn inspect_html(html: &str) -> (bool, usize, usize) {
    let h1_count = Arc::new(AtomicUsize::new(0));
    let script_count = Arc::new(AtomicUsize::new(0));
    let has_h1 = Arc::new(AtomicBool::new(false));
    let h1c = h1_count.clone();
    let sc = script_count.clone();
    let hh1 = has_h1.clone();

    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![
                element!("h1", move |_el| {
                    h1c.fetch_add(1, Ordering::Relaxed);
                    hh1.store(true, Ordering::Relaxed);
                    Ok(())
                }),
                element!("script", move |_el| {
                    sc.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }),
            ],
            ..Settings::default()
        },
        |_: &[u8]| {},
    );
    let _ = rewriter.write(html.as_bytes());
    let _ = rewriter.end();
    let ok = has_h1.load(Ordering::Relaxed) && script_count.load(Ordering::Relaxed) == 0;
    (
        ok,
        h1_count.load(Ordering::Relaxed),
        script_count.load(Ordering::Relaxed),
    )
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
        }
    }

    #[tokio::test]
    async fn valid_html_and_tex_pass() {
        let v = RenderVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let html = "<!doctype html><html><body><h1>Hi</h1><p>x</p></body></html>";
        let tex = "\\begin{document}\\section{X}\\end{document}";
        let r = v.verify(&json!({ "html": html, "tex": tex }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Pass), "{:?}", r.notes);
    }

    #[tokio::test]
    async fn missing_h1_fails() {
        let v = RenderVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let html = "<!doctype html><html><body><p>no heading</p></body></html>";
        let r = v.verify(&json!({ "html": html }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Fail));
    }

    #[tokio::test]
    async fn script_tag_fails() {
        let v = RenderVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let html = "<!doctype html><html><body><h1>Ok</h1><script>alert(1)</script></body></html>";
        let r = v.verify(&json!({ "html": html }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Fail));
    }

    #[tokio::test]
    async fn unbalanced_latex_fails() {
        let v = RenderVerifier::new();
        let paper = ctx_paper();
        let http = reqwest::Client::new();
        let ctx = VerifierContext {
            paper: &paper,
            http: &http,
        };
        let tex = "\\begin{document}\\begin{itemize}\\end{document}"; // 2 begins, 1 end
        let r = v.verify(&json!({ "tex": tex }), &ctx).await;
        assert!(matches!(r.status, VerifierStatus::Fail));
    }
}
