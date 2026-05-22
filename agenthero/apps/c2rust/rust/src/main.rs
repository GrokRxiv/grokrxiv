//! Process adapter for the C2Rust DAG app.

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse};
use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagExecutionReport, DagExecutor};
use agenthero_dag_runtime::DagManifest;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    let request: AppAdapterRequest = serde_json::from_str(&payload)?;
    let response = match run(&request).await {
        Ok(report) => AppAdapterResponse::ok_report(&request, report),
        Err(err) => AppAdapterResponse::failed(&request, format!("{err:#}")),
    };
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

async fn run(request: &AppAdapterRequest) -> anyhow::Result<DagExecutionReport> {
    if request.app != "c2rust" {
        anyhow::bail!("c2rust adapter received app `{}`", request.app);
    }
    if request.dag_type != "c2rust" {
        anyhow::bail!("c2rust adapter received dag_type `{}`", request.dag_type);
    }
    let manifest_path = app_root().join("dags").join("c2rust.yaml");
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|err| anyhow::anyhow!("load {}: {err}", manifest_path.display()))?;
    DagExecutor::new(C2RustDagApp)
        .execute(&manifest, request.input.clone())
        .await
}

fn app_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
