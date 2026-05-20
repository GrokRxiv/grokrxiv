use crate::state::AppState;
use uuid::Uuid;

fn html_quality_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_HTML_QUALITY_DISABLE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Render persisted review state into HTML, Markdown, LaTeX, and ZIP artifacts.
#[cfg(feature = "grokrxiv-render")]
pub async fn render_to_disk(state: &AppState, review_id: Uuid) -> anyhow::Result<()> {
    use grokrxiv_render::AgentRecord;
    use grokrxiv_schemas::{MetaReview, PaperExtract, Section, VerifierResult, VerifierStatus};

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    let bundle = crate::db::load_review_render_bundle(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review render bundle: {e}"))?;
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

    // Reconstruct the input artifact each persisted agent saw so the render
    // bundle mirrors the review database state.
    let specialist_input = crate::db::load_review_input(pool, review_id)
        .await
        .map_err(|e| anyhow::anyhow!("load review_inputs: {e}"))?
        .unwrap_or(serde_json::Value::Null);
    let meta_input_for_render = {
        let mut specialists_map = serde_json::Map::new();
        for row in &bundle.agents {
            if row.role != "meta_reviewer" {
                specialists_map.insert(row.role.clone(), row.output.clone());
            }
        }
        serde_json::json!({ "specialists": serde_json::Value::Object(specialists_map) })
    };

    let mut agents: Vec<AgentRecord> = Vec::with_capacity(bundle.agents.len());
    let mut agent_jsons: Vec<(String, Vec<u8>)> = Vec::with_capacity(bundle.agents.len());
    for row in bundle.agents {
        let role_slug = row.role.clone();
        if let Some(role) = parse_role_slug(&role_slug) {
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
            let input_artifact = if role_slug == "meta_reviewer" {
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
                role,
                model: row.model,
                output: row.output,
                verifier,
            });
        }
    }

    // The renderer only needs a minimal extract when persisted section and
    // bibliography bodies are unavailable.
    let extract = PaperExtract {
        arxiv_id: arxiv_id.clone(),
        title: title.clone(),
        authors: Vec::new(),
        abstract_: abstract_.unwrap_or_default(),
        field,
        sections: Vec::<Section>::new(),
        figures: Vec::new(),
        bibliography: Vec::new(),
        source_format: None,
    };

    let html = grokrxiv_render::render_html(&meta, &extract, &agents)
        .map_err(|e| anyhow::anyhow!("render_html: {e}"))?;
    let md = grokrxiv_render::render_markdown(&meta, &extract, &agents);
    let tex = grokrxiv_render::render_latex(&meta, &extract, &agents);
    let metadata = serde_json::json!({
        "review_id": review_id,
        "arxiv_id": extract.arxiv_id,
    });
    let zip = grokrxiv_render::build_zip(&html, &md, &tex, None, &agent_jsons, &metadata)
        .map_err(|e| anyhow::anyhow!("build_zip: {e}"))?;
    let dir = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    tokio::fs::create_dir_all(&dir).await.ok();
    tokio::fs::write(dir.join("review.html"), &html).await?;
    tokio::fs::write(dir.join("review.md"), md).await?;
    tokio::fs::write(dir.join("review.tex"), tex).await?;
    tokio::fs::write(dir.join("bundle.zip"), &zip).await?;

    // The HTML quality pass is observational and may rewrite review.html plus
    // a formatting_fixes.json sidecar when enabled.
    if !html_quality_disabled() {
        if let Err(e) = crate::html_review::review_and_fix_html(state, review_id, &dir).await {
            tracing::warn!(%review_id, err = %e, "html_quality: stage errored — leaving review.html as-is");
        }
    }

    let dir_str = format!("artifacts/{review_id}");
    let _ = crate::db::set_review_artifacts(
        pool,
        review_id,
        Some(&format!("{dir_str}/review.html")),
        None,
        Some(&format!("{dir_str}/bundle.zip")),
    )
    .await;

    Ok(())
}

#[cfg(feature = "grokrxiv-render")]
fn fallback_meta(title: &str) -> grokrxiv_schemas::MetaReview {
    use grokrxiv_schemas::{MetaReview, Recommendation};
    MetaReview {
        summary: format!("Review of {}", title),
        strengths: vec![],
        weaknesses: vec![],
        questions: vec![],
        recommendation: Recommendation::MinorRevision,
        confidence: 0.5,
    }
}

#[cfg(feature = "grokrxiv-render")]
fn parse_role_slug(s: &str) -> Option<grokrxiv_schemas::AgentRole> {
    use grokrxiv_schemas::AgentRole;
    Some(match s {
        "summary" => AgentRole::Summary,
        "technical_correctness" => AgentRole::TechnicalCorrectness,
        "novelty" => AgentRole::Novelty,
        "reproducibility" => AgentRole::Reproducibility,
        "citation" => AgentRole::Citation,
        "meta_reviewer" => AgentRole::MetaReviewer,
        _ => return None,
    })
}

#[cfg(not(feature = "grokrxiv-render"))]
pub async fn render_to_disk(_state: &AppState, _review_id: Uuid) -> anyhow::Result<()> {
    Ok(())
}
