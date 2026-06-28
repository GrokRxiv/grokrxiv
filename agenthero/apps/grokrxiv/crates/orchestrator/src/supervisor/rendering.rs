use crate::state::AppState;
use std::collections::HashSet;
use uuid::Uuid;

const PAPER_REVIEW_DAG_ID: &str = "paper-review";

fn html_quality_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_HTML_QUALITY_DISABLE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn render_extract_from_input(
    specialist_input: &serde_json::Value,
    arxiv_id: String,
    title: String,
    abstract_: Option<String>,
    field: Option<String>,
) -> grokrxiv_schemas::PaperExtract {
    serde_json::from_value::<grokrxiv_schemas::PaperExtract>(specialist_input.clone())
        .unwrap_or_else(|_| grokrxiv_schemas::PaperExtract {
            arxiv_id,
            title,
            authors: Vec::new(),
            abstract_: abstract_.unwrap_or_default(),
            field,
            sections: Vec::new(),
            figures: Vec::new(),
            bibliography: Vec::new(),
            source_format: None,
        })
}

/// Caller-provided options for deterministic review artifact rendering.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderToDiskOptions {
    /// Optional timeout passed to the HTML quality CLI role for this render.
    pub html_quality_timeout_secs: Option<u32>,
}

/// Summary of non-essential render sub-stages.
#[derive(Debug, Clone, Copy)]
pub struct RenderToDiskReport {
    /// Whether HTML quality cleanup was enabled for this render.
    pub html_quality_enabled: bool,
    /// `Some(true)` when cleanup ran, `Some(false)` when it skipped/failed,
    /// and `None` when cleanup was disabled.
    pub html_quality_ran: Option<bool>,
}

/// Render persisted review state into HTML, Markdown, LaTeX, and ZIP artifacts.
#[cfg(feature = "grokrxiv-render")]
pub async fn render_to_disk(state: &AppState, review_id: Uuid) -> anyhow::Result<()> {
    render_to_disk_with_options(state, review_id, RenderToDiskOptions::default())
        .await
        .map(|_| ())
}

/// Render persisted review state with caller-provided sub-stage options.
#[cfg(feature = "grokrxiv-render")]
pub async fn render_to_disk_with_options(
    state: &AppState,
    review_id: Uuid,
    options: RenderToDiskOptions,
) -> anyhow::Result<RenderToDiskReport> {
    use grokrxiv_render::AgentRecord;
    use grokrxiv_schemas::{MetaReview, VerifierResult, VerifierStatus};

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    crate::cli_status::emit_detail(
        "render load",
        crate::cli_status::StatusMark::Run,
        "review state",
    );
    let bundle = crate::db::load_review_render_bundle(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review render bundle: {e}"))?;
    crate::cli_status::emit_detail(
        "render load",
        crate::cli_status::StatusMark::Ok,
        "review state",
    );
    let crate::db::ReviewRenderHeadRow {
        meta_review: meta_json,
        paper_id: _paper_id,
        arxiv_id,
        title,
        abstract_,
        field,
    } = bundle.review;

    let meta: MetaReview = meta_json
        .and_then(|v| serde_json::from_value::<MetaReview>(v).ok())
        .unwrap_or_else(|| fallback_meta(&title));
    let specialist_roles: HashSet<String> =
        crate::agents::config::dag_feeds_meta_roles(PAPER_REVIEW_DAG_ID)
            .unwrap_or_default()
            .into_iter()
            .collect();
    let meta_input_roles: HashSet<String> =
        crate::agents::config::dag_meta_input_roles(PAPER_REVIEW_DAG_ID)
            .unwrap_or_else(|_| {
                state
                    .agent_configs
                    .iter()
                    .filter(|(_, cfg)| cfg.prompt_context.meta_input)
                    .map(|(role, _)| role.clone())
                    .collect()
            })
            .into_iter()
            .collect();

    // Reconstruct the input artifact each persisted agent saw so the render
    // bundle mirrors the review database state.
    let specialist_input = crate::db::load_review_input(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review_inputs: {e}"))?
        .unwrap_or(serde_json::Value::Null);
    let meta_input_for_render = {
        let mut specialists_map = serde_json::Map::new();
        for row in &bundle.agents {
            if specialist_roles.contains(&row.role) {
                specialists_map.insert(row.role.clone(), row.output.clone());
            }
        }
        serde_json::json!({ "specialists": serde_json::Value::Object(specialists_map) })
    };

    let mut agents: Vec<AgentRecord> = Vec::with_capacity(bundle.agents.len());
    let mut agent_jsons: Vec<(String, Vec<u8>)> = Vec::with_capacity(bundle.agents.len());
    for row in bundle.agents {
        let role_slug = row.role.clone();
        let status = row
            .verifier_status
            .as_deref()
            .and_then(crate::db::verifier_status_from_db_str)
            .unwrap_or(VerifierStatus::Pass);
        let notes = row.verifier_notes.unwrap_or(serde_json::Value::Null);
        let verifier = VerifierResult {
            status,
            notes: notes.clone(),
        };
        let input_artifact = if meta_input_roles.contains(&role_slug) {
            meta_input_for_render.clone()
        } else {
            specialist_input.clone()
        };
        let artifact = serde_json::json!({
            "role": role_slug,
            "model": row.model.clone(),
            "input_artifact": input_artifact,
            "output": row.output.clone(),
            "verifier": {
                "status": status,
                "notes": notes,
            },
        });
        let path = format!("agents/{role_slug}.json");
        let bytes = serde_json::to_vec_pretty(&artifact)
            .map_err(|e| anyhow::anyhow!("serialize {path}: {e}"))?;
        agent_jsons.push((path, bytes));
        agents.push(AgentRecord {
            role: role_slug,
            model: row.model,
            output: row.output,
            verifier,
        });
    }

    let extract = render_extract_from_input(
        &specialist_input,
        arxiv_id.clone(),
        title.clone(),
        abstract_,
        field,
    );

    crate::cli_status::emit_detail("render html", crate::cli_status::StatusMark::Run, "");
    let html = grokrxiv_render::render_html(&meta, &extract, &agents)
        .map_err(|e| anyhow::anyhow!("render_html: {e}"))?;
    crate::cli_status::emit_detail("render html", crate::cli_status::StatusMark::Ok, "");
    crate::cli_status::emit_detail("render markdown", crate::cli_status::StatusMark::Run, "");
    let md = grokrxiv_render::render_markdown(&meta, &extract, &agents);
    crate::cli_status::emit_detail("render markdown", crate::cli_status::StatusMark::Ok, "");
    crate::cli_status::emit_detail("render latex", crate::cli_status::StatusMark::Run, "");
    let tex = grokrxiv_render::render_latex(&meta, &extract, &agents);
    crate::cli_status::emit_detail("render latex", crate::cli_status::StatusMark::Ok, "");
    let metadata = serde_json::json!({
        "review_id": review_id,
        "arxiv_id": extract.arxiv_id,
    });
    crate::cli_status::emit_detail("render bundle", crate::cli_status::StatusMark::Run, "");
    let zip = grokrxiv_render::build_zip(&html, &md, &tex, None, &agent_jsons, &metadata)
        .map_err(|e| anyhow::anyhow!("build_zip: {e}"))?;
    crate::cli_status::emit_detail("render bundle", crate::cli_status::StatusMark::Ok, "");
    let dir = crate::artifacts::review_artifact_dir(review_id);
    crate::cli_status::emit_detail("render write", crate::cli_status::StatusMark::Run, "");
    tokio::fs::create_dir_all(&dir).await.ok();
    tokio::fs::write(dir.join("review.html"), &html).await?;
    tokio::fs::write(dir.join("review.md"), md).await?;
    tokio::fs::write(dir.join("review.tex"), tex).await?;
    tokio::fs::write(dir.join("bundle.zip"), &zip).await?;
    crate::cli_status::emit_detail("render write", crate::cli_status::StatusMark::Ok, "");

    // The HTML quality pass is observational and may rewrite review.html plus
    // a formatting_fixes.json sidecar when enabled.
    let html_quality_enabled = !html_quality_disabled();
    let mut html_quality_ran = None;
    if html_quality_enabled {
        crate::cli_status::emit_detail("html quality", crate::cli_status::StatusMark::Run, "");
        match crate::html_review::review_and_fix_html_with_timeout(
            state,
            review_id,
            &dir,
            options.html_quality_timeout_secs,
        )
        .await
        {
            Ok(ran) => {
                html_quality_ran = Some(ran);
                let mark = if ran {
                    crate::cli_status::StatusMark::Ok
                } else {
                    crate::cli_status::StatusMark::Warn
                };
                crate::cli_status::emit_detail("html quality", mark, "");
            }
            Err(e) => {
                html_quality_ran = Some(false);
                crate::cli_status::emit_detail(
                    "html quality",
                    crate::cli_status::StatusMark::Warn,
                    &format!("{e:#}"),
                );
                tracing::warn!(%review_id, err = %e, "html_quality: stage errored — leaving review.html as-is");
            }
        }
    }

    let dir_str = crate::artifacts::review_artifact_ref(review_id);
    crate::cli_status::emit_detail("render persist", crate::cli_status::StatusMark::Run, "");
    let _ = crate::db::set_review_artifacts(
        pool,
        review_id,
        Some(&format!("{dir_str}/review.html")),
        None,
        Some(&format!("{dir_str}/bundle.zip")),
    )
    .await;
    crate::cli_status::emit_detail("render persist", crate::cli_status::StatusMark::Ok, "");

    Ok(RenderToDiskReport {
        html_quality_enabled,
        html_quality_ran,
    })
}

#[cfg(test)]
mod tests {
    use super::render_extract_from_input;
    use serde_json::json;

    #[test]
    fn render_extract_uses_review_input_bibliography() {
        let input = json!({
            "arxiv_id": "2512.03648",
            "title": "Condensed Group Cohomology",
            "authors": [],
            "abstract": "abs",
            "field": "math.AT",
            "sections": [],
            "figures": [],
            "bibliography": [{
                "raw": "Bhatt2013ThePT: The Pro-etale topology for schemes",
                "doi": "10.24033/ast.960",
                "arxiv_id": null,
                "title": "The Pro-etale topology for schemes"
            }],
            "source_format": "tex"
        });
        let extract = render_extract_from_input(
            &input,
            "fallback".to_string(),
            "fallback title".to_string(),
            None,
            None,
        );
        assert_eq!(extract.bibliography.len(), 1);
        assert_eq!(
            extract.bibliography[0].doi.as_deref(),
            Some("10.24033/ast.960")
        );
    }
}

#[cfg(feature = "grokrxiv-render")]
fn fallback_meta(title: &str) -> grokrxiv_schemas::MetaReview {
    use grokrxiv_schemas::{MetaReview, Recommendation};
    MetaReview {
        summary: format!("Review of {}", title),
        strengths: vec![],
        weaknesses: vec![],
        questions: vec![],
        revision_targets: vec![],
        recommendation: Recommendation::MinorRevision,
        confidence: 0.5,
    }
}

#[cfg(not(feature = "grokrxiv-render"))]
pub async fn render_to_disk(_state: &AppState, _review_id: Uuid) -> anyhow::Result<()> {
    Ok(())
}

/// No-op render report when the render feature is not compiled.
#[cfg(not(feature = "grokrxiv-render"))]
pub async fn render_to_disk_with_options(
    _state: &AppState,
    _review_id: Uuid,
    _options: RenderToDiskOptions,
) -> anyhow::Result<RenderToDiskReport> {
    Ok(RenderToDiskReport {
        html_quality_enabled: false,
        html_quality_ran: None,
    })
}
