//! Registry for DAG app adapters executable by the generic DAG executor.
//!
//! This module is orchestration glue only: it maps a manifest id to a DAG app
//! crate and runs that app through `grokrxiv-dag-executor`. App topology lives
//! in YAML manifests, and app behavior belongs in the app crates.

use std::path::PathBuf;

use grokrxiv_dag_app_c_to_rust::CToRustDagApp;
use grokrxiv_dag_app_citation_validation::CitationValidationDagApp;
use grokrxiv_dag_app_paper_extract::PaperExtractDagApp;
use grokrxiv_dag_app_paper_ingest::PaperIngestDagApp;
use grokrxiv_dag_app_paper_publish::PaperPublishDagApp;
use grokrxiv_dag_app_paper_review::PaperReviewDagApp;
use grokrxiv_dag_app_paper_revise::PaperReviseDagApp;
use grokrxiv_dag_executor::{DagApp, DagExecutionReport, DagExecutor, DagIo};
use grokrxiv_dag_runtime::DagManifest;

/// Static metadata for one registered DAG app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagAppDescriptor {
    /// DAG type id.
    pub dag_type: &'static str,
    /// Manifest file under the DAG manifests directory.
    pub manifest_file: &'static str,
    /// Crate/package that owns the app adapter.
    pub crate_name: &'static str,
}

/// Static metadata for one product app command surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredAppDescriptor {
    /// Product app id used by `grokrxiv app run <app> <action>`.
    pub id: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Actions exposed by this app.
    pub actions: &'static [AppActionDescriptor],
}

/// Static metadata for one app action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppActionDescriptor {
    /// Action id used by `grokrxiv app run <app> <action>`.
    pub id: &'static str,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: &'static str,
    /// Short operator-facing description.
    pub description: &'static str,
}

const RESEARCH_ACTIONS: &[AppActionDescriptor] = &[
    AppActionDescriptor {
        id: "extract",
        dag_type: "paper-extract",
        description: "Extract source artifacts for one or more research papers.",
    },
    AppActionDescriptor {
        id: "ingest",
        dag_type: "paper-ingest",
        description: "Ingest one or more arXiv papers and optionally moderate.",
    },
    AppActionDescriptor {
        id: "ingest-range",
        dag_type: "paper-ingest",
        description: "Backfill arXiv metadata across a date range.",
    },
    AppActionDescriptor {
        id: "ingest-daily",
        dag_type: "paper-ingest",
        description: "Run the daily research ingest scheduler tick once.",
    },
    AppActionDescriptor {
        id: "review",
        dag_type: "paper-review",
        description: "Run the research review DAG for a paper source.",
    },
    AppActionDescriptor {
        id: "review-extracted",
        dag_type: "paper-review",
        description: "Run the review DAG for an already extracted paper.",
    },
    AppActionDescriptor {
        id: "re-review",
        dag_type: "paper-review",
        description: "Re-run the review DAG against an already ingested paper.",
    },
    AppActionDescriptor {
        id: "verify",
        dag_type: "paper-review",
        description: "Re-run verifier nodes for one persisted research review.",
    },
    AppActionDescriptor {
        id: "render",
        dag_type: "paper-publish",
        description: "Re-render deterministic review artifacts.",
    },
    AppActionDescriptor {
        id: "refresh-review",
        dag_type: "paper-review",
        description: "Refresh derived review metadata without rerunning agents.",
    },
    AppActionDescriptor {
        id: "show",
        dag_type: "paper-review",
        description: "Show one persisted research review.",
    },
    AppActionDescriptor {
        id: "list",
        dag_type: "paper-review",
        description: "List persisted research sources or reviews.",
    },
    AppActionDescriptor {
        id: "open",
        dag_type: "paper-review",
        description: "Open the canonical URL for one persisted research review.",
    },
    AppActionDescriptor {
        id: "approve",
        dag_type: "paper-publish",
        description: "Approve and publish a completed research review.",
    },
    AppActionDescriptor {
        id: "request-revisions",
        dag_type: "paper-revise",
        description: "Request author revisions for a failed research review.",
    },
    AppActionDescriptor {
        id: "request-changes",
        dag_type: "paper-revise",
        description: "Request moderator changes for a queued research review.",
    },
    AppActionDescriptor {
        id: "reject",
        dag_type: "paper-review",
        description: "Reject a research review from the moderation flow.",
    },
    AppActionDescriptor {
        id: "close",
        dag_type: "paper-publish",
        description: "Hide a review from web output and optionally close its PR.",
    },
    AppActionDescriptor {
        id: "withdraw",
        dag_type: "paper-publish",
        description: "Withdraw a published research review.",
    },
    AppActionDescriptor {
        id: "correct",
        dag_type: "paper-revise",
        description: "Append a correction to a published research review.",
    },
    AppActionDescriptor {
        id: "html-review",
        dag_type: "paper-publish",
        description: "Run the HTML quality repair harness for rendered output.",
    },
    AppActionDescriptor {
        id: "feedback-loop-smoke",
        dag_type: "paper-revise",
        description: "Run the destructive GitHub correction feedback-loop smoke.",
    },
    AppActionDescriptor {
        id: "batch-create",
        dag_type: "paper-review",
        description: "Create a scheduled research review batch.",
    },
    AppActionDescriptor {
        id: "batch-run",
        dag_type: "paper-review",
        description: "Run queued items from a scheduled research review batch.",
    },
    AppActionDescriptor {
        id: "batch-status",
        dag_type: "paper-review",
        description: "Show scheduled research review batch status.",
    },
    AppActionDescriptor {
        id: "batch-list",
        dag_type: "paper-review",
        description: "List scheduled research review batches.",
    },
];

const C_TO_RUST_ACTIONS: &[AppActionDescriptor] = &[AppActionDescriptor {
    id: "translate",
    dag_type: "c-to-rust",
    description: "Run the c-to-rust DAG app.",
}];

const REGISTERED_APPS: &[RegisteredAppDescriptor] = &[
    RegisteredAppDescriptor {
        id: "c-to-rust",
        label: "C to Rust",
        actions: C_TO_RUST_ACTIONS,
    },
    RegisteredAppDescriptor {
        id: "research",
        label: "Research review",
        actions: RESEARCH_ACTIONS,
    },
];

const REGISTERED_DAG_APPS: &[DagAppDescriptor] = &[
    DagAppDescriptor {
        dag_type: "c-to-rust",
        manifest_file: "c-to-rust.yaml",
        crate_name: "grokrxiv-dag-app-c-to-rust",
    },
    DagAppDescriptor {
        dag_type: "citation-validation",
        manifest_file: "citation-validation.yaml",
        crate_name: "grokrxiv-dag-app-citation-validation",
    },
    DagAppDescriptor {
        dag_type: "paper-extract",
        manifest_file: "paper-extract.yaml",
        crate_name: "grokrxiv-dag-app-paper-extract",
    },
    DagAppDescriptor {
        dag_type: "paper-ingest",
        manifest_file: "paper-ingest.yaml",
        crate_name: "grokrxiv-dag-app-paper-ingest",
    },
    DagAppDescriptor {
        dag_type: "paper-publish",
        manifest_file: "paper-publish.yaml",
        crate_name: "grokrxiv-dag-app-paper-publish",
    },
    DagAppDescriptor {
        dag_type: "paper-review",
        manifest_file: "paper-review.yaml",
        crate_name: "grokrxiv-dag-app-paper-review",
    },
    DagAppDescriptor {
        dag_type: "paper-revise",
        manifest_file: "paper-revise.yaml",
        crate_name: "grokrxiv-dag-app-paper-revise",
    },
];

/// Return all registered product app descriptors.
pub fn registered_apps() -> &'static [RegisteredAppDescriptor] {
    REGISTERED_APPS
}

/// Return all registered product app ids in deterministic order.
pub fn registered_app_ids() -> Vec<&'static str> {
    REGISTERED_APPS.iter().map(|app| app.id).collect()
}

/// Find one registered product app descriptor.
pub fn registered_app(app_id: &str) -> Option<RegisteredAppDescriptor> {
    REGISTERED_APPS.iter().copied().find(|app| app.id == app_id)
}

/// Find one registered product app action.
pub fn registered_app_action(app_id: &str, action_id: &str) -> Option<AppActionDescriptor> {
    registered_app(app_id)?
        .actions
        .iter()
        .copied()
        .find(|action| action.id == action_id)
}

/// Return all registered DAG app descriptors.
pub fn registered_dag_apps() -> &'static [DagAppDescriptor] {
    REGISTERED_DAG_APPS
}

/// Return all registered DAG app ids in deterministic order.
pub fn registered_dag_app_ids() -> Vec<&'static str> {
    REGISTERED_DAG_APPS.iter().map(|app| app.dag_type).collect()
}

/// Find one registered DAG app descriptor.
pub fn registered_dag_app(dag_type: &str) -> Option<DagAppDescriptor> {
    REGISTERED_DAG_APPS
        .iter()
        .copied()
        .find(|app| app.dag_type == dag_type)
}

/// Run a registered DAG app through the generic executor.
pub async fn run_registered_dag_app(
    dag_type: &str,
    input: DagIo,
) -> anyhow::Result<DagExecutionReport> {
    match dag_type {
        "c-to-rust" => run_app(CToRustDagApp, input).await,
        "citation-validation" => run_app(CitationValidationDagApp, input).await,
        "paper-extract" => run_app(PaperExtractDagApp, input).await,
        "paper-ingest" => run_app(PaperIngestDagApp, input).await,
        "paper-publish" => run_app(PaperPublishDagApp, input).await,
        "paper-review" => run_app(PaperReviewDagApp, input).await,
        "paper-revise" => run_app(PaperReviseDagApp, input).await,
        _ => anyhow::bail!("unknown DAG app `{dag_type}`"),
    }
}

async fn run_app<A>(app: A, input: DagIo) -> anyhow::Result<DagExecutionReport>
where
    A: DagApp,
{
    let path = dags_dir().join(app.manifest_file());
    let manifest = DagManifest::from_path(&path)
        .map_err(|err| anyhow::anyhow!("load DAG app manifest {}: {err}", path.display()))?;
    if manifest.id.as_str() != app.dag_type() {
        anyhow::bail!(
            "DAG app `{}` expected manifest id `{}`, found `{}`",
            app.app_name(),
            app.dag_type(),
            manifest.id
        );
    }
    DagExecutor::new(app).execute(&manifest, input).await
}

fn dags_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("GROKRXIV_DAGS_DIR") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(|path| path.join("dags"))
        .unwrap_or_else(|| PathBuf::from("dags"))
}
