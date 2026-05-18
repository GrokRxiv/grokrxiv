//! Post-render HTML quality harness.
//!
//! Runs after `review.html` is written by the render stage and BEFORE the
//! review row reaches `awaiting_moderation`. Spawns the `codex` CLI
//! (provider=openai, model=gpt-5.5) which audits the rendered HTML and
//! returns a corrected copy + a structured list of fixes. The fix log is
//! persisted as a sidecar JSON in the artifact directory so the moderator
//! can audit what was rewritten.
//!
//! This is NOT a peer reviewer of the source arXiv paper. It only audits
//! GrokRxiv's own generated artifact for readability + rendering bugs (e.g.
//! literal LaTeX layout commands surfacing as text, broken anchors, stray
//! template artifacts). Failures are non-fatal: any error logs a warning
//! and the original `review.html` is left untouched.
//!
//! Output schema: `schemas/html_quality_review.schema.json`.

use crate::agents::{
    runners::cli::CliRunner, AgentInput, AgentMode, AgentRunner, AgentRunnerKind, AgentSpec,
    SandboxPolicy, ToolPolicy,
};
use crate::state::AppState;
use anyhow::{Context, Result};
use grokrxiv_schemas::AgentRole;
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;

const HTML_QUALITY_SCHEMA: &str =
    include_str!("../../../schemas/html_quality_review.schema.json");
const HTML_QUALITY_PROMPT_TEMPLATE: &str =
    include_str!("../../../prompts/html_quality.md");
const PR_TEXT_QUALITY_SCHEMA: &str =
    include_str!("../../../schemas/pr_text_quality_review.schema.json");
const PR_TEXT_QUALITY_PROMPT_TEMPLATE: &str =
    include_str!("../../../prompts/pr_text_quality.md");

/// Cleaned PR title + body returned by [`clean_pr_text`]. Fields default to
/// the inputs when codex declines to rewrite them.
#[derive(Debug, Clone)]
pub struct CleanedPrText {
    pub title: String,
    pub body: String,
    pub fixes: serde_json::Value,
    pub summary: String,
    pub confidence: f64,
}

/// Pre-approve PR-text formatter. Spawns codex (gpt-5.5) with the proposed
/// `title` + GitHub-markdown `body`, gets back cleaned versions and a fix
/// log. Used by `cli.rs::approve_impl` to scrub unexpanded LaTeX macros
/// (`\sysname` etc.) and other latex residue out of PR titles BEFORE the
/// PR is opened on GrokRxiv/grokrxiv-reviews.
///
/// All failures are non-fatal: any error logs a warning and the caller
/// receives a `CleanedPrText` carrying the ORIGINAL title + body so the
/// PR opens with what we had.
pub async fn clean_pr_text(
    state: &AppState,
    review_id: Uuid,
    title: &str,
    body: &str,
) -> CleanedPrText {
    let fallback = || CleanedPrText {
        title: title.to_string(),
        body: body.to_string(),
        fixes: Value::Array(vec![]),
        summary: String::new(),
        confidence: 0.0,
    };
    let model = std::env::var("GROKRXIV_HTML_QUALITY_MODEL")
        .unwrap_or_else(|_| "gpt-5.5".to_string());
    let timeout_secs: u32 = std::env::var("GROKRXIV_HTML_QUALITY_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);

    let schema: Value = match serde_json::from_str(PR_TEXT_QUALITY_SCHEMA) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(%review_id, err = %e, "clean_pr_text: schema parse failed — passing through originals");
            return fallback();
        }
    };
    let spec = AgentSpec {
        role: AgentRole::MetaReviewer,
        runner: AgentRunnerKind::Cli,
        sandbox: SandboxPolicy::None,
        mode: AgentMode::ReviewOnly,
        provider: "openai".to_string(),
        model: model.clone(),
        schema,
        tool_policy: ToolPolicy::default(),
        max_retries: 1,
        timeout_secs,
    };

    let prompt = PR_TEXT_QUALITY_PROMPT_TEMPLATE
        .replace("{{title}}", title)
        .replace("{{body}}", body);
    let input = AgentInput {
        paper_id: Uuid::nil(),
        review_id,
        role: AgentRole::MetaReviewer,
        content_hash_material: Value::String("pr_text_quality".into()),
        artifact: Value::String(format!("{title}\n\n{body}")),
        system_prompt: "You are the PR text formatter. Output strict JSON only.".into(),
        user_prompt: prompt,
        source_bundle_path: None,
    };

    let runner = CliRunner::new();
    let run = match runner.run(&spec, &input).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%review_id, err = %e, "clean_pr_text: codex CLI failed — passing through originals");
            return fallback();
        }
    };

    let fixed_title = run
        .output
        .get("fixed_title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| title.to_string());
    let fixed_body = run
        .output
        .get("fixed_body")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| body.to_string());
    let summary = run
        .output
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let confidence = run
        .output
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let fixes = run
        .output
        .get("fixes")
        .cloned()
        .unwrap_or(Value::Array(vec![]));

    // Belt-and-suspenders: don't accept a "cleaned" body that dropped the
    // grokrxiv-review-id marker (the merge webhook depends on it).
    let body_to_use = if fixed_body.contains("grokrxiv-review-id:") {
        fixed_body
    } else {
        tracing::warn!(
            %review_id,
            "clean_pr_text: codex output dropped the review-id marker — reverting to original body"
        );
        body.to_string()
    };

    tracing::info!(
        %review_id,
        model = %model,
        title_changed = title != fixed_title,
        body_changed = body != body_to_use,
        confidence,
        "clean_pr_text: PR title/body audit complete"
    );
    let _ = state;
    CleanedPrText {
        title: fixed_title,
        body: body_to_use,
        fixes,
        summary,
        confidence,
    }
}

/// Read `<dir>/review.html`, run the html_quality codex audit, write the
/// fixed HTML back, and persist `<dir>/formatting_fixes.json` with the
/// structured fix log. Returns `Ok(false)` if no review.html exists yet
/// (caller can decide whether that's an error). Returns `Ok(true)` on
/// success or when codex emitted zero fixes.
///
/// Failures inside this function are surfaced via `tracing::warn!` and
/// returned as `Ok(false)` — the caller treats them as "html_quality did
/// not run, ship the original" and continues.
pub async fn review_and_fix_html(
    state: &AppState,
    review_id: Uuid,
    artifacts_dir: &Path,
) -> Result<bool> {
    let html_path = artifacts_dir.join("review.html");
    let html_bytes = match tokio::fs::read(&html_path).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                %review_id,
                path = %html_path.display(),
                err = %e,
                "html_quality: review.html missing — skipping"
            );
            return Ok(false);
        }
    };
    let original_html = String::from_utf8_lossy(&html_bytes).to_string();

    let model = std::env::var("GROKRXIV_HTML_QUALITY_MODEL")
        .unwrap_or_else(|_| "gpt-5.5".to_string());
    let timeout_secs: u32 = std::env::var("GROKRXIV_HTML_QUALITY_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);

    let schema: Value = serde_json::from_str(HTML_QUALITY_SCHEMA)
        .context("html_quality schema parse")?;
    let spec = AgentSpec {
        role: AgentRole::MetaReviewer, // closest existing role; not persisted to review_agents
        runner: AgentRunnerKind::Cli,
        sandbox: SandboxPolicy::None,
        mode: AgentMode::ReviewOnly,
        provider: "openai".to_string(),
        model: model.clone(),
        schema,
        tool_policy: ToolPolicy::default(),
        max_retries: 1,
        timeout_secs,
    };

    let prompt = render_html_quality_prompt(&original_html);
    let input = AgentInput {
        paper_id: Uuid::nil(),
        review_id,
        role: AgentRole::MetaReviewer,
        content_hash_material: Value::String("html_quality".into()),
        artifact: Value::String(original_html.clone()),
        system_prompt: "You are the HTML Quality harness. Output strict JSON only.".into(),
        user_prompt: prompt,
        source_bundle_path: None,
    };

    let runner = CliRunner::new();
    let run = match runner.run(&spec, &input).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                %review_id,
                err = %e,
                "html_quality: codex CLI failed — leaving review.html unchanged"
            );
            return Ok(false);
        }
    };

    let fixed_html = run
        .output
        .get("fixed_html")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let fixes_len = run
        .output
        .get("fixes")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let confidence = run
        .output
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if fixed_html.is_empty() {
        tracing::warn!(
            %review_id,
            "html_quality: codex returned no fixed_html field — leaving review.html unchanged"
        );
        return Ok(false);
    }

    // Only rewrite the file if codex actually changed something. Saves a
    // disk write on every review and makes the no-op case observable.
    let changed = fixed_html != original_html;
    if changed {
        tokio::fs::write(&html_path, fixed_html.as_bytes())
            .await
            .with_context(|| format!("write fixed review.html: {}", html_path.display()))?;
    }

    // Persist the sidecar log every time so the moderator can see "no fixes
    // needed" vs "ran but did nothing because confidence was low".
    let sidecar_path = artifacts_dir.join("formatting_fixes.json");
    let sidecar = serde_json::json!({
        "review_id": review_id,
        "model": model,
        "changed": changed,
        "fixes": run.output.get("fixes").cloned().unwrap_or(Value::Array(vec![])),
        "summary": run.output.get("summary").cloned().unwrap_or(Value::String(String::new())),
        "confidence": confidence,
    });
    if let Err(e) = tokio::fs::write(
        &sidecar_path,
        serde_json::to_vec_pretty(&sidecar).unwrap_or_default(),
    )
    .await
    {
        tracing::warn!(%review_id, err = %e, "html_quality: failed to persist sidecar log");
    }

    tracing::info!(
        %review_id,
        model = %model,
        fixes = fixes_len,
        confidence,
        changed,
        "html_quality: review.html audit complete"
    );
    let _ = state; // currently unused; reserved for db persistence in a follow-up
    Ok(true)
}

fn render_html_quality_prompt(html: &str) -> String {
    HTML_QUALITY_PROMPT_TEMPLATE.replace("{{html}}", html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_html_quality_prompt_substitutes_html_placeholder() {
        let rendered = render_html_quality_prompt("<h1>Hello</h1>");
        assert!(rendered.contains("<h1>Hello</h1>"));
        assert!(!rendered.contains("{{html}}"));
    }

    #[test]
    fn html_quality_schema_parses_as_json() {
        let v: serde_json::Value = serde_json::from_str(HTML_QUALITY_SCHEMA)
            .expect("schema is valid JSON");
        assert_eq!(v["$id"], "https://grokrxiv.org/schemas/html_quality_review.schema.json");
        // required fields present
        let required = v["required"].as_array().expect("required is array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        for f in ["fixed_html", "fixes", "summary", "confidence"] {
            assert!(names.contains(&f), "schema missing required field {f}");
        }
    }
}
