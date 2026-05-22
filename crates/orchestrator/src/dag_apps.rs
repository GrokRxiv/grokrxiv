//! Registry for DAG app adapters executable by the generic DAG executor.
//!
//! This module is orchestration glue only: it maps a manifest id to a DAG app
//! crate and runs that app through `agenthero-dag-executor`. App topology lives
//! in YAML manifests, and app behavior belongs in the app crates.

use std::path::{Path, PathBuf};

use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagApp, DagExecutionReport, DagExecutor, DagIo};
use agenthero_dag_runtime::DagManifest;
use grokrxiv_dag_app_citation_validation::CitationValidationDagApp;
use grokrxiv_dag_app_paper_extract::PaperExtractDagApp;
use grokrxiv_dag_app_paper_ingest::PaperIngestDagApp;
use grokrxiv_dag_app_paper_publish::PaperPublishDagApp;
use grokrxiv_dag_app_paper_review::PaperReviewDagApp;
use grokrxiv_dag_app_paper_revise::PaperReviseDagApp;
use serde::Deserialize;

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
    /// Product app id used by `agenthero <app> <action>`.
    pub id: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Actions exposed by this app.
    pub actions: &'static [AppActionDescriptor],
}

/// Static metadata for one app action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppActionDescriptor {
    /// Action id used by `agenthero <app> <action>`.
    pub id: &'static str,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: &'static str,
    /// Short operator-facing description.
    pub description: &'static str,
}

/// YAML product app manifest loaded from `agenthero/apps/*.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppManifest {
    /// Product app slug used by `agenthero <app> ...`.
    pub slug: String,
    /// Human-readable app label.
    pub label: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Rust adapter id.
    pub adapter: String,
    /// App actions exposed by this manifest.
    #[serde(default)]
    pub actions: Vec<AppManifestAction>,
}

/// YAML action metadata for one product app command path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppManifestAction {
    /// Stable action id.
    pub id: String,
    /// Command path after the app slug, e.g. `["validate", "citations"]`.
    pub command: Vec<String>,
    /// DAG type bound to this action.
    pub dag_type: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
}

/// Resolved app action binding loaded from the YAML app manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppActionBinding {
    /// Product app slug.
    pub app: String,
    /// Stable action id.
    pub id: String,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: String,
    /// Operator-facing description.
    pub description: String,
}

const RESEARCH_ACTIONS: &[AppActionDescriptor] = &[
    AppActionDescriptor {
        id: "extract",
        dag_type: "paper-extract",
        description: "Extract source artifacts for one or more GrokRxiv papers.",
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
        description: "Run the daily GrokRxiv ingest scheduler tick once.",
    },
    AppActionDescriptor {
        id: "review",
        dag_type: "paper-review",
        description: "Run the GrokRxiv review DAG for a paper source.",
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
        id: "validate-citations",
        dag_type: "citation-validation",
        description:
            "Validate citations through parser, resolver, metadata, graph, and agent review nodes.",
    },
    AppActionDescriptor {
        id: "verify",
        dag_type: "paper-review",
        description: "Re-run verifier nodes for one persisted GrokRxiv review.",
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
        description: "Show one persisted GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "list",
        dag_type: "paper-review",
        description: "List persisted GrokRxiv sources or reviews.",
    },
    AppActionDescriptor {
        id: "open",
        dag_type: "paper-review",
        description: "Open the canonical URL for one persisted GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "approve",
        dag_type: "paper-publish",
        description: "Approve and publish a completed GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "request-revisions",
        dag_type: "paper-revise",
        description: "Request author revisions for a failed GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "request-changes",
        dag_type: "paper-revise",
        description: "Request moderator changes for a queued GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "reject",
        dag_type: "paper-review",
        description: "Reject a GrokRxiv review from the moderation flow.",
    },
    AppActionDescriptor {
        id: "close",
        dag_type: "paper-publish",
        description: "Hide a review from web output and optionally close its PR.",
    },
    AppActionDescriptor {
        id: "withdraw",
        dag_type: "paper-publish",
        description: "Withdraw a published GrokRxiv review.",
    },
    AppActionDescriptor {
        id: "correct",
        dag_type: "paper-revise",
        description: "Append a correction to a published GrokRxiv review.",
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
        description: "Create a scheduled GrokRxiv review batch.",
    },
    AppActionDescriptor {
        id: "batch-run",
        dag_type: "paper-review",
        description: "Run queued items from a scheduled GrokRxiv review batch.",
    },
    AppActionDescriptor {
        id: "batch-status",
        dag_type: "paper-review",
        description: "Show scheduled GrokRxiv review batch status.",
    },
    AppActionDescriptor {
        id: "batch-list",
        dag_type: "paper-review",
        description: "List scheduled GrokRxiv review batches.",
    },
];

const C2RUST_ACTIONS: &[AppActionDescriptor] = &[AppActionDescriptor {
    id: "migrate",
    dag_type: "c2rust",
    description: "Run the C2Rust migration DAG app.",
}];

const REGISTERED_APPS: &[RegisteredAppDescriptor] = &[
    RegisteredAppDescriptor {
        id: "c2rust",
        label: "C2Rust",
        actions: C2RUST_ACTIONS,
    },
    RegisteredAppDescriptor {
        id: "grokrxiv",
        label: "GrokRxiv",
        actions: RESEARCH_ACTIONS,
    },
];

const REGISTERED_DAG_APPS: &[DagAppDescriptor] = &[
    DagAppDescriptor {
        dag_type: "c2rust",
        manifest_file: "c2rust.yaml",
        crate_name: "agenthero-dag-app-c2rust",
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

/// Resolve one product app action from the YAML app manifest.
pub fn app_action_binding(app_id: &str, action_id: &str) -> anyhow::Result<AppActionBinding> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{app_id} {action_id}`"))?;
    Ok(AppActionBinding {
        app: manifest.slug,
        id: action.id.clone(),
        dag_type: action.dag_type.clone(),
        description: action.description.clone(),
    })
}

/// Load all YAML app manifests in deterministic slug order.
pub fn load_app_manifests() -> anyhow::Result<Vec<AppManifest>> {
    let mut manifests = Vec::new();
    let dir = app_manifests_dir();
    for entry in std::fs::read_dir(&dir)
        .map_err(|err| anyhow::anyhow!("read app manifests dir {}: {err}", dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        manifests.push(load_app_manifest(&path)?);
    }
    manifests.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(manifests)
}

/// Load one product app manifest by slug.
pub fn load_app_manifest_by_slug(slug: &str) -> anyhow::Result<AppManifest> {
    let path = app_manifests_dir().join(format!("{slug}.yaml"));
    load_app_manifest(&path)
}

fn load_app_manifest(path: &Path) -> anyhow::Result<AppManifest> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| anyhow::anyhow!("read app manifest {}: {err}", path.display()))?;
    let manifest: AppManifest = serde_yaml::from_str(&text)
        .map_err(|err| anyhow::anyhow!("parse app manifest {}: {err}", path.display()))?;
    validate_app_manifest(&manifest)
        .map_err(|err| anyhow::anyhow!("validate app manifest {}: {err}", path.display()))?;
    Ok(manifest)
}

fn validate_app_manifest(manifest: &AppManifest) -> anyhow::Result<()> {
    if manifest.slug.trim().is_empty() {
        anyhow::bail!("slug is required");
    }
    if manifest.adapter.trim().is_empty() {
        anyhow::bail!("adapter is required");
    }
    let mut ids = std::collections::HashSet::new();
    for action in &manifest.actions {
        if action.id.trim().is_empty() {
            anyhow::bail!("action id is required");
        }
        if !ids.insert(action.id.as_str()) {
            anyhow::bail!("duplicate action id `{}`", action.id);
        }
        if action.command.is_empty() || action.command.iter().any(|part| part.trim().is_empty()) {
            anyhow::bail!("action `{}` command path is required", action.id);
        }
        if action.dag_type.trim().is_empty() {
            anyhow::bail!("action `{}` dag_type is required", action.id);
        }
    }
    Ok(())
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
        "c2rust" => run_app(C2RustDagApp, input).await,
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
    if let Some(path) = std::env::var_os("AGENTHERO_DAGS_DIR") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(|path| path.join("dags"))
        .unwrap_or_else(|| PathBuf::from("dags"))
}

fn app_manifests_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENTHERO_APPS_DIR") {
        return PathBuf::from(path);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(|path| path.join("agenthero").join("apps"))
        .unwrap_or_else(|| PathBuf::from("agenthero/apps"))
}
