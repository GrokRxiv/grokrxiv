//! FP4 acceptance test for the real typed review DAG.
//!
//! Stubs Anthropic with `wiremock`, runs `supervisor::run_review_dag` against
//! a hand-built `PaperExtract`, and asserts:
//!
//!   - exactly 6 rows in `review_agents` for the review
//!   - exactly 1 row in `review_inputs` for the review with the shared
//!     specialist input artifact (FP6 A2 dedup)
//!   - every row's `verifier_status = 'pass'` against its role-specific schema
//!   - the synthesised meta input (rebuilt from specialist `output` rows)
//!     carries a `specialists` object with all five role slugs
//!   - the meta-reviewer's `output` deserializes as `MetaReview`
//!
//! Requires a live Postgres at `DATABASE_URL` (matches CI's local Supabase
//! container). Without it, the test is skipped with a tracing notice — that's
//! the simplest way to be CI-portable without dragging in `testcontainers`.

#![cfg(feature = "grokrxiv-ingest")]

use std::sync::Arc;

use grokrxiv_llm_adapter::providers::claude::ClaudeProvider;
use grokrxiv_llm_adapter::{LLMProvider, ProviderConfig};
use grokrxiv_orchestrator::supervisor::run_review_dag;
use grokrxiv_orchestrator::{AppState, Config};
use grokrxiv_schemas::{AgentRole, MetaReview, PaperExtract, Section};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::io::Read;
use uuid::Uuid;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn claude_response(text: &str) -> Value {
    json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "model": "claude-opus-4-7",
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 100, "output_tokens": 50 }
    })
}

/// Schema-compliant JSON each role returns. Mirrors `schemas/*.schema.json`.
fn role_output(role: AgentRole) -> Value {
    match role {
        AgentRole::Summary => json!({
            "plain_language_summary": "A clear summary of the paper for a literate non-expert.",
            "key_contributions": ["c1", "c2"],
            "tldr": "One-sentence elevator pitch."
        }),
        AgentRole::TechnicalCorrectness => json!({
            "claims": [{
                "id": "c1",
                "claim": "The main result follows from Lemma 2.",
                "assessment": "supported",
                "severity": "info"
            }],
            "overall_correctness": "mostly_sound",
            "confidence": 0.7
        }),
        AgentRole::Novelty => json!({
            "novelty_score": 0.6,
            "verdict": "incremental",
            "confidence": 0.7
        }),
        AgentRole::Reproducibility => json!({
            "code_availability": "open_source",
            "data_availability": "public",
            "reproducibility_score": 0.8,
            "confidence": 0.7
        }),
        AgentRole::Citation => json!({
            "entries": [],
            "summary": "All cited references resolved cleanly.",
            "confidence": 0.6
        }),
        AgentRole::MetaReviewer => json!({
            "summary": "Solid, mostly-sound, incremental paper that's reproducible.",
            "strengths": ["clear writing", "open code"],
            "weaknesses": ["limited novelty"],
            "questions": ["how does it scale?"],
            "recommendation": "minor_revision",
            "confidence": 0.75
        }),
    }
}

/// Distinctive substring that appears in each role's system prompt and lets us
/// route the wiremock to the right role. These mirror `role_system_prompt` in
/// `supervisor.rs` exactly.
fn role_system_needle(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Summary => "summarize papers in plain language",
        AgentRole::TechnicalCorrectness => {
            "assess mathematical, logical, and empirical correctness"
        }
        AgentRole::Novelty => "compare against prior work and judge novelty",
        AgentRole::Reproducibility => "judge whether the work can be reproduced",
        AgentRole::Citation => "verify cited references and surface missing ones",
        AgentRole::MetaReviewer => "synthesize five specialist reviews",
    }
}

fn fake_paper() -> PaperExtract {
    PaperExtract {
        arxiv_id: format!("test/{}", Uuid::new_v4()),
        title: "Test Paper for FP4 DAG".into(),
        authors: vec![],
        abstract_: "We test the typed DAG.".into(),
        field: Some("cs.LG".into()),
        sections: vec![Section {
            heading: "1. Introduction".into(),
            body_markdown: "Introductory text.".into(),
        }],
        figures: vec![],
        bibliography: vec![],
    }
}

async fn setup_pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    match PgPool::connect(&url).await {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("dag test: could not connect to {url}: {e}");
            None
        }
    }
}

#[tokio::test]
async fn typed_dag_persists_six_agents_with_inputs_and_passes_verifier() {
    let Some(pool) = setup_pool().await else {
        eprintln!("DAG test skipped: DATABASE_URL not set or unreachable.");
        return;
    };

    // 1. Spin up wiremock and register six role-specific responders. Each
    //    matcher keys on the unique text we know goes into the system prompt
    //    for that role; the response body is schema-compliant JSON.
    let server = MockServer::start().await;
    for role in [
        AgentRole::Summary,
        AgentRole::TechnicalCorrectness,
        AgentRole::Novelty,
        AgentRole::Reproducibility,
        AgentRole::Citation,
        AgentRole::MetaReviewer,
    ] {
        let needle = role_system_needle(role);
        let text = serde_json::to_string(&role_output(role)).expect("serialize role output");
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_string_contains(needle))
            .respond_with(ResponseTemplate::new(200).set_body_json(claude_response(&text)))
            .mount(&server)
            .await;
    }

    // 2. Build an AppState whose default provider points at the mock server.
    let mut cfg = ProviderConfig {
        anthropic_api_key: Some("test-key".into()),
        ..ProviderConfig::default()
    };
    cfg.http = Some(Arc::new(reqwest::Client::new()));
    let provider: Arc<dyn LLMProvider> = Arc::new(
        ClaudeProvider::from_config(&cfg)
            .expect("claude provider")
            .with_base_url(format!("{}/v1/messages", server.uri())),
    );

    // Disable the worker pool — we won't enqueue jobs. `from_config` connects
    // to the same DATABASE_URL.
    let mut config = Config::from_env();
    // Ensure DATABASE_URL is set inside Config even when the env var is loaded
    // by tokio after `Config::from_env` ran above. Tests run with the env var
    // set so this is normally a no-op.
    if config.database_url.is_none() {
        config.database_url = std::env::var("DATABASE_URL").ok();
    }
    let state = AppState::from_config(config).await.expect("AppState built");

    // 3. Insert a paper row directly (skipping arxiv ingest).
    let extract = fake_paper();
    let paper_id = grokrxiv_orchestrator::db::upsert_paper(&pool, &extract, None)
        .await
        .expect("upsert paper");

    // 4. Drive the DAG.
    let review_id = run_review_dag(&state, &pool, provider, paper_id, extract.clone())
        .await
        .expect("DAG ran");

    // 5. Read back and assert.
    let rows: Vec<(String, Value, String)> = sqlx::query_as(
        "select role, output, verifier_status \
         from review_agents where review_id = $1 order by role",
    )
    .bind(review_id)
    .fetch_all(&pool)
    .await
    .expect("fetch review_agents");

    assert_eq!(
        rows.len(),
        6,
        "expected 6 review_agent rows, got {}",
        rows.len()
    );

    // Every row has verifier_status='pass'.
    for (role_str, output, vstatus) in &rows {
        assert_eq!(
            vstatus, "pass",
            "row {role_str} expected verifier_status=pass, got {vstatus}; output={output}"
        );
    }

    // FP6 A2: exactly one shared specialist input artifact row per review.
    let input_rows: Vec<(Value,)> =
        sqlx::query_as("select artifact from review_inputs where review_id = $1")
            .bind(review_id)
            .fetch_all(&pool)
            .await
            .expect("fetch review_inputs");
    assert_eq!(
        input_rows.len(),
        1,
        "expected exactly 1 review_inputs row, got {}",
        input_rows.len()
    );
    assert!(
        !input_rows[0].0.is_null(),
        "review_inputs.artifact is null"
    );

    // Meta-reviewer row: output deserializes as MetaReview. The meta input
    // is no longer persisted (FP6 A1) — it's synthesised at render time from
    // the specialist `output` rows.
    let meta_row = rows
        .iter()
        .find(|(r, _, _)| r == "meta_reviewer")
        .expect("meta_reviewer row");
    let _meta: MetaReview =
        serde_json::from_value(meta_row.1.clone()).expect("meta output is a valid MetaReview");
    let synth_specialists: std::collections::HashMap<String, Value> = rows
        .iter()
        .filter(|(r, _, _)| r != "meta_reviewer")
        .map(|(r, output, _)| (r.clone(), output.clone()))
        .collect();
    for slug in [
        "summary",
        "technical_correctness",
        "novelty",
        "reproducibility",
        "citation",
    ] {
        assert!(
            synth_specialists.contains_key(slug),
            "synthesised meta input is missing specialists.{slug}"
        );
    }

    let bundle_path = format!("artifacts/{review_id}/bundle.zip");
    let bundle_bytes = std::fs::read(&bundle_path).expect("bundle.zip was rendered to disk");
    let cursor = std::io::Cursor::new(bundle_bytes);
    let mut zip = zip::ZipArchive::new(cursor).expect("bundle.zip is a valid zip");
    for slug in [
        "summary",
        "technical_correctness",
        "novelty",
        "reproducibility",
        "citation",
        "meta_reviewer",
    ] {
        let name = format!("agents/{slug}.json");
        let mut file = zip
            .by_name(&name)
            .unwrap_or_else(|_| panic!("bundle.zip is missing persisted agent artifact {name}"));
        let mut body = String::new();
        file.read_to_string(&mut body)
            .expect("agent artifact is UTF-8 JSON");
        let artifact: Value = serde_json::from_str(&body).expect("agent artifact is JSON");
        assert_eq!(
            artifact.get("role").and_then(|v| v.as_str()),
            Some(slug),
            "agent artifact {name} records its role"
        );
        assert!(
            artifact.get("input_artifact").is_some(),
            "agent artifact {name} includes the persisted input_artifact"
        );
    }

    // Cleanup: drop the synthetic paper (cascade nukes review + agents).
    let _ = sqlx::query("delete from papers where id = $1")
        .bind(paper_id)
        .execute(&pool)
        .await;
}
