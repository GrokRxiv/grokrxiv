use std::path::PathBuf;
use std::time::Duration;

use super::jobs::{exp_backoff, is_retryable};
use super::merge_facts::{
    merge_citation_verifier_into_output, merge_novelty_facts_into_output,
    merge_reproducibility_facts_into_output,
};
use super::prompts::{
    debug_prompt_root, dump_debug_prompt, render_agent_user_prompt, render_meta_synthesis_prompt,
    render_system_prompt, ReviewPromptFacts,
};
use super::rendering::render_to_disk;
use super::verification::{
    meta_failure_output, role_status_label, specialist_failure_output,
    specialist_failure_verifier_result, validate_role_output_after_merge, verifier_status_mark,
    verify_artifact,
};
use super::{MAX_RETRIES, MIN_SPECIALIST_QUORUM};
use crate::agents::grokrxiv_agent_context;
use crate::cli_status::StatusMark;
use crate::state::AppState;
use agenthero_dag_runtime::{DagManifest, DagNodeKind};
use serde_json::json;
use uuid::Uuid;

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_one_paper_full(state: &AppState, arxiv_id: &str) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    tracing::info!(arxiv_id, "M1: ingest start");
    crate::cli_status::emit_stage(1, 6, "Fetch", StatusMark::Run, "arXiv source and metadata");

    // With storage enabled, staged ingest persists review_input.json and the
    // review path uses that artifact as its source of truth.
    let (paper_id, extract);
    #[cfg(feature = "grokrxiv-storage")]
    {
        let opts = ingest_options_from_env();
        crate::cli_status::emit_stage(
            2,
            6,
            "Extract",
            StatusMark::Run,
            "staged extraction pipeline",
        );
        match crate::ingest_pipeline::run_ingest_pipeline(state, arxiv_id, &opts).await {
            Ok(out) => {
                paper_id = out.paper_id;
                extract = out.extract;
            }
            Err(e) => {
                tracing::warn!(arxiv_id, err = %format!("{e:#}"), "staged ingest pipeline failed; falling back to deterministic-only path");
                let pe = {
                    let _permit = state.arxiv.acquire().await;
                    grokrxiv_ingest::pipeline::ingest(arxiv_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
                };
                paper_id = crate::db::upsert_paper(pool, &pe, None).await?;
                extract = pe;
            }
        }
    }
    #[cfg(not(feature = "grokrxiv-storage"))]
    {
        let pe = {
            let _permit = state.arxiv.acquire().await;
            grokrxiv_ingest::pipeline::ingest(arxiv_id)
                .await
                .map_err(|e| anyhow::anyhow!("ingest: {e}"))?
        };
        paper_id = crate::db::upsert_paper(pool, &pe, None).await?;
        extract = pe;
    }
    ensure_extraction_completeness(&extract)?;
    tracing::info!(arxiv_id, %paper_id, "M1: paper persisted");
    crate::cli_status::emit_stage(2, 6, "Extract", StatusMark::Ok, "paper artifacts persisted");
    crate::cli_status::emit_stage(
        3,
        6,
        "Review DAG",
        StatusMark::Run,
        "starting specialist reviewers",
    );

    run_review_dag_from_state(state, pool, paper_id, extract).await
}

#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
pub(super) fn ingest_options_from_env() -> crate::ingest_pipeline::IngestOptions {
    crate::ingest_pipeline::IngestOptions::from_env()
}

/// Drive the review DAG for a paper row that is already present in the database.

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_from_state(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    run_review_dag_from_state_with_context(state, pool, paper_id, extract, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_from_state_with_context(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
    let provider = state
        .providers
        .as_ref()
        .map(|registry| registry.default.clone());
    run_review_dag_inner_with_context(state, pool, provider, paper_id, extract, submission).await
}

// CLI runner overrides are passed through environment variables before review dispatch.
// Format:
//   AGENTHERO_RUNNER_OVERRIDE        = "cli" | "api"
//   AGENTHERO_RUNNER_OVERRIDE_<ROLE> = same enum, per role
#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn review_runner_override_for(role: &str) -> Option<crate::agents::AgentRunnerKind> {
    use crate::agents::AgentRunnerKind;

    let per_role_var = format!(
        "AGENTHERO_RUNNER_OVERRIDE_{}",
        crate::runtime_config::role_env_suffix(role)
    );
    std::env::var(&per_role_var)
        .ok()
        .or_else(|| std::env::var("AGENTHERO_RUNNER_OVERRIDE").ok())
        .and_then(|s| match s.as_str() {
            "api" => Some(AgentRunnerKind::Api),
            "cli" => Some(AgentRunnerKind::Cli),
            _ => None,
        })
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn review_cache_disabled() -> bool {
    matches!(
        std::env::var("GROKRXIV_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    ) || matches!(
        std::env::var("GROKRXIV_INGEST_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    )
}

#[cfg(feature = "grokrxiv-ingest")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtractionCompletenessGate {
    review_ready: bool,
    body_chars: usize,
    section_count: usize,
    failures: Vec<String>,
    warnings: Vec<String>,
}

#[cfg(feature = "grokrxiv-ingest")]
fn extraction_completeness_gate(
    extract: &grokrxiv_schemas::PaperExtract,
) -> ExtractionCompletenessGate {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let section_count = extract.sections.len();
    let body_chars = extract
        .sections
        .iter()
        .map(|section| section.heading.chars().count() + section.body_markdown.chars().count())
        .sum::<usize>();

    if extract.title.trim().is_empty() {
        failures.push("metadata title is empty".to_string());
    }
    if extract.abstract_.trim().is_empty() {
        failures.push("metadata abstract is empty".to_string());
    }
    if section_count == 0 {
        failures.push("extraction completeness failed: no body sections".to_string());
    }
    if body_chars < 1_000 {
        failures.push(format!(
            "extraction completeness failed: body text is too small for review context ({body_chars} chars)"
        ));
    }
    if extract.bibliography.is_empty() {
        warnings.push("bibliography is empty".to_string());
    }

    ExtractionCompletenessGate {
        review_ready: failures.is_empty(),
        body_chars,
        section_count,
        failures,
        warnings,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
fn ensure_extraction_completeness(extract: &grokrxiv_schemas::PaperExtract) -> anyhow::Result<()> {
    let gate = extraction_completeness_gate(extract);
    if gate.review_ready {
        return Ok(());
    }
    let failure_summary = gate.failures.join("; ");
    tracing::warn!(
        arxiv_id = %extract.arxiv_id,
        body_chars = gate.body_chars,
        section_count = gate.section_count,
        failures = %failure_summary,
        "review: extraction completeness gate failed"
    );
    crate::cli_status::emit_stage(
        2,
        6,
        "Extract",
        StatusMark::Fail,
        "extraction completeness failed",
    );
    anyhow::bail!(
        "extraction completeness gate failed for {}: {}",
        extract.arxiv_id,
        failure_summary
    )
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_with_job_tracking(
    state: &AppState,
    paper_id: Uuid,
    job_id: Uuid,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let mut attempt = 0;
    loop {
        crate::db::mark_running(pool, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("mark review job running: {e}"))?;
        match run_review_for_paper_full(state, paper_id).await {
            Ok(review_id) => {
                crate::db::mark_done(pool, job_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("mark review job done: {e}"))?;
                return Ok(review_id);
            }
            Err(e) if attempt + 1 < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = exp_backoff(attempt);
                tracing::warn!(
                    %job_id,
                    %paper_id,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    err = %format!("{e:#}"),
                    "blocking review failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                let error = format!("{e:#}");
                crate::db::mark_failed(pool, job_id, &error)
                    .await
                    .map_err(|mark_err| {
                        anyhow::anyhow!(
                            "mark review job failed: {mark_err}; original error: {error}"
                        )
                    })?;
                return Err(e);
            }
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_extract_with_job_tracking(
    state: &AppState,
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    job_id: Uuid,
) -> anyhow::Result<Uuid> {
    let mut attempt = 0;
    loop {
        crate::db::mark_running(pool, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("mark review job running: {e}"))?;
        match run_review_dag_from_state(state, pool, paper_id, extract.clone()).await {
            Ok(review_id) => {
                crate::db::mark_done(pool, job_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("mark review job done: {e}"))?;
                return Ok(review_id);
            }
            Err(e) if attempt + 1 < MAX_RETRIES && is_retryable(&e) => {
                attempt += 1;
                let delay = exp_backoff(attempt);
                tracing::warn!(
                    %job_id,
                    %paper_id,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    err = %format!("{e:#}"),
                    "blocking extract review failed; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                let error = format!("{e:#}");
                crate::db::mark_failed(pool, job_id, &error)
                    .await
                    .map_err(|mark_err| {
                        anyhow::anyhow!(
                            "mark review job failed: {mark_err}; original error: {error}"
                        )
                    })?;
                return Err(e);
            }
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn specialist_review_concurrency_limit(roles: &[String]) -> usize {
    use crate::agents::AgentRunnerKind;

    let max = roles.len().max(1);
    let has_cli_role = roles.iter().any(|role| {
        review_runner_override_for(role).unwrap_or(AgentRunnerKind::Cli) == AgentRunnerKind::Cli
    });
    review_concurrency_limit_from(
        std::env::var("GROKRXIV_REVIEW_CONCURRENCY").ok().as_deref(),
        has_cli_role,
        max,
    )
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn review_concurrency_limit_from(
    raw: Option<&str>,
    _has_cli_role: bool,
    max: usize,
) -> usize {
    let max = max.max(1);
    if let Some(parsed) = raw.and_then(|s| s.trim().parse::<usize>().ok()) {
        return parsed.clamp(1, max);
    }
    max
}

#[cfg(feature = "grokrxiv-ingest")]
#[derive(Debug, Clone)]
struct ReviewDagRuntimeConfig {
    node_count: usize,
    layers: Vec<Vec<String>>,
    specialist_roles: Vec<String>,
    synthesizer_role: String,
    min_specialist_quorum: usize,
}

#[cfg(feature = "grokrxiv-ingest")]
fn load_review_dag_runtime_config() -> anyhow::Result<ReviewDagRuntimeConfig> {
    let manifest_path = review_dag_manifest_path();
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|e| anyhow::anyhow!("validate {}: {e}", manifest_path.display()))?;
    let layers = manifest.execution_layers()?;
    let mut specialist_roles = Vec::new();
    let mut synthesizer_role = None;

    for node in &manifest.nodes {
        let Some(role_id) = node.role.as_ref().map(|role| role.as_str()) else {
            continue;
        };
        if node.kind == DagNodeKind::Agent && node.feeds_meta {
            specialist_roles.push(role_id.to_string());
        } else if node.kind == DagNodeKind::Synthesizer {
            synthesizer_role = Some(role_id.to_string());
        }
    }

    if specialist_roles.is_empty() {
        anyhow::bail!("paper-review DAG must define at least one agent node with feeds_meta=true");
    }
    let synthesizer_role = synthesizer_role
        .ok_or_else(|| anyhow::anyhow!("paper-review DAG must define a synthesizer node"))?;

    let manifest_min_quorum = manifest
        .nodes
        .iter()
        .find(|node| node.kind == DagNodeKind::Gate)
        .and_then(|node| node.gate.as_ref())
        .and_then(|gate| gate.min_usable)
        .map(|value| value as usize)
        .unwrap_or(MIN_SPECIALIST_QUORUM);
    let min_specialist_quorum = manifest_min_quorum.min(specialist_roles.len()).max(1);
    Ok(ReviewDagRuntimeConfig {
        node_count: manifest.nodes.len(),
        layers,
        specialist_roles,
        synthesizer_role,
        min_specialist_quorum,
    })
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_dag_manifest_path() -> PathBuf {
    crate::agents::config::dag_manifest_path("paper-review")
}

#[cfg(all(test, feature = "grokrxiv-ingest"))]
mod tests {
    use super::*;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn review_dag_runtime_config_uses_manifest_gate_min_usable() {
        let _env_lock = env_test_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("paper-review.yaml"),
            r#"
id: paper-review
version: 1
accepts: [critic, synthesizer]
roles:
  - id: summary
    kind: critic
    config: agents/paper-review/summary.yaml
  - id: technical_correctness
    kind: critic
    config: agents/paper-review/technical_correctness.yaml
  - id: novelty
    kind: critic
    config: agents/paper-review/novelty.yaml
  - id: reproducibility
    kind: critic
    config: agents/paper-review/reproducibility.yaml
  - id: citation
    kind: critic
    config: agents/paper-review/citation.yaml
  - id: meta_reviewer
    kind: synthesizer
    config: agents/paper-review/meta_reviewer.yaml
nodes:
  - id: summary
    kind: agent
    role: summary
    feeds_meta: true
  - id: technical_correctness
    kind: agent
    role: technical_correctness
    feeds_meta: true
  - id: novelty
    kind: agent
    role: novelty
    feeds_meta: true
  - id: reproducibility
    kind: agent
    role: reproducibility
    feeds_meta: true
  - id: citation
    kind: agent
    role: citation
    feeds_meta: true
  - id: specialist_quorum
    kind: gate
    gate:
      min_usable: 4
      sources: [summary, technical_correctness, novelty, reproducibility, citation]
  - id: meta_reviewer
    kind: synthesizer
    role: meta_reviewer
edges:
  - from: [summary, technical_correctness, novelty, reproducibility, citation]
    to: specialist_quorum
  - from: specialist_quorum
    to: meta_reviewer
"#,
        )
        .expect("write manifest");
        std::fs::write(
            dir.path().join("review-loop.yaml"),
            r#"
id: review-loop
version: 1
accepts: []
nodes: []
edges: []
"#,
        )
        .expect("write review-loop manifest");
        let _guard = EnvGuard::set("AGENTHERO_DAGS_DIR", dir.path().to_str().unwrap());

        let cfg = load_review_dag_runtime_config().expect("runtime config");

        assert_eq!(cfg.min_specialist_quorum, 4);
    }

    #[test]
    fn extraction_completeness_gate_rejects_empty_review_context() {
        let extract = grokrxiv_schemas::PaperExtract {
            arxiv_id: "2606.00799".to_string(),
            title: "A Weyl Geometry Test Paper".to_string(),
            authors: vec![],
            abstract_: "The abstract is present, but the extracted body is empty.".to_string(),
            field: Some("math-ph".to_string()),
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: Some("tex".to_string()),
        };

        let gate = extraction_completeness_gate(&extract);

        assert!(!gate.review_ready);
        assert_eq!(gate.body_chars, 0);
        assert!(gate
            .failures
            .iter()
            .any(|msg| msg.contains("body sections")));
        assert!(gate.failures.iter().any(|msg| msg.contains("body text")));
    }

    #[test]
    fn extraction_completeness_gate_accepts_substantive_sections() {
        let extract = grokrxiv_schemas::PaperExtract {
            arxiv_id: "2407.07620".to_string(),
            title: "An Elementary Bertrand Postulate Proof".to_string(),
            authors: vec![],
            abstract_: "We give a complete proof with explicit lemmas.".to_string(),
            field: Some("math.NT".to_string()),
            sections: vec![grokrxiv_schemas::Section {
                heading: "Main theorem".to_string(),
                body_markdown: "Theorem. ".repeat(140),
            }],
            figures: vec![],
            bibliography: vec![],
            source_format: Some("tex".to_string()),
        };

        let gate = extraction_completeness_gate(&extract);

        assert!(gate.review_ready, "{:?}", gate.failures);
        assert!(gate.body_chars >= 1_000);
        assert_eq!(gate.section_count, 1);
    }

    #[test]
    fn review_verifier_timeout_uses_citation_default_and_role_override() {
        let _env_lock = env_test_lock();
        let _role_guard = EnvGuard::set("GROKRXIV_CITATION_VERIFIER_TIMEOUT_SECS", "7");
        let _global_guard = EnvGuard::set("GROKRXIV_REVIEW_VERIFIER_TIMEOUT_SECS", "99");

        assert_eq!(review_verifier_timeout_secs("citation"), 7);
        assert_eq!(review_verifier_timeout_secs("summary"), 99);
    }

    #[test]
    fn review_agent_supervisor_timeout_caps_long_retry_budgets() {
        let _env_lock = env_test_lock();
        let _guard = EnvGuard::set("GROKRXIV_REVIEW_AGENT_SUPERVISOR_TIMEOUT_SECS", "45");

        assert_eq!(
            review_agent_supervisor_timeout_secs("technical_correctness", 600, 2),
            45
        );
        assert_eq!(review_agent_supervisor_timeout_secs("summary", 10, 1), 20);
        assert_eq!(review_agent_supervisor_timeout_secs("citation", 360, 2), 45);
    }

    #[test]
    fn review_agent_supervisor_default_cap_allows_citation_sonnet() {
        assert_eq!(default_review_agent_supervisor_cap_secs("citation"), 1200);
        assert_eq!(
            default_review_agent_supervisor_cap_secs("technical_correctness"),
            900
        );
        assert_eq!(default_review_agent_supervisor_cap_secs("novelty"), 420);
        assert_eq!(
            default_review_agent_supervisor_cap_secs("meta_reviewer"),
            420
        );
        assert_eq!(default_review_agent_supervisor_cap_secs("summary"), 180);
    }

    #[test]
    fn review_agent_supervisor_timeout_role_override_wins() {
        let _env_lock = env_test_lock();
        let _role_guard = EnvGuard::set("GROKRXIV_NOVELTY_SUPERVISOR_TIMEOUT_SECS", "77");
        let _global_guard = EnvGuard::set("GROKRXIV_REVIEW_AGENT_SUPERVISOR_TIMEOUT_SECS", "45");

        assert_eq!(review_agent_supervisor_timeout_secs("novelty", 360, 2), 77);
    }

    #[test]
    fn review_verifier_timeout_notes_are_auditable_for_citation() {
        let notes = review_verifier_timeout_notes("citation", 11);

        assert_eq!(notes["citation_existence"]["status"], "warn");
        assert_eq!(
            notes["citation_existence"]["notes"]["coverage_status"],
            "timeout"
        );
        assert_eq!(notes["verifier_timeout"]["notes"]["timeout_secs"], 11);
    }

    #[test]
    fn review_citation_existence_is_deferred_by_default_and_env_gated() {
        let _env_lock = env_test_lock();
        let _guard = EnvGuard::unset(REVIEW_CITATION_EXISTENCE_ENV);

        assert!(!review_citation_existence_enabled());

        let _enabled = EnvGuard::set(REVIEW_CITATION_EXISTENCE_ENV, "true");
        assert!(review_citation_existence_enabled());
    }

    #[test]
    fn review_citation_existence_deferred_notes_are_auditable() {
        let notes = review_citation_existence_deferred_notes();

        assert_eq!(notes["citation_existence"]["status"], "warn");
        assert_eq!(
            notes["citation_existence"]["notes"]["coverage_status"],
            "deferred"
        );
        assert_eq!(
            notes["citation_existence"]["notes"]["enable_env"],
            REVIEW_CITATION_EXISTENCE_ENV
        );
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_inner(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: Option<std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    run_review_dag_inner_with_context(state, pool, provider, paper_id, extract, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
#[derive(Debug, Clone)]
pub(super) struct ReviewSubmissionContext {
    /// Account user that requested the review, if it came from the web app.
    pub submitted_by: Option<Uuid>,
    /// Review visibility to persist on the `reviews` row.
    pub visibility: String,
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_dag_inner_with_context(
    state: &AppState,
    pool: &sqlx::PgPool,
    _provider: Option<std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
    use crate::agents::{AgentInput, AgentRunner, AgentRunnerKind, ConfiguredAgent};
    use grokrxiv_schemas::{MetaReview, VerifierStatus};
    use serde_json::json;
    use std::sync::Arc;

    ensure_extraction_completeness(&extract)?;

    let resolve_agent = |role: &str| -> anyhow::Result<(
        Arc<ConfiguredAgent>,
        Arc<dyn AgentRunner>,
        String,
        AgentRunnerKind,
    )> {
        let agent = state
            .agents
            .get(role)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("DAG role `{role}` has no configured agent"))?;
        let model = agent.spec().model.clone();
        // Runtime override beats YAML's runner: field for this run.
        let runner_kind = review_runner_override_for(role).unwrap_or(agent.spec().runner);
        let runner = state
            .runners
            .get(&runner_kind)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("runner {runner_kind:?} not registered"))?;
        tracing::info!(
            role,
            model = %model,
            runner = ?runner_kind,
            "review agent resolved"
        );
        eprintln!("agent_config role={role} model={model} runner={runner_kind:?}");
        Ok((agent, runner, model, runner_kind))
    };

    let review_dag = load_review_dag_runtime_config()?;
    tracing::debug!(
        nodes = review_dag.node_count,
        layers = review_dag.layers.len(),
        "review: loaded paper-review DAG manifest"
    );

    // Pre-create the review row. `models_used` records the per-role model so
    // the moderation UI + the m1-pipeline `distinct model` assertion can show
    // which model each specialist used.
    let mut models_used_map = serde_json::Map::new();
    for role in review_dag
        .specialist_roles
        .iter()
        .chain(std::iter::once(&review_dag.synthesizer_role))
    {
        let model = resolve_agent(role)?.2;
        models_used_map.insert(role.clone(), json!(model));
    }
    let models_used = serde_json::Value::Object(models_used_map);
    let review_id = match submission.as_ref() {
        Some(context) => {
            crate::db::insert_review_with_submission(
                pool,
                paper_id,
                models_used,
                None,
                context.submitted_by,
                &context.visibility,
            )
            .await?
        }
        None => crate::db::insert_review(pool, paper_id, models_used, None).await?,
    };
    tracing::info!(%review_id, "M1: review row created");
    crate::cli_status::emit(format!("review_id={review_id}"));

    // Drive the DAG inside an inner async block so any error path can
    // transition the review row off the stale `awaiting_moderation` state.
    // Fatal DAG/runtime failures are terminal but distinct from moderator
    // withdrawal so operators can remediate and retry explicitly.
    let dag_result: anyhow::Result<()> = async {
    // This function still owns the concrete review-stage behavior, but the
    // executable specialist set comes from the DAG manifest instead of the old
    // canonical Rust topology.
    let specialist_roles = review_dag.specialist_roles.clone();
    let min_specialist_quorum = review_dag.min_specialist_quorum;

    let review_concurrency = specialist_review_concurrency_limit(&specialist_roles);
    crate::cli_status::emit_stage(
        3,
        6,
        "Review DAG",
        StatusMark::Run,
        &format!("{review_concurrency} specialist reviewers"),
    );
    let sem = Arc::new(tokio::sync::Semaphore::new(review_concurrency));
    let extract_arc = Arc::new(extract);
    let specialist_input: serde_json::Value =
        serde_json::to_value(extract_arc.as_ref()).unwrap_or_else(|_| json!({}));

    // Persist the shared specialist input artifact exactly once per review.
    crate::db::insert_review_input(pool, review_id, paper_id, &specialist_input).await?;

    // Hash the exact specialist input bytes so cache lookups match what each
    // role reasoned over.
    let specialist_content_hash =
        sha256_hex(&serde_json::to_vec(&specialist_input).unwrap_or_default());

    // Surface moderator notes from the latest request-changes pass.
    let moderator_notes: Option<String> = crate::db::fetch_latest_changes_request_notes(pool, paper_id)
        .await
        .unwrap_or(None);

    // Gather deterministic facts before specialist prompts so agents can use
    // verifier-side provenance instead of relying only on model memory.
    let (reproducibility_facts, novelty_facts) = tokio::join!(
        crate::agents::review::facts::gather_reproducibility_facts(&state.http, extract_arc.as_ref()),
        crate::agents::review::facts::gather_novelty_facts(&state.http, extract_arc.as_ref()),
    );
    let tc_facts = crate::agents::review::facts::gather_tc_facts(extract_arc.as_ref());
    tracing::info!(
        %paper_id,
        %review_id,
        urls_checked = reproducibility_facts.urls_checked.len(),
        urls_reachable = reproducibility_facts.urls_checked.iter().filter(|u| u.reachable).count(),
        github_repos = reproducibility_facts.github_repos.len(),
        related_papers = novelty_facts.related_papers.len(),
        novelty_retrieval_error = %novelty_facts.retrieval_error,
        tc_tables = tc_facts.tables.len(),
        tc_equation_labels = tc_facts.equation_labels.len(),
        tc_complexity_mentions = tc_facts.complexity_mentions.len(),
        "review: gathered reproducibility + novelty + TC facts"
    );
    if let Some(notes) = moderator_notes.as_deref() {
        tracing::info!(
            %paper_id,
            %review_id,
            notes_len = notes.len(),
            "review: surfacing moderator notes from prior changes-requested round"
        );
    }

    // Debug prompt dumps are best-effort and never fail the review.
    let debug_root = debug_prompt_root();
    let skip_review_cache = review_cache_disabled();

    let mut handles = Vec::with_capacity(specialist_roles.len());
    for role in specialist_roles.iter().cloned() {
        let cfg = state
            .agent_configs
            .get(&role)
            .ok_or_else(|| anyhow::anyhow!("missing YAML config for DAG role `{role}`"))?;
        let prompt = render_agent_user_prompt(
            &role,
            cfg,
            extract_arc.as_ref(),
            ReviewPromptFacts {
                moderator_notes: moderator_notes.as_deref(),
                reproducibility: Some(&reproducibility_facts),
                novelty: Some(&novelty_facts),
                technical: Some(&tc_facts),
            },
        );
        if let Some(root) = debug_root.as_deref() {
            dump_debug_prompt(root, &extract_arc.arxiv_id, &role, &prompt);
        }
        let system = render_system_prompt(&role, cfg, extract_arc.field.as_deref());
        let (agent, runner, role_model, role_runner) = resolve_agent(&role)?;
        let sem = sem.clone();
        let pool_cloned = pool.clone();
        let cache_hash = specialist_content_hash.clone();
        let specialist_input_cloned = specialist_input.clone();
        let role_for_task = role.clone();
        handles.push((role, role_model, role_runner, tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore alive");
            crate::cli_status::emit_detail(role_status_label(&role_for_task), StatusMark::Run, "starting");

            // Only passed verifier rows are reused from cache.
            if !skip_review_cache {
                if let Ok(Some(hit)) =
                    crate::db::lookup_cache(
                        &pool_cloned,
                        paper_id,
                        "paper-review",
                        &role_for_task,
                        &cache_hash,
                    )
                    .await
                {
                    if hit.verifier_status == "pass" {
                        tracing::info!(
                            event = "cache",
                            role = %role_for_task,
                            hit = true,
                            "cache hit"
                        );
                        return anyhow::Ok((
                            role_for_task,
                            hit.output,
                            Some(hit.tokens_in.unwrap_or(0) as i32),
                            Some(hit.tokens_out.unwrap_or(0) as i32),
                            0i32,
                            hit.model,
                            hit.runner,
                            true,
                            None::<String>,
                        ));
                    }
                }
            } else {
                tracing::info!(
                    event = "cache",
                    role = %role_for_task,
                    disabled = true,
                    "cache bypassed"
                );
            }
            tracing::info!(
                event = "cache",
                role = %role_for_task,
                hit = false,
                "cache miss"
            );

            let input = AgentInput {
                context: grokrxiv_agent_context(paper_id, review_id),
                role: role_for_task.clone(),
                content_hash_material: specialist_input_cloned.clone(),
                artifact: specialist_input_cloned,
                system_prompt: system,
                user_prompt: prompt,
                source_bundle_path: None,
            };
            let run =
                run_agent_with_supervisor_timeout(agent.as_ref(), runner.as_ref(), input).await?;
            anyhow::Ok((
                role_for_task,
                run.output,
                run.tokens_in,
                run.tokens_out,
                run.latency_ms,
                run.model,
                run.runner,
                false,
                None::<String>,
            ))
        })));
    }

    let mut specialist_results: Vec<(
        String,
        serde_json::Value,
        Option<i32>,
        Option<i32>,
        i32,
        String, // model actually used
        AgentRunnerKind,
        bool,   // cache hit
        Option<String>, // specialist execution failure reason
    )> = Vec::with_capacity(specialist_roles.len());
    for (role, role_model, role_runner, h) in handles {
        match h.await {
            Ok(Ok(result)) => specialist_results.push(result),
            Ok(Err(e)) => {
                let error = format!("{e:#}");
                tracing::warn!(
                    %review_id,
                    role = %role,
                    err = %error,
                    "specialist reviewer failed; recording failed verifier output"
                );
                crate::cli_status::emit_detail(role_status_label(&role), StatusMark::Fail, &error);
                specialist_results.push((
                    role.clone(),
                    specialist_failure_output(&role, &error),
                    None,
                    None,
                    0i32,
                    role_model,
                    role_runner,
                    false,
                    Some(error),
                ));
            }
            Err(e) => {
                let error = format!("specialist join: {e}");
                tracing::warn!(
                    %review_id,
                    role = %role,
                    err = %error,
                    "specialist reviewer task failed; recording failed verifier output"
                );
                crate::cli_status::emit_detail(role_status_label(&role), StatusMark::Fail, &error);
                specialist_results.push((
                    role.clone(),
                    specialist_failure_output(&role, &error),
                    None,
                    None,
                    0i32,
                    role_model,
                    role_runner,
                    false,
                    Some(error),
                ));
            }
        }
    }
    crate::cli_status::emit_stage(4, 6, "Verify", StatusMark::Run, "verifier ladder");

    // Persist and verify each specialist output, then capture verifier status
    // for the quorum gate before meta-review synthesis.
    let mut specialist_verifier_status: Vec<(String, Option<VerifierStatus>)> =
        Vec::with_capacity(specialist_results.len());
    for (
        role,
        output,
        tokens_in,
        tokens_out,
        latency_ms,
        used_model,
        used_runner,
        cache_hit,
        execution_failure,
    ) in &specialist_results
    {
        let (mut v_status, mut v_notes) =
            verify_artifact_with_review_timeout(state, &extract_arc, role, output).await;
        if let Some(error) = execution_failure.as_deref() {
            (v_status, v_notes) = specialist_failure_verifier_result(role, error, v_notes);
        }
        let cfg = state
            .agent_configs
            .get(role)
            .ok_or_else(|| anyhow::anyhow!("missing YAML config for DAG role `{role}`"))?;
        let mut output_to_persist = apply_agent_postprocessors(
            cfg,
            output.clone(),
            v_notes.as_ref(),
            &reproducibility_facts,
            &novelty_facts,
        )?;
        // Citation reviewers routinely emit bibliographic `year` as a numeric string
        // ("2008"); the schema requires integer|null. Coerce so a formatting quirk never
        // marks the whole review system_failed at post-merge validation.
        if role == "citation" {
            coerce_year_strings(&mut output_to_persist);
        }
        #[cfg(feature = "grokrxiv-verifier")]
        if !matches!(v_status, Some(VerifierStatus::Fail)) {
            validate_role_output_after_merge(role, &output_to_persist, &state.agent_schemas)?;
        }
        crate::db::insert_review_agent(
            pool,
            crate::db::ReviewAgentInsert {
                review_id,
                dag_type: "paper-review".to_string(),
                role: role.clone(),
                node_id: Some(role.clone()),
                agent_type: Some("critic".to_string()),
                node_kind: Some("agent".to_string()),
                runner: *used_runner,
                model: used_model,
                output: output_to_persist,
                verifier_status: v_status,
                verifier_notes: v_notes.clone(),
                tokens_in: *tokens_in,
                tokens_out: *tokens_out,
                latency_ms: Some(*latency_ms),
            },
        )
        .await?;

        // Cache only fresh successful outputs.
        if !*cache_hit && v_status == Some(VerifierStatus::Pass) {
            let _ = crate::db::insert_cache(
                pool,
                paper_id,
                "paper-review",
                role,
                &specialist_content_hash,
                output,
                "pass",
                used_model,
                *used_runner,
                *tokens_in,
                *tokens_out,
            )
            .await;
        }
        specialist_verifier_status.push((role.clone(), v_status));
        crate::cli_status::emit_detail(
            role_status_label(role),
            verifier_status_mark(v_status),
            "",
        );
        tracing::info!(role = %role, latency_ms, model = %used_model, cache_hit, "M1: specialist persisted");
    }

    // The review gate decides whether the specialist set is usable for
    // meta-review synthesis.
    let specialist_gate = crate::review_gate::SpecialistGate::evaluate_required_roles(
        &specialist_roles,
        &specialist_verifier_status,
        min_specialist_quorum,
    );
    let revision_source_hint = revision_target_source_path_hint(pool, paper_id, &extract_arc).await;
    if !specialist_gate.meta_can_run {
        let error = format!(
            "verifier quorum not met: only {} of {} specialists produced usable output (need >= {})",
            specialist_gate.usable_roles.len(),
            specialist_gate.expected_total,
            specialist_gate.min_usable,
        );
        let synthetic_meta = json!({
            "summary": "Automated review gate failed before meta-review synthesis because too few specialist outputs passed verifier checks.",
            "strengths": [],
            "weaknesses": [
                error,
                format!("Roles without usable verifier output: {}", specialist_gate.blocked_roles.join(", ")),
            ],
            "questions": [
                "Please address the verifier failures and resubmit corrections for automated re-review.",
            ],
            "recommendation": "major_revision",
            "confidence": 1.0,
            "gate": {
                "name": "specialist_verifier_quorum",
                "usable_roles": specialist_gate.usable_roles.clone(),
                "blocked_roles": specialist_gate.blocked_roles.clone(),
                "warning_roles": specialist_gate.warning_roles.clone(),
                "min_quorum": specialist_gate.min_usable,
            },
            "revision_targets": []
        });
        crate::db::set_review_meta_review(pool, review_id, &synthetic_meta).await?;

        let failure = crate::github_feedback::gate_failure_from_meta(
            review_id,
            "major_revision",
            Some(&synthetic_meta),
        );
        let _ = crate::github_feedback::record_gate_failure(state, review_id, &failure).await;
        let _ = crate::db::insert_review_event(
            pool,
            Some(review_id),
            Some(paper_id),
            "automated_gate_failed",
            "specialist_verifier_quorum",
            &synthetic_meta,
            None,
        )
        .await;
        tracing::warn!(
            %review_id,
            usable = specialist_gate.usable_roles.len(),
            quorum = min_specialist_quorum,
            "specialist quorum not met; recorded major_revision gate failure"
        );
        crate::cli_status::emit_detail(
            "meta reviewer",
            StatusMark::Fail,
            "specialist quorum not met",
        );
    } else {

    // Meta-review synthesis receives only specialist outputs keyed by role slug.
    let mut specialists_map = serde_json::Map::new();
    for (role, output, _ti, _to, _lat, _model, _runner, _cache_hit, _failure) in
        &specialist_results
    {
        specialists_map.insert(role.clone(), output.clone());
    }
    let meta_input = json!({
        "specialists": serde_json::Value::Object(specialists_map),
    });
    let meta_role = review_dag.synthesizer_role.clone();
    let meta_cfg = state
        .agent_configs
        .get(&meta_role)
        .ok_or_else(|| anyhow::anyhow!("missing YAML config for DAG role `{meta_role}`"))?;
    let meta_prompt = render_meta_synthesis_prompt(meta_cfg, &meta_input);
    if let Some(root) = debug_root.as_deref() {
        dump_debug_prompt(
            root,
            &extract_arc.arxiv_id,
            &meta_role,
            &meta_prompt,
        );
    }
    let meta_system = render_system_prompt(&meta_role, meta_cfg, extract_arc.field.as_deref());

    let (meta_agent, meta_runner, meta_model_used, meta_runner_used) =
        resolve_agent(&meta_role)?;
    crate::cli_status::emit_detail(role_status_label(&meta_role), StatusMark::Run, "synthesis");

    // Meta-review cache keys on the specialist-output bundle.
    let meta_content_hash =
        sha256_hex(&serde_json::to_vec(&meta_input).unwrap_or_default());
    let mut meta_from_cache = false;
    let (
        meta_value,
        meta_tokens_in,
        meta_tokens_out,
        meta_latency_ms,
        meta_model_recorded,
        meta_runner_recorded,
    ) =
        match if skip_review_cache {
            Ok(None)
        } else {
            crate::db::lookup_cache(
                pool,
                paper_id,
                "paper-review",
                &meta_role,
                &meta_content_hash,
            )
                .await
        } {
            Ok(Some(hit)) if hit.verifier_status == "pass" => {
                tracing::info!(
                    event = "cache",
                    role = %meta_role,
                    hit = true,
                    "cache hit"
                );
                meta_from_cache = true;
                (
                    hit.output,
                    Some(hit.tokens_in.unwrap_or(0) as i32),
                    Some(hit.tokens_out.unwrap_or(0) as i32),
                    0i32,
                    hit.model,
                    hit.runner,
                )
            }
            _ => {
                if skip_review_cache {
                    tracing::info!(
                        event = "cache",
                        role = %meta_role,
                        disabled = true,
                        "cache bypassed"
                    );
                }
                tracing::info!(
                    event = "cache",
                    role = %meta_role,
                    hit = false,
                    "cache miss"
                );
                let meta_agent_input = AgentInput {
                    context: grokrxiv_agent_context(paper_id, review_id),
                    role: meta_role.clone(),
                    content_hash_material: meta_input.clone(),
                    artifact: meta_input.clone(),
                    system_prompt: meta_system,
                    user_prompt: meta_prompt,
                    source_bundle_path: None,
                };
                match run_agent_with_supervisor_timeout(
                    meta_agent.as_ref(),
                    meta_runner.as_ref(),
                    meta_agent_input,
                )
                .await
                {
                    Ok(run) => (
                        run.output,
                        run.tokens_in,
                        run.tokens_out,
                        run.latency_ms,
                        run.model,
                        run.runner,
                    ),
                    Err(e) => {
                        let error = format!("{e:#}");
                        tracing::warn!(
                            %review_id,
                            err = %error,
                            "meta reviewer failed; recording major_revision gate output"
                        );
                        crate::cli_status::emit_detail(role_status_label(&meta_role), StatusMark::Fail, &error);
                        (
                            meta_failure_output(&error),
                            None,
                            None,
                            0i32,
                            meta_model_used.clone(),
                            meta_runner_used,
                        )
                    }
                }
            }
        };

    let meta_value = crate::revision_targets::enrich_meta_review(
        meta_value,
        &meta_input,
        revision_source_hint.as_deref(),
    );

    let (meta_v_status, meta_v_notes) =
        verify_artifact_with_review_timeout(state, &extract_arc, &meta_role, &meta_value).await;
    crate::db::insert_review_agent(
        pool,
        crate::db::ReviewAgentInsert {
            review_id,
            dag_type: "paper-review".to_string(),
            role: meta_role.clone(),
            node_id: Some(meta_role.clone()),
            agent_type: Some("synthesizer".to_string()),
            node_kind: Some("synthesizer".to_string()),
            runner: meta_runner_recorded,
            model: &meta_model_recorded,
            output: meta_value.clone(),
            verifier_status: meta_v_status,
            verifier_notes: meta_v_notes.clone(),
            tokens_in: meta_tokens_in,
            tokens_out: meta_tokens_out,
            latency_ms: Some(meta_latency_ms),
        },
    )
    .await?;
    crate::cli_status::emit_detail(
        role_status_label(&meta_role),
        verifier_status_mark(meta_v_status),
        "",
    );

    // Cache only fresh successful meta-reviews.
    if !meta_from_cache && meta_v_status == Some(VerifierStatus::Pass) {
        let _ = crate::db::insert_cache(
            pool,
            paper_id,
            "paper-review",
            &meta_role,
            &meta_content_hash,
            &meta_value,
            "pass",
            &meta_model_recorded,
            meta_runner_recorded,
            meta_tokens_in,
            meta_tokens_out,
        )
        .await;
    }
    tracing::info!(meta_latency_ms, model = %meta_model_recorded, cache_hit = meta_from_cache, "M1: meta-reviewer persisted");

    // Stash the synthesized meta_review JSON on the reviews row. If parsing
    // into the typed `MetaReview` fails we still persist the raw JSON so the
    // moderator can inspect what the model produced.
    let _ = serde_json::from_value::<MetaReview>(meta_value.clone());
    crate::db::set_review_meta_review(pool, review_id, &meta_value).await?;
    }

    // Render artifacts from persisted review and agent rows.
    let _ = paper_id; // not needed by the new render path
    crate::cli_status::emit_stage(5, 6, "Render", StatusMark::Run, "review artifacts");
    if let Err(e) = render_to_disk(state, review_id).await {
        tracing::warn!(%review_id, err = %e, "render_to_disk failed");
        crate::cli_status::emit_stage(
            5,
            6,
            "Render",
            StatusMark::Warn,
            &format!("review artifacts: {e:#}"),
        );
    } else {
        crate::cli_status::emit_stage(5, 6, "Render", StatusMark::Ok, "review artifacts written");
    }

        Ok(())
    }
    .await;

    if let Err(e) = dag_result {
        let failure_message = format!("{e:#}");
        tracing::error!(
            %review_id,
            err = %failure_message,
            "review DAG bailed; transitioning review row to system_failed"
        );
        let _ = crate::db::set_review_system_failed(
            pool,
            review_id,
            "review_dag_failed",
            &failure_message,
            true,
        )
        .await;
        crate::cli_status::emit_stage(
            6,
            6,
            "Moderation",
            StatusMark::Fail,
            "review marked system_failed after DAG failure",
        );
        return Err(e);
    }

    // Only completed reviews enter the moderation queue. Failed DAG runs move
    // to `system_failed` above and must not remain actionable in admin review.
    let _ = crate::db::insert_moderation_pending(pool, review_id).await;
    crate::cli_status::emit_stage(6, 6, "Moderation", StatusMark::Ok, "awaiting moderation");
    crate::cli_status::emit(format!("next: agenthero grokrxiv show {review_id}"));
    Ok(review_id)
}

/// Hex-encoded SHA-256 of the input bytes for review-output cache keys.

#[cfg(feature = "grokrxiv-ingest")]
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_full(
    state: &AppState,
    paper_id: Uuid,
) -> anyhow::Result<Uuid> {
    run_review_for_paper_full_with_context(state, paper_id, None).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_review_for_paper_full_with_context(
    state: &AppState,
    paper_id: Uuid,
    submission: Option<ReviewSubmissionContext>,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    // Reload the paper row's data; the ingest crate is the canonical source so
    // we round-trip through it for the fields the DAG needs.
    let row = crate::db::load_paper_review_seed(pool, paper_id)
        .await
        .map_err(|e| anyhow::anyhow!("load paper row: {e}"))?;
    let crate::db::PaperReviewSeedRow {
        arxiv_id,
        title,
        abstract_,
        field,
        submitted_date: _submitted,
    } = row;

    if let Some(extract) = load_latest_review_input_extract(pool, paper_id).await? {
        tracing::info!(
            %paper_id,
            arxiv_id = %extract.arxiv_id,
            "review: loaded extract from latest persisted review_input"
        );
        return run_review_dag_from_state_with_context(state, pool, paper_id, extract, submission)
            .await;
    }

    // Prefer persisted review_input.json when staged extraction already produced it.
    #[cfg(feature = "grokrxiv-storage")]
    {
        if let Ok(Some(assets)) = crate::db::read_paper_assets(pool, paper_id).await {
            if matches!(assets.extraction_status, crate::db::ExtractionStatus::Ready) {
                if let Some(git_path) = assets.git_path.as_deref() {
                    let repo_root: std::path::PathBuf = std::env::var("GROKRXIV_DATA_REPO_PATH")
                        .ok()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| {
                            std::path::PathBuf::from(
                                "/Users/mlong/Documents/Development/grokrxiv-data",
                            )
                        });
                    let ri_path = repo_root.join(git_path).join("review_input.json");
                    if let Ok(bytes) = std::fs::read(&ri_path) {
                        if let Ok(ri) =
                            serde_json::from_slice::<grokrxiv_storage::ReviewInput>(&bytes)
                        {
                            match crate::ingest_pipeline::load_paper_extract(&repo_root, &ri) {
                                Ok(extract) => {
                                    tracing::info!(
                                        %paper_id,
                                        arxiv_id,
                                        git_path,
                                        "review: loaded extract from cached review_input.json"
                                    );
                                    return run_review_dag_from_state_with_context(
                                        state, pool, paper_id, extract, submission,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!(arxiv_id, err = %format!("{e:#}"), "review_input.json present but load_paper_extract failed; re-ingesting");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // The DAG's call sites only need title/abstract/field/arxiv_id — sections
    // and bibliography are nice-to-have. Re-ingest to get them when possible;
    // fall back to a minimal extract on transient arXiv failure so a single
    // network blip doesn't tank a queued review.
    let extract = match grokrxiv_ingest::pipeline::ingest(&arxiv_id).await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(arxiv_id, err = %e, "review: re-ingest failed, using DB-only fields");
            grokrxiv_schemas::PaperExtract {
                arxiv_id,
                title,
                authors: vec![],
                abstract_: abstract_.unwrap_or_default(),
                field,
                sections: vec![],
                figures: vec![],
                bibliography: vec![],
                source_format: None,
            }
        }
    };

    run_review_dag_from_state_with_context(state, pool, paper_id, extract, submission).await
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn load_latest_review_input_extract(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
) -> anyhow::Result<Option<grokrxiv_schemas::PaperExtract>> {
    let Some(artifact) = crate::db::load_latest_review_input_artifact(pool, paper_id).await? else {
        return Ok(None);
    };
    match serde_json::from_value::<grokrxiv_schemas::PaperExtract>(artifact) {
        Ok(extract) => Ok(Some(extract)),
        Err(e) => {
            tracing::warn!(%paper_id, err = %e, "review: latest review_input is not a PaperExtract");
            Ok(None)
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn revision_target_source_path_hint(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    extract: &grokrxiv_schemas::PaperExtract,
) -> Option<String> {
    let row: Option<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
        "select coalesce(source_kind, 'arxiv'), source_id, source_metadata \
         from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some((source_kind, source_id, metadata)) = row else {
        return fallback_source_path_hint(extract);
    };
    if let Some(path) = metadata
        .get("correction_source_path")
        .and_then(|v| v.as_str())
    {
        if !path.trim().is_empty() {
            return Some(path.to_string());
        }
    }
    let adapter = metadata.get("adapter").unwrap_or(&serde_json::Value::Null);
    let raw_source_path = match source_kind.as_str() {
        "git_repo" => adapter.get("paper_path").and_then(|v| v.as_str()),
        "local_file" => adapter.get("path").and_then(|v| v.as_str()),
        "arxiv" => return correction_repo_path_hint(source_id.as_deref(), extract),
        _ => None,
    };
    raw_source_path
        .and_then(|path| correction_repo_path_from_raw(source_id.as_deref(), path))
        .or_else(|| raw_source_path.map(str::to_string))
        .or_else(|| fallback_source_path_hint(extract))
}

#[cfg(feature = "grokrxiv-ingest")]
fn correction_repo_path_hint(
    source_id: Option<&str>,
    extract: &grokrxiv_schemas::PaperExtract,
) -> Option<String> {
    let default_name = match extract.source_format.as_deref() {
        Some("tex") => "paper.tex",
        Some("pdf") => "paper.pdf",
        _ => return fallback_source_path_hint(extract),
    };
    correction_repo_path_from_raw(source_id, default_name)
        .or_else(|| Some(default_name.to_string()))
}

#[cfg(feature = "grokrxiv-ingest")]
fn correction_repo_path_from_raw(source_id: Option<&str>, raw_path: &str) -> Option<String> {
    let source_id = source_id?.trim();
    if source_id.is_empty() {
        return None;
    }
    let safe_source_id: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let file_name = std::path::Path::new(raw_path)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())?;
    Some(format!("corrections/{safe_source_id}/{file_name}"))
}

#[cfg(feature = "grokrxiv-ingest")]
fn fallback_source_path_hint(extract: &grokrxiv_schemas::PaperExtract) -> Option<String> {
    match extract.source_format.as_deref() {
        Some("tex") => Some("paper.tex".to_string()),
        Some("pdf") => Some("paper.pdf".to_string()),
        _ => None,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
fn apply_agent_postprocessors(
    cfg: &crate::agents::config::AgentConfig,
    mut output: serde_json::Value,
    verifier_notes: Option<&serde_json::Value>,
    reproducibility_facts: &crate::agents::review::facts::ReproducibilityFacts,
    novelty_facts: &crate::agents::review::facts::NoveltyFacts,
) -> anyhow::Result<serde_json::Value> {
    for postprocessor in &cfg.postprocessors {
        output = match postprocessor.as_str() {
            "merge_citation_verifier" => {
                merge_citation_verifier_into_output(output, verifier_notes)
            }
            "merge_reproducibility_facts" => {
                merge_reproducibility_facts_into_output(output, reproducibility_facts)
            }
            "merge_novelty_facts" => merge_novelty_facts_into_output(output, novelty_facts),
            other => anyhow::bail!("unknown agent postprocessor `{other}`"),
        };
    }
    Ok(output)
}

/// Recursively coerce every `"year"` field from a numeric string to an integer
/// (and blank / non-numeric to null). LLM citation reviewers frequently emit
/// `year: "2008"` while the citation schema requires `["integer", "null"]`; a
/// single such string would otherwise fail post-merge schema validation and
/// mark the entire review `system_failed`.
fn coerce_year_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "year" {
                    if let serde_json::Value::String(text) = child {
                        *child = text
                            .trim()
                            .parse::<i64>()
                            .ok()
                            .map(serde_json::Value::from)
                            .unwrap_or(serde_json::Value::Null);
                    }
                } else {
                    coerce_year_strings(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                coerce_year_strings(item);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "grokrxiv-ingest")]
pub(super) async fn run_agent_with_supervisor_timeout(
    agent: &crate::agents::ConfiguredAgent,
    runner: &dyn crate::agents::AgentRunner,
    input: crate::agents::AgentInput,
) -> anyhow::Result<crate::agents::AgentRun> {
    let role = input.role.clone();
    let spec = agent.spec();
    let timeout_secs =
        review_agent_supervisor_timeout_secs(&role, spec.timeout_secs, u32::from(spec.max_retries));
    let timeout_duration = Duration::from_secs(timeout_secs);
    tokio::time::timeout(timeout_duration, agent.run(runner, input))
        .await
        .map_err(|_| {
            anyhow::anyhow!("agent {role:?} timed out after {timeout_secs}s at supervisor level")
        })?
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_agent_supervisor_timeout_secs(role: &str, timeout_secs: u32, max_retries: u32) -> u64 {
    let configured = u64::from(timeout_secs.max(1))
        .saturating_mul(u64::from(max_retries).saturating_add(1))
        .max(1);
    let role_key = format!(
        "GROKRXIV_{}_SUPERVISOR_TIMEOUT_SECS",
        crate::runtime_config::role_env_suffix(role)
    );
    if let Some(role_override) = timeout_env_u64(&role_key) {
        return role_override;
    }
    let cap = timeout_env_u64("GROKRXIV_REVIEW_AGENT_SUPERVISOR_TIMEOUT_SECS")
        .unwrap_or_else(|| default_review_agent_supervisor_cap_secs(role));
    configured.min(cap.max(1))
}

#[cfg(feature = "grokrxiv-ingest")]
fn default_review_agent_supervisor_cap_secs(role: &str) -> u64 {
    match role {
        "technical_correctness" => 900,
        "novelty" | "meta_reviewer" => 420,
        "citation" => 1200,
        _ => 180,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn verify_artifact_with_review_timeout(
    state: &AppState,
    extract: &grokrxiv_schemas::PaperExtract,
    role: &str,
    artifact: &serde_json::Value,
) -> (
    Option<grokrxiv_schemas::VerifierStatus>,
    Option<serde_json::Value>,
) {
    if role == "citation" && !review_citation_existence_enabled() {
        return verify_citation_artifact_without_live_existence(state, extract, role, artifact)
            .await;
    }

    let timeout_secs = review_verifier_timeout_secs(role);
    match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        verify_artifact(state, extract, role, artifact),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                role,
                timeout_secs,
                "review verifier exceeded watchdog timeout"
            );
            (
                Some(grokrxiv_schemas::VerifierStatus::Warn),
                Some(review_verifier_timeout_notes(role, timeout_secs)),
            )
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
const REVIEW_CITATION_EXISTENCE_ENV: &str = "GROKRXIV_REVIEW_CITATION_EXISTENCE";

#[cfg(feature = "grokrxiv-ingest")]
fn review_citation_existence_enabled() -> bool {
    std::env::var(REVIEW_CITATION_EXISTENCE_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(feature = "grokrxiv-ingest")]
async fn verify_citation_artifact_without_live_existence(
    _state: &AppState,
    _extract: &grokrxiv_schemas::PaperExtract,
    role: &str,
    _artifact: &serde_json::Value,
) -> (
    Option<grokrxiv_schemas::VerifierStatus>,
    Option<serde_json::Value>,
) {
    tracing::info!(
        role,
        "review citation existence verifier deferred from blocking review path"
    );
    (
        Some(grokrxiv_schemas::VerifierStatus::Warn),
        Some(review_citation_existence_deferred_notes()),
    )
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_citation_existence_deferred_notes() -> serde_json::Value {
    json!({
        "citation_existence": {
            "status": "warn",
            "notes": {
                "checked": 0,
                "coverage_status": "deferred",
                "reason": "Live citation existence verification is deferred from the blocking review/PR path; run the citation validation DAG for full resolver coverage.",
                "enable_env": REVIEW_CITATION_EXISTENCE_ENV,
                "entries": []
            }
        }
    })
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_verifier_timeout_secs(role: &str) -> u64 {
    let role_key = format!(
        "GROKRXIV_{}_VERIFIER_TIMEOUT_SECS",
        crate::runtime_config::role_env_suffix(role)
    );
    timeout_env_u64(&role_key)
        .or_else(|| timeout_env_u64("GROKRXIV_REVIEW_VERIFIER_TIMEOUT_SECS"))
        .unwrap_or_else(|| if role == "citation" { 45 } else { 60 })
        .max(1)
}

#[cfg(feature = "grokrxiv-ingest")]
fn timeout_env_u64(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
}

#[cfg(feature = "grokrxiv-ingest")]
fn review_verifier_timeout_notes(role: &str, timeout_secs: u64) -> serde_json::Value {
    let reason = format!("review verifier exceeded {timeout_secs}s watchdog timeout");
    if role == "citation" {
        json!({
            "citation_existence": {
                "status": "warn",
                "notes": {
                    "checked": 0,
                    "coverage_status": "timeout",
                    "reason": reason,
                    "entries": []
                }
            },
            "verifier_timeout": {
                "status": "warn",
                "notes": {
                    "role": role,
                    "timeout_secs": timeout_secs,
                    "reason": reason
                }
            }
        })
    } else {
        json!({
            "verifier_timeout": {
                "status": "warn",
                "notes": {
                    "role": role,
                    "timeout_secs": timeout_secs,
                    "reason": reason
                }
            }
        })
    }
}
