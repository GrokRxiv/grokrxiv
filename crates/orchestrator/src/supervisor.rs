//! Job supervisor.
//!
//! A lightweight tokio-based dispatcher that owns mpsc channels for each
//! [`JobKind`]. The supervisor review pipeline ends at
//! `status = awaiting_moderation`; publishing requires explicit admin approval
//! through the `/admin/reviews/:id/approve` endpoint, which calls
//! [`Supervisor::publish_after_approval`].

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use grokrxiv_schemas::JobKind;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::state::AppState;

mod jobs;
mod merge_facts;
mod prompts;
mod publish;
mod rendering;
mod review_flow;
mod verification;

#[cfg(feature = "grokrxiv-ingest")]
pub use publish::apply_revisions;
#[cfg(feature = "grokrxiv-publisher")]
pub use publish::spawn_publish_reconcile;
pub use rendering::render_to_disk;

use jobs::{run_item, supervisor_queue_capacity, supervisor_worker_limit};

#[cfg(test)]
use jobs::{exp_backoff, supervisor_queue_capacity_from, supervisor_worker_limit_from};
#[cfg(test)]
use merge_facts::{
    merge_citation_verifier_into_output, merge_novelty_facts_into_output,
    merge_reproducibility_facts_into_output,
};
#[cfg(all(test, feature = "grokrxiv-ingest"))]
use prompts::{body_budget_chars, build_specialist_prompt};
#[cfg(test)]
use prompts::{build_meta_synthesis_prompt, is_code_amenable_field, role_system_prompt};
#[cfg(all(test, feature = "grokrxiv-publisher"))]
use publish::{
    real_pr_url, reconcile_published_reviews_with, PublishFinalizer, PublishPrLookup,
    PublishReconcileStats,
};
#[cfg(all(test, feature = "grokrxiv-ingest"))]
use review_flow::{review_concurrency_limit_from, run_agent_with_supervisor_timeout};
#[cfg(test)]
use verification::{meta_failure_output, specialist_failure_output};

/// Single in-flight unit of work.
#[derive(Debug, Clone)]
pub struct WorkItem {
    /// Database job id.
    pub job_id: Uuid,
    /// What to do.
    pub kind: JobKind,
    /// Entity reference for jobs that already have a persisted row.
    pub ref_id: Option<Uuid>,
    /// Job payload. Ingest jobs carry `{ "arxiv_id": "<id>" }`.
    pub payload: serde_json::Value,
    /// Attempt counter, where `0` is the first attempt.
    pub attempt: u32,
}

/// Maximum retry attempts for any single job.
pub const MAX_RETRIES: u32 = 3;

/// Minimum number of usable specialist outputs required before meta-review synthesis.
pub const MIN_SPECIALIST_QUORUM: usize = crate::review_dag::DEFAULT_MIN_SPECIALIST_QUORUM;

/// In-memory supervisor handle.
#[derive(Clone)]
pub struct Supervisor {
    tx: mpsc::Sender<WorkItem>,
    shutdown_tx: watch::Sender<bool>,
    publish_inflight: Arc<Mutex<HashSet<Uuid>>>,
}

impl Supervisor {
    /// Spawn the supervisor task and return a handle for enqueueing work.
    pub fn spawn(state: AppState) -> Self {
        let queue_capacity = supervisor_queue_capacity();
        let worker_limit = supervisor_worker_limit();
        let (tx, mut rx) = mpsc::channel::<WorkItem>(queue_capacity);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let publish_inflight = Arc::new(Mutex::new(HashSet::new()));
        let me = Self {
            tx: tx.clone(),
            shutdown_tx,
            publish_inflight: publish_inflight.clone(),
        };
        let state2 = state;
        tokio::spawn(async move {
            tracing::info!(queue_capacity, worker_limit, "supervisor started");
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            tracing::info!("supervisor shutdown requested; closing work queue");
                            rx.close();
                            break;
                        }
                    }
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                        if let Err(e) = result {
                            tracing::error!(err = %e, "supervisor worker task panicked");
                        }
                    }
                    Some(item) = rx.recv(), if tasks.len() < worker_limit => {
                        let state = state2.clone();
                        let retry_tx = tx.clone();
                        let publish_inflight = publish_inflight.clone();
                        tasks.spawn(async move {
                            let result = run_item(&state, &item, &retry_tx).await;
                            if matches!(item.kind, JobKind::Publish) {
                                if let Some(review_id) = item.ref_id {
                                    publish_inflight.lock().unwrap().remove(&review_id);
                                }
                            }
                            if let Err(e) = result {
                                tracing::error!(
                                    job_id = %item.job_id,
                                    attempt = item.attempt,
                                    err = %e,
                                    "job failed"
                                );
                            }
                        });
                    }
                    else => break,
                }
            }
            while let Some(result) = tasks.join_next().await {
                if let Err(e) = result {
                    tracing::error!(err = %e, "supervisor worker task panicked during drain");
                }
            }
        });
        me
    }

    /// Enqueue a unit of work.
    pub async fn enqueue(&self, item: WorkItem) -> anyhow::Result<()> {
        if *self.shutdown_tx.borrow() {
            anyhow::bail!("supervisor is shutting down");
        }
        self.tx
            .send(item)
            .await
            .map_err(|e| anyhow::anyhow!("supervisor channel closed: {e}"))
    }

    /// Borrow the underlying sender so other runtime components can enqueue work.
    pub fn sender(&self) -> mpsc::Sender<WorkItem> {
        self.tx.clone()
    }

    /// Stop accepting new work and let already-spawned worker tasks finish.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Start the publish step after moderator approval.
    pub async fn publish_after_approval(&self, review_id: Uuid) -> anyhow::Result<()> {
        {
            let mut inflight = self.publish_inflight.lock().unwrap();
            if !inflight.insert(review_id) {
                tracing::info!(
                    %review_id,
                    "publish_after_approval: publish job already in flight"
                );
                return Ok(());
            }
        }
        let job = WorkItem {
            job_id: Uuid::new_v4(),
            kind: JobKind::Publish,
            ref_id: Some(review_id),
            payload: serde_json::Value::Null,
            attempt: 0,
        };
        let result = self.enqueue(job).await;
        if result.is_err() {
            self.publish_inflight.lock().unwrap().remove(&review_id);
        }
        result
    }
}

/// Drive a single paper through ingest and review synchronously.
pub async fn run_one_paper_blocking(
    _supervisor: &Supervisor,
    state: &AppState,
    arxiv_id: &str,
) -> anyhow::Result<Uuid> {
    #[cfg(feature = "grokrxiv-ingest")]
    {
        review_flow::run_one_paper_full(state, arxiv_id).await
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = state;
        let _ = arxiv_id;
        anyhow::bail!(
            "run_one_paper_blocking requires --features full (grokrxiv-ingest \
             + grokrxiv-render). Rebuild with: cargo run --release -p \
             grokrxiv-orchestrator -- ingest <ARXIV_ID>"
        );
    }
}

/// Drive the review DAG for a paper row that is already present in the database.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_for_paper_blocking(
    state: &AppState,
    paper_id: Uuid,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let job_id = crate::db::create_job(pool, JobKind::Review, Some(paper_id)).await?;
    review_flow::run_review_for_paper_with_job_tracking(state, paper_id, job_id).await
}

/// Drive the review DAG for a paper row using a caller-supplied extract.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_for_extract_blocking(
    state: &AppState,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let job_id = crate::db::create_job(pool, JobKind::Review, Some(paper_id)).await?;
    review_flow::run_review_for_extract_with_job_tracking(state, pool, paper_id, extract, job_id)
        .await
}

/// Drive the review DAG for a paper row with an explicit provider.
#[cfg(feature = "grokrxiv-ingest")]
pub async fn run_review_dag(
    state: &AppState,
    pool: &sqlx::PgPool,
    provider: std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>,
    paper_id: Uuid,
    extract: grokrxiv_schemas::PaperExtract,
) -> anyhow::Result<Uuid> {
    review_flow::run_review_dag_inner(state, pool, Some(provider), paper_id, extract).await
}

fn role_slug(role: grokrxiv_schemas::AgentRole) -> &'static str {
    crate::review_dag::role_slug(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "grokrxiv-ingest")]
    struct NeverCompletesRunner;

    #[cfg(feature = "grokrxiv-ingest")]
    #[async_trait::async_trait]
    impl crate::agents::AgentRunner for NeverCompletesRunner {
        fn name(&self) -> &'static str {
            "never-completes"
        }

        async fn run(
            &self,
            spec: &crate::agents::AgentSpec,
            _input: &crate::agents::AgentInput,
        ) -> anyhow::Result<crate::agents::AgentRun> {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(crate::agents::AgentRun {
                role: spec.role,
                runner: crate::agents::AgentRunnerKind::Cli,
                model: spec.model.clone(),
                output: serde_json::json!({}),
                verifier_status: None,
                verifier_notes: None,
                tokens_in: None,
                tokens_out: None,
                latency_ms: 0,
                cache_hit: false,
                sandbox_ref: None,
            })
        }
    }

    #[cfg(feature = "grokrxiv-ingest")]
    struct SlowCompletesRunner;

    #[cfg(feature = "grokrxiv-ingest")]
    #[async_trait::async_trait]
    impl crate::agents::AgentRunner for SlowCompletesRunner {
        fn name(&self) -> &'static str {
            "slow-completes"
        }

        async fn run(
            &self,
            spec: &crate::agents::AgentSpec,
            _input: &crate::agents::AgentInput,
        ) -> anyhow::Result<crate::agents::AgentRun> {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            Ok(crate::agents::AgentRun {
                role: spec.role,
                runner: crate::agents::AgentRunnerKind::Cli,
                model: spec.model.clone(),
                output: serde_json::json!({}),
                verifier_status: None,
                verifier_notes: None,
                tokens_in: None,
                tokens_out: None,
                latency_ms: 0,
                cache_hit: false,
                sandbox_ref: None,
            })
        }
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[tokio::test]
    async fn supervisor_times_out_wedged_agent_execution() {
        use crate::agents::{
            AgentInput, AgentRunnerKind, AgentSpec, ConfiguredAgent, SandboxPolicy,
        };
        use grokrxiv_schemas::AgentRole;
        use std::sync::Arc;

        let spec = AgentSpec {
            role: AgentRole::Summary,
            runner: AgentRunnerKind::Cli,
            sandbox: SandboxPolicy::None,
            provider: "claude".to_string(),
            model: "fake-model".to_string(),
            schema: std::sync::Arc::new(serde_json::json!({ "type": "object" })),
            max_retries: 0,
            timeout_secs: 1,
        };
        let input = AgentInput {
            paper_id: Uuid::nil(),
            review_id: Uuid::nil(),
            role: AgentRole::Summary,
            content_hash_material: serde_json::json!({}),
            artifact: serde_json::json!({}),
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            source_bundle_path: None,
        };
        let agent = Arc::new(ConfiguredAgent::new(spec));
        let runner = Arc::new(NeverCompletesRunner);

        let err = run_agent_with_supervisor_timeout(agent.as_ref(), runner.as_ref(), input)
            .await
            .expect_err("wedged runner should time out at supervisor level");

        assert!(
            err.to_string().contains("timed out"),
            "expected timeout error, got: {err:#}"
        );
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[tokio::test]
    async fn supervisor_timeout_allows_configured_retry_budget() {
        use crate::agents::{
            AgentInput, AgentRunnerKind, AgentSpec, ConfiguredAgent, SandboxPolicy,
        };
        use grokrxiv_schemas::AgentRole;
        use std::sync::Arc;

        let spec = AgentSpec {
            role: AgentRole::Citation,
            runner: AgentRunnerKind::Cli,
            sandbox: SandboxPolicy::None,
            provider: "gemini".to_string(),
            model: "fake-model".to_string(),
            schema: std::sync::Arc::new(serde_json::json!({ "type": "object" })),
            max_retries: 1,
            timeout_secs: 1,
        };
        let input = AgentInput {
            paper_id: Uuid::nil(),
            review_id: Uuid::nil(),
            role: AgentRole::Citation,
            content_hash_material: serde_json::json!({}),
            artifact: serde_json::json!({}),
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            source_bundle_path: None,
        };
        let agent = Arc::new(ConfiguredAgent::new(spec));
        let runner = Arc::new(SlowCompletesRunner);

        run_agent_with_supervisor_timeout(agent.as_ref(), runner.as_ref(), input)
            .await
            .expect("supervisor timeout should include configured retry budget");
    }

    #[tokio::test]
    async fn publish_after_approval_deduplicates_inflight_review() {
        let (tx, mut rx) = mpsc::channel::<WorkItem>(4);
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let supervisor = Supervisor {
            tx,
            shutdown_tx,
            publish_inflight: Arc::new(Mutex::new(HashSet::new())),
        };
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();

        supervisor
            .publish_after_approval(review_id)
            .await
            .expect("first publish enqueue");
        supervisor
            .publish_after_approval(review_id)
            .await
            .expect("duplicate publish enqueue should be a no-op");

        let first = rx.recv().await.expect("one publish work item queued");
        assert_eq!(first.kind, JobKind::Publish);
        assert_eq!(first.ref_id, Some(review_id));
        assert!(
            rx.try_recv().is_err(),
            "duplicate approval must not enqueue a second publish job"
        );
    }

    #[cfg(feature = "grokrxiv-publisher")]
    #[test]
    fn publish_pr_url_filter_requires_real_github_pr() {
        let real = "https://github.com/GrokRxiv/grokrxiv-reviews/pull/123";
        assert_eq!(real_pr_url(Some(real)), Some(real));
        assert_eq!(
            real_pr_url(Some(
                "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-123"
            )),
            None
        );
        assert_eq!(real_pr_url(Some("https://example.com/foo/pull/123")), None);
        assert_eq!(
            real_pr_url(Some(
                "https://github.com/GrokRxiv/grokrxiv-reviews/pull/not-a-number"
            )),
            None
        );
    }

    #[cfg(feature = "grokrxiv-publisher")]
    #[tokio::test]
    async fn reconcile_published_reviews_with_fakes_skips_bad_urls_and_continues_on_errors() {
        use std::collections::{HashMap, HashSet};
        use std::sync::{Arc, Mutex};

        struct FakeLookup {
            merged: HashMap<u64, anyhow::Result<bool>>,
        }

        #[async_trait::async_trait]
        impl PublishPrLookup for FakeLookup {
            async fn is_pr_merged(
                &self,
                _owner: &str,
                _repo: &str,
                number: u64,
            ) -> anyhow::Result<bool> {
                match self.merged.get(&number) {
                    Some(Ok(value)) => Ok(*value),
                    Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                    None => Ok(false),
                }
            }
        }

        struct FakeFinalizer {
            fail: HashSet<Uuid>,
            calls: Arc<Mutex<Vec<Uuid>>>,
        }

        #[async_trait::async_trait]
        impl PublishFinalizer for FakeFinalizer {
            async fn finalize(&self, review_id: Uuid) -> anyhow::Result<bool> {
                self.calls.lock().unwrap().push(review_id);
                if self.fail.contains(&review_id) {
                    anyhow::bail!("finalize failed");
                }
                Ok(true)
            }
        }

        let merged_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let open_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let malformed_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let lookup_error_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let finalize_error_id = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let lookup = FakeLookup {
            merged: HashMap::from([
                (1, Ok(true)),
                (2, Ok(false)),
                (3, Err(anyhow::anyhow!("lookup failed"))),
                (4, Ok(true)),
            ]),
        };
        let finalizer = FakeFinalizer {
            fail: HashSet::from([finalize_error_id]),
            calls: calls.clone(),
        };

        let stats = reconcile_published_reviews_with(
            vec![
                (
                    merged_id,
                    "https://github.com/GrokRxiv/grokrxiv-reviews/pull/1".to_string(),
                ),
                (
                    open_id,
                    "https://github.com/GrokRxiv/grokrxiv-reviews/pull/2".to_string(),
                ),
                (
                    malformed_id,
                    "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-3".to_string(),
                ),
                (
                    lookup_error_id,
                    "https://github.com/GrokRxiv/grokrxiv-reviews/pull/3".to_string(),
                ),
                (
                    finalize_error_id,
                    "https://github.com/GrokRxiv/grokrxiv-reviews/pull/4".to_string(),
                ),
            ],
            &lookup,
            &finalizer,
        )
        .await;

        assert_eq!(
            stats,
            PublishReconcileStats {
                checked: 4,
                finalized: 1,
                skipped_malformed: 1,
                lookup_errors: 1,
                finalize_errors: 1,
            }
        );
        assert_eq!(*calls.lock().unwrap(), vec![merged_id, finalize_error_id]);
    }

    #[test]
    fn backoff_caps_at_30s() {
        assert!(exp_backoff(10) <= Duration::from_secs(30));
    }

    #[test]
    fn supervisor_queue_capacity_defaults_above_old_tiny_channel() {
        assert!(supervisor_queue_capacity_from(None) >= 4096);
        assert_eq!(supervisor_queue_capacity_from(Some("16")), 128);
    }

    #[test]
    fn supervisor_worker_limit_is_bounded_and_configurable() {
        assert!(supervisor_worker_limit_from(None) >= 1);
        assert_eq!(supervisor_worker_limit_from(Some("0")), 1);
        assert_eq!(supervisor_worker_limit_from(Some("7")), 7);
    }

    #[tokio::test]
    async fn supervisor_rejects_enqueue_after_shutdown() {
        let mut config = crate::Config::from_env();
        config.database_url = None;
        let state = crate::AppState::from_config(config)
            .await
            .expect("AppState builds without a database url");
        let supervisor = Supervisor::spawn(state);
        supervisor.shutdown();

        let err = supervisor
            .enqueue(WorkItem {
                job_id: Uuid::new_v4(),
                kind: JobKind::Ingest,
                ref_id: None,
                payload: serde_json::Value::Null,
                attempt: 0,
            })
            .await
            .expect_err("shutdown supervisor should reject new work");

        assert!(err.to_string().contains("shutting down"));
    }

    /// The revision_artifact schema accepts a complete artifact and rejects
    /// missing required fields.
    #[test]
    fn revision_artifact_schema_validates() {
        let schema_str = include_str!("../../../schemas/revision_artifact.schema.json");
        let schema: serde_json::Value =
            serde_json::from_str(schema_str).expect("schema parses as JSON");
        let validator =
            jsonschema::validator_for(&schema).expect("schema compiles as JSON Schema draft-07");

        // Happy path: every required field present.
        let good = serde_json::json!({
            "target": "paper_latex",
            "patches": [{
                "section": "introduction",
                "original": "We propose ...",
                "proposed":  "We introduce ...",
                "rationale": "Match the abstract's verb.",
                "confidence": 0.8,
            }],
        });
        assert!(
            validator.is_valid(&good),
            "expected valid artifact to validate"
        );

        // Bad: missing `rationale` on the patch.
        let bad = serde_json::json!({
            "target": "paper_latex",
            "patches": [{
                "section": "introduction",
                "original": "x",
                "proposed":  "y",
                "confidence": 0.5,
            }],
        });
        assert!(
            !validator.is_valid(&bad),
            "expected artifact missing rationale to fail"
        );

        // Bad: target not in the enum.
        let bad_target = serde_json::json!({
            "target": "something_else",
            "patches": [],
        });
        assert!(
            !validator.is_valid(&bad_target),
            "expected bad target to fail"
        );
    }

    /// `apply_revisions` returns a clear error when no database is configured.
    #[cfg(feature = "grokrxiv-ingest")]
    #[tokio::test]
    async fn apply_revisions_errors_without_db() {
        let mut config = crate::Config::from_env();
        config.database_url = None;
        let state = crate::AppState::from_config(config)
            .await
            .expect("AppState builds without a database url");
        let err = apply_revisions(&state, Uuid::new_v4(), vec![0])
            .await
            .expect_err("expected error when DATABASE_URL is unset");
        let msg = err.to_string();
        assert!(
            msg.contains("DATABASE_URL"),
            "error should mention missing DATABASE_URL, got: {msg}"
        );
    }

    /// The global `--mode` and `--revision-target` flags parse through clap.
    #[test]
    fn cli_parses_mode_and_revision_target_flags() {
        use crate::agents::{AgentMode, RevisionTarget};
        use crate::cli::Cli;
        use clap::Parser;

        let cli = Cli::try_parse_from([
            "grokrxiv",
            "--mode",
            "review_and_revise",
            "--revision-target",
            "paper_latex",
            "doctor",
        ])
        .expect("cli parses with review mode flags");
        assert_eq!(cli.mode, AgentMode::ReviewAndRevise);
        assert_eq!(cli.revision_target, RevisionTarget::PaperLatex);

        // Defaults exercise the value-enum default arm.
        let defaults = Cli::try_parse_from(["grokrxiv", "doctor"]).expect("defaults parse");
        assert_eq!(defaults.mode, AgentMode::ReviewOnly);
        assert_eq!(defaults.revision_target, RevisionTarget::PaperLatex);
    }

    // build_specialist_prompt fidelity tests cover body inclusion, truncation,
    // bibliography rendering, and the MetaReviewer input contract.

    #[cfg(feature = "grokrxiv-ingest")]
    fn fake_extract(
        sections: Vec<(&str, String)>,
        bibliography: Vec<&str>,
    ) -> grokrxiv_schemas::PaperExtract {
        use grokrxiv_schemas::{Citation, PaperExtract, Section};
        PaperExtract {
            arxiv_id: "test/0001".into(),
            title: "Test Title".into(),
            authors: vec![],
            abstract_: "Test abstract sentence.".into(),
            field: Some("cs.LG".into()),
            sections: sections
                .into_iter()
                .map(|(h, b)| Section {
                    heading: h.into(),
                    body_markdown: b,
                })
                .collect(),
            figures: vec![],
            bibliography: bibliography
                .into_iter()
                .map(|raw| Citation {
                    raw: raw.into(),
                    doi: None,
                    arxiv_id: None,
                    title: None,
                })
                .collect(),
            source_format: None,
        }
    }

    /// Body-review specialist prompts include section body markdown.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_includes_section_body() {
        use grokrxiv_schemas::AgentRole;
        let body = "Introductory text. ".repeat(50);
        let head_200: String = body.chars().take(200).collect();
        let extract = fake_extract(
            vec![
                ("1. Introduction", body.clone()),
                ("2. Methods", "Methods text.".to_string()),
            ],
            vec![],
        );

        for role in [
            AgentRole::Summary,
            AgentRole::TechnicalCorrectness,
            AgentRole::Novelty,
            AgentRole::Reproducibility,
        ] {
            let prompt = build_specialist_prompt(role, &extract, None, None, None, None);
            assert!(
                prompt.contains("## 1. Introduction"),
                "role {role:?}: prompt missing heading: {}",
                &prompt[..prompt.len().min(400)]
            );
            assert!(
                prompt.contains(&head_200),
                "role {role:?}: first 200 chars of body not in prompt"
            );
            assert!(
                prompt.contains("## 2. Methods"),
                "role {role:?}: second section heading missing"
            );
        }
    }

    /// Oversized paper bodies are truncated within the per-role prompt budget.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_respects_per_role_budget() {
        use grokrxiv_schemas::AgentRole;
        // Reproducibility has an 80k body budget; this fixture exceeds it.
        let big = "x".repeat(15_000);
        let extract = fake_extract(
            (0..8)
                .map(|i| {
                    (
                        // headings need 'static-ish lifetime → leak intentionally OK in tests
                        Box::leak(format!("Section {i}").into_boxed_str()) as &str,
                        big.clone(),
                    )
                })
                .collect(),
            vec![],
        );

        let budget = body_budget_chars(AgentRole::Reproducibility);
        let prompt =
            build_specialist_prompt(AgentRole::Reproducibility, &extract, None, None, None, None);

        // Sanity: actually exceeded budget with raw bodies (8 * 15_000 = 120_000 > 80_000).
        let total_raw: usize = extract.sections.iter().map(|s| s.body_markdown.len()).sum();
        assert!(total_raw > budget);

        // Either the per-section truncation marker is present, or the
        // remaining-sections footer is present (both indicate truncation
        // actually fired).
        let has_section_marker = prompt.contains("[…truncated…]");
        let has_footer_marker = prompt.contains("[…remaining sections truncated; headings:");
        assert!(
            has_section_marker || has_footer_marker,
            "expected a truncation marker in the rendered prompt"
        );

        // Body block must fit inside its budget plus a small slack for the
        // headings + truncation markers we render in this test. Use the
        // raw byte length of the rendered "Paper body:" region.
        let body_start = prompt
            .find("Paper body:\n\n")
            .expect("Paper body block present")
            + "Paper body:\n\n".len();
        let body_end = prompt.find("\nTask:").unwrap_or(prompt.len());
        let body_region_chars = prompt[body_start..body_end].chars().count();
        // Allow up to 5% slack for the trailing remaining-sections-footer line.
        let allowed = budget + (budget / 20);
        assert!(
            body_region_chars <= allowed,
            "body region {body_region_chars} chars exceeds allowed {allowed} (budget {budget})"
        );
    }

    /// Bibliography entries appear in prompts with 1-indexed synthesized keys.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_renders_bibliography() {
        use grokrxiv_schemas::AgentRole;
        let mut extract = fake_extract(
            vec![(
                "1. Introduction",
                "The result builds on prior work [@alice2020].".to_string(),
            )],
            vec!["alice2020", "Bob, A second paper, 2021."],
        );
        extract.bibliography[0].title = Some("A foundational paper".to_string());

        let prompt = build_specialist_prompt(AgentRole::Citation, &extract, None, None, None, None);
        assert!(
            prompt.contains("Bibliography (2 entries):"),
            "expected bibliography header in prompt"
        );
        assert!(
            prompt.contains("[1] alice2020 | title: A foundational paper"),
            "expected first bib entry with key and title"
        );
        assert!(
            prompt.contains("[2] Bob, A second paper, 2021."),
            "expected second bib entry with key [2]"
        );
        assert!(
            prompt.contains("Citation contexts:"),
            "expected citation contexts block"
        );
        assert!(
            prompt.contains("The result builds on prior work [@alice2020]."),
            "expected cited sentence in compact citation prompt"
        );
        assert!(
            !prompt.contains("Paper body:"),
            "citation prompt should not include the full body block"
        );
    }

    /// Large bibliography prompts must stay bounded for local CLI runners.
    /// The verifier preserves full citation existence data; the LLM relevance
    /// pass only needs a representative capped slice plus in-text contexts.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn citation_prompt_caps_large_bibliography_for_cli_runtime() {
        use grokrxiv_schemas::AgentRole;
        let bibliography = (1..=40)
            .map(|i| Box::leak(format!("Reference {i}").into_boxed_str()) as &str)
            .collect();
        let extract = fake_extract(
            vec![(
                "1. Introduction",
                "The result follows earlier work [@ref1].".to_string(),
            )],
            bibliography,
        );

        let prompt = build_specialist_prompt(AgentRole::Citation, &extract, None, None, None, None);

        assert!(
            prompt.contains("Bibliography (40 entries; showing 32):"),
            "expected capped bibliography header in prompt"
        );
        assert!(prompt.contains("[32] Reference 32"));
        assert!(
            !prompt.contains("[33] Reference 33"),
            "entry past the prompt cap should be omitted"
        );
        assert!(
            prompt.contains("additional bibliography entries omitted from citation LLM prompt"),
            "expected explicit truncation marker"
        );
        assert!(
            prompt.contains("for each bibliography entry included below"),
            "task text must not ask the CLI to classify omitted entries"
        );
    }

    /// The MetaReviewer specialist prompt is empty because it receives only
    /// specialist outputs through `build_meta_synthesis_prompt`.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn meta_reviewer_prompt_omits_paper_body() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![(
                "1. Introduction",
                "This is the introductory body markdown that must NOT leak.".into(),
            )],
            vec!["Some citation, 2020."],
        );
        let prompt =
            build_specialist_prompt(AgentRole::MetaReviewer, &extract, None, None, None, None);
        assert_eq!(prompt, "", "MetaReviewer prompt must be empty");
        assert!(!prompt.contains("introductory body markdown"));
        assert!(!prompt.contains("Bibliography"));
    }

    /// A single oversized section uses the 60/40 truncation marker.
    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn single_giant_section_uses_60_40_truncation() {
        use grokrxiv_schemas::AgentRole;
        // Summary budget = 48_000. Build a single 100_000-char section.
        let huge = "abcdefghij".repeat(10_000); // 100_000 chars
        let extract = fake_extract(vec![("1. Introduction", huge)], vec![]);
        let prompt = build_specialist_prompt(AgentRole::Summary, &extract, None, None, None, None);
        assert!(
            prompt.contains("[…truncated…]"),
            "expected the 60/40 truncation marker for single oversized section"
        );
    }

    // Proof-as-code prompt tests.

    #[test]
    fn role_system_prompt_adds_proof_as_code_for_code_amenable_field() {
        use grokrxiv_schemas::AgentRole;
        let tc = role_system_prompt(AgentRole::TechnicalCorrectness, Some("math.AG"));
        assert!(
            tc.contains("PROOF-AS-CODE AXIOM"),
            "math.* should trigger axiom for technical_correctness"
        );
        let rp = role_system_prompt(AgentRole::Reproducibility, Some("cs.LO"));
        assert!(
            rp.contains("PROOF-AS-CODE AXIOM"),
            "cs.* should trigger axiom for reproducibility"
        );
        let mr = role_system_prompt(AgentRole::MetaReviewer, Some("hep-th"));
        assert!(
            mr.contains("RECOMMENDATION GATE"),
            "hep-* should trigger gate for meta_reviewer"
        );
    }

    #[test]
    fn role_system_prompt_skips_axiom_for_non_amenable_field() {
        use grokrxiv_schemas::AgentRole;
        let tc = role_system_prompt(AgentRole::TechnicalCorrectness, Some("q-bio.GN"));
        assert!(
            !tc.contains("PROOF-AS-CODE AXIOM"),
            "q-bio.* should NOT trigger the axiom"
        );
        let mr = role_system_prompt(AgentRole::MetaReviewer, None);
        assert!(
            !mr.contains("RECOMMENDATION GATE"),
            "missing field should NOT trigger the gate"
        );
    }

    #[test]
    fn role_system_prompt_skips_axiom_for_unrelated_roles() {
        use grokrxiv_schemas::AgentRole;
        let s = role_system_prompt(AgentRole::Summary, Some("cs.LO"));
        let n = role_system_prompt(AgentRole::Novelty, Some("math.AG"));
        let c = role_system_prompt(AgentRole::Citation, Some("hep-th"));
        for p in [&s, &n, &c] {
            assert!(
                !p.contains("PROOF-AS-CODE AXIOM"),
                "axiom should only fire for TC/Reproducibility"
            );
            assert!(
                !p.contains("RECOMMENDATION GATE"),
                "gate should only fire for MetaReviewer"
            );
        }
    }

    #[test]
    fn merge_citation_verifier_into_output_keeps_verifier_facts_out_of_llm_output() {
        let llm_output = serde_json::json!({
            "entries": [
                {
                    "citation": { "key": "[1]", "raw": "Foo et al.", "title": null, "authors": [] },
                    "exists": null,
                    "resolved_doi": null,
                    "resolved_url": null,
                    "relevance": "high",
                    "notes": "Used in Section 3",
                    "explanation": "Cited as the source of Theorem 2."
                },
                {
                    "citation": { "key": "[2]", "raw": "Bar et al.", "title": null, "authors": [] },
                    "exists": null,
                    "resolved_doi": null,
                    "resolved_url": null,
                    "relevance": "medium",
                    "notes": null,
                    "explanation": "Cited in passing."
                }
            ],
            "missing_references": [],
            "summary": "LLM prose stays.",
            "confidence": 0.7
        });
        let v_notes = serde_json::json!({
            "checked": 2,
            "unresolved": ["Bar et al."],
            "entries": [
                { "raw": "Foo et al.", "exists": true,  "resolved_doi": "10.1/foo", "resolved_url": "https://doi.org/10.1/foo", "source": "crossref" },
                { "raw": "Bar et al.", "exists": false, "resolved_doi": null,       "resolved_url": null,                       "source": "none" },
                { "raw": "Baz et al.", "exists": true,  "resolved_doi": "10.1/baz", "resolved_url": "https://doi.org/10.1/baz", "source": "crossref" }
            ]
        });
        let merged = merge_citation_verifier_into_output(llm_output, Some(&v_notes));
        let entries = merged.get("entries").unwrap().as_array().unwrap();
        // Verifier facts stay in verifier_notes; they are not rewritten into
        // LLM-owned citation review fields or appended as synthetic entries.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["exists"], serde_json::Value::Null);
        assert_eq!(entries[0]["resolved_doi"], serde_json::Value::Null);
        assert_eq!(entries[0]["resolved_url"], serde_json::Value::Null);
        assert_eq!(entries[0]["relevance"], "high");
        assert_eq!(entries[0]["notes"], "Used in Section 3");
        assert_eq!(merged["summary"], "LLM prose stays.");
    }

    #[test]
    fn merge_citation_verifier_skips_error_outputs() {
        let llm_output = serde_json::json!({
            "error": "citation reviewer failed"
        });
        let v_notes = serde_json::json!({
            "entries": [
                { "raw": "Foo et al.", "exists": true, "resolved_doi": "10.1/foo", "resolved_url": "https://doi.org/10.1/foo", "source": "crossref" }
            ]
        });

        let merged = merge_citation_verifier_into_output(llm_output.clone(), Some(&v_notes));

        assert_eq!(merged, llm_output);
    }

    #[test]
    fn merge_novelty_facts_does_not_append_schema_invalid_candidates() {
        let llm_output = serde_json::json!({
            "novelty_score": 0.7,
            "related_work": [],
            "missing_prior_art": [],
            "verdict": "significant",
            "confidence": 0.8
        });
        let facts = crate::agents::specialist_facts::NoveltyFacts {
            related_papers: vec![crate::agents::specialist_facts::RelatedPaper {
                title: "Nearby Work".into(),
                abstract_snippet: Some("A nearby result.".into()),
                year: Some(2025),
                primary_author: Some("Ada".into()),
                source: "Semantic Scholar".into(),
                source_id: "s2:nearby".into(),
                url: Some("https://example.com/paper".into()),
                doi: Some("10.1/example".into()),
                arxiv_id: Some("2605.00001".into()),
            }],
            retrieval_error: String::new(),
        };

        let merged = merge_novelty_facts_into_output(llm_output.clone(), &facts);

        assert_eq!(merged, llm_output);
    }

    #[test]
    fn merge_reproducibility_facts_appends_concerns_for_dead_urls_and_archived_repos() {
        use crate::agents::specialist_facts::{
            GithubRepoFact, ReproducibilityFacts, UrlCheck, UrlKind,
        };
        let llm = serde_json::json!({
            "code_availability": "open_source",
            "code_url": "https://github.com/foo/bar",
            "data_availability": "public",
            "data_url": null,
            "environment": null,
            "concerns": [{ "area": "evaluation", "description": "no held-out test set", "severity": "minor" }],
            "reproducibility_score": 0.8,
            "confidence": 0.7
        });
        let facts = ReproducibilityFacts {
            urls_checked: vec![
                UrlCheck {
                    url: "https://github.com/foo/bar".into(),
                    reachable: false,
                    status: Some(404),
                    final_url: None,
                    kind: UrlKind::Code,
                },
                UrlCheck {
                    url: "https://zenodo.org/record/123".into(),
                    reachable: false,
                    status: Some(410),
                    final_url: None,
                    kind: UrlKind::Dataset,
                },
                UrlCheck {
                    url: "https://example.com/keep".into(),
                    reachable: true,
                    status: Some(200),
                    final_url: None,
                    kind: UrlKind::Other,
                },
            ],
            github_repos: vec![GithubRepoFact {
                owner: "foo".into(),
                repo: "bar".into(),
                exists: true,
                default_branch: Some("main".into()),
                pushed_at: Some("2020-01-01T00:00:00Z".into()),
                stargazers_count: Some(5),
                license_spdx: None,
                archived: Some(true),
            }],
        };
        let merged = merge_reproducibility_facts_into_output(llm, &facts);
        let concerns = merged["concerns"].as_array().unwrap();
        // 1 existing + 2 unreachable urls (code + dataset) + 1 archived repo = 4.
        assert_eq!(concerns.len(), 4);
        // The existing concern is preserved.
        assert_eq!(concerns[0]["description"], "no held-out test set");
        // The two unreachable URL concerns are appended at major severity.
        let dead_url_concerns: Vec<_> = concerns
            .iter()
            .filter(|c| {
                c["description"]
                    .as_str()
                    .unwrap_or("")
                    .contains("could not reach")
            })
            .collect();
        assert_eq!(dead_url_concerns.len(), 2);
        for c in &dead_url_concerns {
            assert_eq!(c["severity"], "major");
        }
        // The archived-repo concern is minor.
        let archived: Vec<_> = concerns
            .iter()
            .filter(|c| c["description"].as_str().unwrap_or("").contains("archived"))
            .collect();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0]["severity"], "minor");
    }

    #[test]
    fn merge_reproducibility_facts_dedupes_urls_and_marks_other_minor() {
        use crate::agents::specialist_facts::{ReproducibilityFacts, UrlCheck, UrlKind};
        let llm = serde_json::json!({
            "code_availability": "not_applicable",
            "code_url": null,
            "data_availability": "not_applicable",
            "data_url": null,
            "environment": null,
            "concerns": [{
                "area": "other",
                "description": "Verifier could not reach `https://example.com/dead` (status=404)",
                "severity": "minor"
            }],
            "reproducibility_score": 0.6,
            "confidence": 0.7
        });
        let facts = ReproducibilityFacts {
            urls_checked: vec![
                UrlCheck {
                    url: "https://example.com/dead".into(),
                    reachable: false,
                    status: Some(404),
                    final_url: None,
                    kind: UrlKind::Other,
                },
                UrlCheck {
                    url: "https://example.com/new-dead".into(),
                    reachable: false,
                    status: Some(404),
                    final_url: None,
                    kind: UrlKind::Other,
                },
            ],
            github_repos: vec![],
        };

        let merged = merge_reproducibility_facts_into_output(llm, &facts);
        let concerns = merged["concerns"].as_array().unwrap();

        assert_eq!(concerns.len(), 2);
        assert_eq!(concerns[1]["area"], "other");
        assert_eq!(concerns[1]["severity"], "minor");
        assert!(concerns[1]["description"]
            .as_str()
            .unwrap()
            .contains("new-dead"));
    }

    #[test]
    fn merge_citation_verifier_passes_through_when_no_notes() {
        let llm_output = serde_json::json!({
            "entries": [{ "citation": {"key":"[1]","raw":"x","title":null,"authors":[]}, "exists": false, "resolved_doi": null, "resolved_url": null, "relevance": "low", "notes": null, "explanation": "" }],
            "missing_references": [],
            "summary": "s",
            "confidence": 0.0
        });
        let merged = merge_citation_verifier_into_output(llm_output.clone(), None);
        assert_eq!(merged, llm_output);
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_renders_moderator_notes_when_present() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![("1. Intro", "Some content.".to_string())],
            vec!["[Foo23] Foo et al."],
        );
        let prompt = build_specialist_prompt(
            AgentRole::TechnicalCorrectness,
            &extract,
            Some("Please tighten the proof of Theorem 3."),
            None,
            None,
            None,
        );
        assert!(
            prompt.contains("Moderator notes from a prior `request-changes` round"),
            "moderator-notes section should be rendered"
        );
        assert!(
            prompt.contains("tighten the proof of Theorem 3"),
            "operator's notes should be embedded verbatim"
        );
    }

    #[cfg(feature = "grokrxiv-ingest")]
    #[test]
    fn specialist_prompt_omits_moderator_notes_when_absent_or_blank() {
        use grokrxiv_schemas::AgentRole;
        let extract = fake_extract(
            vec![("1. Intro", "Some content.".to_string())],
            vec!["[Foo23] Foo et al."],
        );
        let p_none =
            build_specialist_prompt(AgentRole::Reproducibility, &extract, None, None, None, None);
        assert!(
            !p_none.contains("Moderator notes"),
            "None should not emit the section"
        );
        let p_blank = build_specialist_prompt(
            AgentRole::Reproducibility,
            &extract,
            Some("  "),
            None,
            None,
            None,
        );
        assert!(
            !p_blank.contains("Moderator notes"),
            "whitespace-only should not emit the section"
        );
    }

    #[test]
    fn is_code_amenable_field_matches_expected_prefixes() {
        for f in [
            "cs.LO",
            "cs.LG",
            "math.AG",
            "hep-th",
            "hep-ph",
            "gr-qc",
            "astro-ph.CO",
            "cond-mat.str-el",
            "nlin.CD",
            "quant-ph",
            "nucl-th",
            "stat.ML",
        ] {
            assert!(
                is_code_amenable_field(f),
                "expected {f} to be code-amenable"
            );
        }
        for f in [
            "q-bio.GN",
            "q-fin.RM",
            "econ.GN",
            "eess.SP",
            "physics.med-ph",
        ] {
            assert!(
                !is_code_amenable_field(f),
                "expected {f} to NOT be code-amenable"
            );
        }
    }

    /// The meta-synthesis prompt documents only the `specialists` input key.
    #[test]
    fn meta_synthesis_prompt_does_not_document_paper_key() {
        let meta_input = serde_json::json!({
            "specialists": {
                "summary": {"tldr": "x"},
                "technical_correctness": {"claims": [], "overall_correctness": "pass", "confidence": 0.5},
                "novelty": {"verdict": "moderate", "novelty_score": 0.5, "related_work": [], "missing_prior_art": [], "confidence": 0.5},
                "reproducibility": {"reproducibility_score": 0.5, "code_availability": "present", "code_url": null, "data_availability": "present", "data_url": null, "environment": "x", "concerns": [], "confidence": 0.5},
                "citation": {"entries": [], "missing_references": [], "summary": "x", "confidence": 0.5},
            }
        });
        let prompt = build_meta_synthesis_prompt(&meta_input);

        // Guard both the explicit key name and the older two-key wording.
        assert!(
            !prompt.contains("`paper`"),
            "prompt must not document a `paper` input key, got: {prompt}"
        );
        assert!(
            !prompt.contains("two keys"),
            "prompt must say `one key`, not `two keys`, got: {prompt}"
        );
        assert!(
            prompt.contains("one key"),
            "prompt should describe the single-key shape, got: {prompt}"
        );
        assert!(
            prompt.contains("`specialists`"),
            "prompt should document the `specialists` key, got: {prompt}"
        );
    }

    /// The specialist quorum constant remains three usable outputs.
    #[test]
    fn min_specialist_quorum_is_three() {
        assert_eq!(MIN_SPECIALIST_QUORUM, 3);
    }

    #[test]
    fn review_concurrency_defaults_to_full_parallel_for_cli_roles() {
        assert_eq!(review_concurrency_limit_from(None, true, 5), 5);
    }

    #[test]
    fn review_concurrency_defaults_to_full_parallel_for_api_roles() {
        assert_eq!(review_concurrency_limit_from(None, false, 5), 5);
    }

    #[test]
    fn review_concurrency_env_override_is_clamped() {
        assert_eq!(review_concurrency_limit_from(Some("2"), true, 5), 2);
        assert_eq!(review_concurrency_limit_from(Some("99"), true, 5), 5);
        assert_eq!(review_concurrency_limit_from(Some("0"), true, 5), 1);
    }

    /// Mirrors the quorum-count predicate used before meta-review synthesis.
    fn quorum_passes(statuses: &[Option<grokrxiv_schemas::VerifierStatus>]) -> bool {
        statuses
            .iter()
            .filter(|s| {
                matches!(
                    **s,
                    Some(grokrxiv_schemas::VerifierStatus::Pass)
                        | Some(grokrxiv_schemas::VerifierStatus::Warn)
                )
            })
            .count()
            >= MIN_SPECIALIST_QUORUM
    }

    #[test]
    fn quorum_fires_when_only_two_specialists_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Fail),
            Some(VerifierStatus::Fail),
            None,
        ];
        assert!(
            !quorum_passes(&statuses),
            "quorum should NOT pass at 2-of-5; meta_reviewer must be skipped"
        );
    }

    #[test]
    fn quorum_allows_meta_when_all_five_specialists_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![Some(VerifierStatus::Pass); 5];
        assert!(quorum_passes(&statuses), "quorum should pass at 5-of-5");
    }

    #[test]
    fn quorum_allows_meta_at_exactly_three_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Warn),
            Some(VerifierStatus::Fail),
            Some(VerifierStatus::Fail),
        ];
        assert!(
            quorum_passes(&statuses),
            "quorum should pass at exactly the 3-of-5 usable threshold"
        );
    }

    #[test]
    fn quorum_allows_meta_when_all_specialists_warn() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![Some(VerifierStatus::Warn); 5];
        assert!(
            quorum_passes(&statuses),
            "warn is non-blocking and should count as usable verifier output"
        );
    }

    /// Common case: one specialist degrades (e.g., transient API hiccup);
    /// the other four pass. Meta should still run.
    #[test]
    fn quorum_allows_meta_when_four_of_five_pass() {
        use grokrxiv_schemas::VerifierStatus;
        let statuses = vec![
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Pass),
            Some(VerifierStatus::Fail),
        ];
        assert!(
            quorum_passes(&statuses),
            "4-of-5 should clear the quorum; meta runs on the surviving four"
        );
    }

    #[test]
    fn specialist_failure_output_records_role_and_error() {
        use grokrxiv_schemas::AgentRole;
        let output = specialist_failure_output(
            AgentRole::Citation,
            "CliRunner timed out after 120s for role Citation",
        );
        assert_eq!(
            output["error"],
            "CliRunner timed out after 120s for role Citation"
        );
        assert_eq!(output["role"], "citation");
        assert_eq!(output["status"], "agent_failed");
    }

    #[test]
    fn meta_failure_output_is_schema_valid_major_revision() {
        let output = meta_failure_output("`claude` exited with Some(1)");
        assert_eq!(output["recommendation"], "major_revision");
        assert!(output["weaknesses"][0]
            .as_str()
            .unwrap()
            .contains("`claude` exited"));
        serde_json::from_value::<grokrxiv_schemas::MetaReview>(output)
            .expect("synthetic meta-review should deserialize");
    }

    /// The quorum failure payload keeps its structured moderation shape.
    #[test]
    fn quorum_error_payload_is_structured() {
        let usable_roles: Vec<&'static str> = vec!["summary", "novelty"];
        let blocked_roles: Vec<&'static str> =
            vec!["technical_correctness", "reproducibility", "citation"];
        let payload = serde_json::json!({
            "summary": "Automated review gate failed before meta-review synthesis because too few specialist outputs passed verifier checks.",
            "strengths": [],
            "weaknesses": [
                format!(
                    "verifier quorum not met: only {} of 5 specialists produced usable output (need >= {})",
                    usable_roles.len(),
                    MIN_SPECIALIST_QUORUM,
                ),
                format!("Roles without usable verifier output: {}", blocked_roles.join(", ")),
            ],
            "questions": [
                "Please address the verifier failures and resubmit corrections for automated re-review.",
            ],
            "recommendation": "major_revision",
            "confidence": 1.0,
            "gate": {
                "name": "specialist_verifier_quorum",
                "usable_roles": usable_roles,
                "blocked_roles": blocked_roles,
                "min_quorum": MIN_SPECIALIST_QUORUM,
            }
        });

        let err_msg = payload
            .get("weaknesses")
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str())
            .expect("error key is a string");
        assert!(
            err_msg.starts_with("verifier quorum not met: only 2 of 5 specialists"),
            "structured error message has wrong prefix: {err_msg}"
        );
        assert!(
            err_msg.contains("need >= 3"),
            "structured error must surface the quorum threshold: {err_msg}"
        );
        assert_eq!(
            payload["recommendation"],
            serde_json::json!("major_revision")
        );
        assert_eq!(
            payload["gate"]["usable_roles"],
            serde_json::json!(["summary", "novelty"])
        );
        assert_eq!(payload["gate"]["min_quorum"], serde_json::json!(3));
    }
}
