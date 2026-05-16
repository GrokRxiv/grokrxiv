//! `POST /preview` — landing-page sample-review pipeline.
//!
//! Sample reviews are NEVER published. They exist only to demo the pipeline to
//! a paper author. This route:
//!
//! 1. Accepts a `multipart/form-data` upload with a single `file` part.
//! 2. Extracts text from the PDF via `grokrxiv-ingest::extract::pdf_to_text`.
//! 3. Heuristic-builds a [`PaperExtract`] from the text (title from the first
//!    non-empty line; abstract from the next paragraph; sections via
//!    `split_sections`; bibliography via `extract_bibliography`).
//! 4. Calls the configured Anthropic model (`PREVIEW_MODEL`, default
//!    `claude-opus-4-7`) with a structured-output request shaped like a
//!    [`MetaReview`].
//! 5. Renders an HTML + Markdown + LaTeX + zip bundle via `grokrxiv-render`.
//! 6. Persists the result to the `uploads` table only (not `reviews` /
//!    `papers`) when a DB pool is configured.
//! 7. Returns `{ is_sample: true, sample_review_id, html, bundle_b64,
//!    meta_review }`.

use axum::body::Bytes;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use grokrxiv_llm_adapter::{ChatRequest, ContentPart, Message, ResponseFormat, Role};
use grokrxiv_schemas::{Author, FigureRef, MetaReview, PaperExtract};
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

/// Response payload for `/preview`.
#[derive(Debug, serde::Serialize)]
pub struct PreviewResponse {
    /// Always `true` — sample reviews are never publication-grade.
    pub is_sample: bool,
    /// Stable id stored on the `uploads` row.
    pub sample_review_id: Uuid,
    /// Rendered HTML preview (self-contained, safe for `<iframe srcDoc>`).
    pub html: String,
    /// Base64-encoded zip bundle (HTML+MD+TeX+metadata).
    pub bundle_b64: String,
    /// Structured meta-review returned by the LLM.
    pub meta_review: MetaReview,
}

/// Handle a `/preview` upload.
pub async fn preview(State(state): State<AppState>, mut multipart: Multipart) -> impl IntoResponse {
    let mut pdf_bytes: Option<Bytes> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            match field.bytes().await {
                Ok(b) => {
                    pdf_bytes = Some(b);
                    break;
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("multipart read: {e}") })),
                    )
                        .into_response();
                }
            }
        }
    }
    let Some(pdf) = pdf_bytes else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing `file` form field" })),
        )
            .into_response();
    };

    // Magic-bytes guard: only accept real PDFs. Stops payloads that just
    // happen to have a `.pdf` extension or `Content-Type: application/pdf`.
    if pdf.len() < 5 || &pdf[..5] != b"%PDF-" {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(json!({
                "error": "not a PDF",
                "hint": "Only standard PDF files are accepted (the first bytes of the file must be %PDF-).",
            })),
        )
            .into_response();
    }

    let paper = match extract_paper(&pdf).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(err = %e, "pdf extract failed");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": format!("pdf extract: {e}"),
                    "hint": "Only standard text-based PDFs are supported (not scanned images).",
                })),
            )
                .into_response();
        }
    };

    // Low-text fallback: if extraction yielded essentially nothing, the PDF
    // is image-only / scanned / malformed. We don't run OCR in the preview
    // path; return a clear error rather than burning an LLM call on noise.
    let approximate_text_len = paper.abstract_.len()
        + paper
            .sections
            .iter()
            .map(|s| s.body_markdown.len())
            .sum::<usize>();
    if approximate_text_len < 200 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "PDF contains too little extractable text",
                "hint": "PDF appears empty or image-only; OCR is not supported in the preview path. Try a text-based PDF.",
            })),
        )
            .into_response();
    }

    let meta = match run_meta_review(&state, &paper).await {
        Ok(m) => m,
        Err(e) => {
            let msg = e.to_string();
            tracing::error!(err = %msg, "meta review failed");
            // Classify the failure so the dropzone can render an actionable hint:
            //   - no LLM provider configured → 503 + "set ANTHROPIC_API_KEY"
            //   - upstream rate-limited (HTTP 429) → 503 + retry hint
            //   - upstream timeout → 504
            //   - anything else → 502
            let no_provider = msg.contains("no LLM provider");
            let rate_limited = msg.contains("rate limited") || msg.contains("429");
            let timeout = msg.contains("timeout") || msg.contains("Timeout");

            let (status, hint): (StatusCode, Option<&str>) = if no_provider {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(
                        "Set ANTHROPIC_API_KEY in the orchestrator's environment \
                         and restart (e.g. `just orch` or `docker compose up orchestrator`).",
                    ),
                )
            } else if rate_limited {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some("Upstream LLM is rate-limited. Try again in about a minute."),
                )
            } else if timeout {
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    Some("Upstream LLM call timed out. The paper may be very long; try a shorter PDF or retry."),
                )
            } else {
                (StatusCode::BAD_GATEWAY, None)
            };
            return (
                status,
                Json(json!({
                    "error": format!("llm: {msg}"),
                    "hint": hint,
                })),
            )
                .into_response();
        }
    };

    let bundle = match render_bundle(&meta, &paper).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(err = %e, "render failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("render: {e}") })),
            )
                .into_response();
        }
    };

    let bundle_b64 = base64::engine::general_purpose::STANDARD.encode(&bundle.zip);

    let sample_id = match state.db.as_ref() {
        Some(pool) => crate::db::insert_sample_upload(
            pool,
            None,
            serde_json::to_value(&meta).unwrap_or(json!({})),
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(err = %e, "upload row insert failed; returning ephemeral id");
            Uuid::new_v4()
        }),
        None => Uuid::new_v4(),
    };

    let body = PreviewResponse {
        is_sample: true,
        sample_review_id: sample_id,
        html: bundle.html,
        bundle_b64,
        meta_review: meta,
    };

    (StatusCode::OK, Json(body)).into_response()
}

// ---------------------------------------------------------------------------
// PDF -> PaperExtract
// ---------------------------------------------------------------------------

#[cfg(feature = "grokrxiv-ingest")]
async fn extract_paper(pdf: &[u8]) -> anyhow::Result<PaperExtract> {
    // `pdf_extract` is CPU-bound; run on the blocking pool so we don't block
    // the tokio runtime.
    let bytes = pdf.to_vec();
    let text = tokio::task::spawn_blocking(move || grokrxiv_ingest::extract::pdf_to_text(&bytes))
        .await
        .map_err(|e| anyhow::anyhow!("join: {e}"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let title = first_non_empty_line(&text).unwrap_or_else(|| "Uploaded paper".to_string());
    let abstract_ = pull_abstract(&text);
    let sections = grokrxiv_ingest::extract::split_sections(&text);
    let bibliography = grokrxiv_ingest::extract::extract_bibliography(&text);

    Ok(PaperExtract {
        arxiv_id: "preview".into(),
        title,
        authors: vec![Author {
            name: "Unknown".into(),
            affiliation: None,
            email: None,
        }],
        abstract_,
        field: None,
        sections,
        figures: vec![] as Vec<FigureRef>,
        bibliography,
        source_format: Some("pdf".to_string()),
    })
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn extract_paper(_pdf: &[u8]) -> anyhow::Result<PaperExtract> {
    crate::stubs::pdf_to_text(std::path::Path::new("")).await
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(200).collect())
}

fn pull_abstract(text: &str) -> String {
    // Best-effort: take a paragraph immediately after a line containing the
    // word "abstract" (case-insensitive); otherwise the first 1k chars of the
    // first paragraph.
    let needle = "abstract";
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find(needle) {
        let tail = &text[pos..];
        let after_newline = tail.find('\n').map(|i| pos + i + 1).unwrap_or(pos);
        let rest = &text[after_newline..];
        let para_end = rest.find("\n\n").unwrap_or_else(|| rest.len().min(1500));
        let para = rest[..para_end].trim().to_string();
        if !para.is_empty() {
            return para.chars().take(1500).collect();
        }
    }
    text.split("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(1500)
        .collect()
}

// ---------------------------------------------------------------------------
// LLM call
// ---------------------------------------------------------------------------

async fn run_meta_review(state: &AppState, paper: &PaperExtract) -> anyhow::Result<MetaReview> {
    let registry = state
        .providers
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no LLM provider configured"))?;
    let provider = registry.default.clone();

    let schema = meta_review_schema();
    let prompt = build_preview_prompt(paper);

    let req = ChatRequest {
        system: Some(SYSTEM_PROMPT.into()),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text(prompt)],
        }],
        model: state.config.preview_model.clone(),
        max_tokens: 4_000,
        temperature: 0.2,
        response_format: ResponseFormat::JsonSchema(schema),
        cache_system: true,
    };

    let resp = provider
        .complete(req)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let value: serde_json::Value = match serde_json::from_str(resp.text.trim()) {
        Ok(v) => v,
        Err(_) => {
            // Some models (and some retries) wrap their JSON in ```json fences;
            // strip them.
            let stripped = strip_code_fences(resp.text.trim());
            serde_json::from_str(stripped)
                .map_err(|e| anyhow::anyhow!("non-JSON response: {e}: {}", resp.text))?
        }
    };
    let meta: MetaReview = serde_json::from_value(value)
        .map_err(|e| anyhow::anyhow!("meta-review schema mismatch: {e}"))?;
    Ok(meta)
}

fn strip_code_fences(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    s
}

const SYSTEM_PROMPT: &str = "You are GrokRxiv, an AI peer reviewer. Produce a careful, \
honest, single-pass meta-review of the supplied paper. Return strict JSON only.";

fn build_preview_prompt(paper: &PaperExtract) -> String {
    format!(
        "Paper title: {title}\n\nAbstract:\n{abstract_}\n\nSections (head only):\n{sections}\n\n\
         Produce a meta-review JSON object with fields: summary, strengths (list), weaknesses (list), \
         questions (list), recommendation (one of accept|minor_revision|major_revision|reject), \
         confidence (0..1).",
        title = paper.title,
        abstract_ = paper.abstract_,
        sections = paper
            .sections
            .iter()
            .map(|s| format!("- {}", s.heading))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn meta_review_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["summary", "strengths", "weaknesses", "questions", "recommendation", "confidence"],
        "properties": {
            "summary": { "type": "string" },
            "strengths": { "type": "array", "items": { "type": "string" } },
            "weaknesses": { "type": "array", "items": { "type": "string" } },
            "questions": { "type": "array", "items": { "type": "string" } },
            "recommendation": {
                "type": "string",
                "enum": ["accept", "minor_revision", "major_revision", "reject"]
            },
            "confidence": { "type": "number", "minimum": 0, "maximum": 1 }
        }
    })
}

// ---------------------------------------------------------------------------
// Bundle rendering
// ---------------------------------------------------------------------------

struct RenderedBundle {
    html: String,
    zip: Vec<u8>,
}

#[cfg(feature = "grokrxiv-render")]
async fn render_bundle(meta: &MetaReview, paper: &PaperExtract) -> anyhow::Result<RenderedBundle> {
    // For the preview the agent list is just the synthesized meta-reviewer
    // output, so we expose a single agent record for the renderer.
    let agent = grokrxiv_render::AgentRecord {
        role: grokrxiv_schemas::AgentRole::MetaReviewer,
        model: "preview".to_string(),
        output: serde_json::to_value(meta).unwrap_or(json!({})),
        verifier: grokrxiv_schemas::VerifierResult {
            status: grokrxiv_schemas::VerifierStatus::Pass,
            notes: json!({ "preview": true }),
        },
    };
    let agents = [agent];
    let html = grokrxiv_render::render_html(meta, paper, &agents)
        .map_err(|e| anyhow::anyhow!("render_html: {e}"))?;
    let md = grokrxiv_render::render_markdown(meta, paper, &agents);
    let tex = grokrxiv_render::render_latex(meta, paper, &agents);
    let metadata = json!({
        "is_sample": true,
        "paper_title": paper.title,
        "recommendation": meta.recommendation,
    });
    let agent_json = serde_json::to_vec_pretty(&agents[0].output).unwrap_or_default();
    let agent_files: Vec<(String, Vec<u8>)> =
        vec![("agents/meta_reviewer.json".to_string(), agent_json)];
    let zip = grokrxiv_render::build_zip(&html, &md, &tex, None, &agent_files, &metadata)
        .map_err(|e| anyhow::anyhow!("build_zip: {e}"))?;
    Ok(RenderedBundle { html, zip })
}

#[cfg(not(feature = "grokrxiv-render"))]
async fn render_bundle(meta: &MetaReview, paper: &PaperExtract) -> anyhow::Result<RenderedBundle> {
    let stub = crate::stubs::render_bundle(meta, paper).await?;
    Ok(RenderedBundle {
        html: stub.html,
        zip: stub.bundle,
    })
}
