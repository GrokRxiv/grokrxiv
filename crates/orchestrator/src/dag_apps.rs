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
