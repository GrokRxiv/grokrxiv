//! Process adapter for the C2Rust DAG app.

#![forbid(unsafe_code)]

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse};
use agenthero_app_sdk::{
    app_root_from_manifest_dir, load_dag_manifest, read_adapter_request, write_adapter_response,
};
use agenthero_dag_app_c2rust::C2RustDagApp;
use agenthero_dag_executor::{DagExecutionReport, DagExecutor};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let request = read_adapter_request(std::io::stdin())?;
    let response = match run(&request).await {
        Ok(report) => AppAdapterResponse::ok_report(&request, report),
        Err(err) => AppAdapterResponse::failed(&request, format!("{err:#}")),
    };
    write_adapter_response(std::io::stdout(), &response)?;
    Ok(())
}

async fn run(request: &AppAdapterRequest) -> anyhow::Result<DagExecutionReport> {
    if request.app != "c2rust" {
        anyhow::bail!("c2rust adapter received app `{}`", request.app);
    }
    if request.dag_type != "c2rust" {
        anyhow::bail!("c2rust adapter received dag_type `{}`", request.dag_type);
    }
    let manifest = load_dag_manifest(app_root(), "c2rust")?;
    DagExecutor::new(C2RustDagApp)
        .execute(&manifest, request.input.clone())
        .await
}

fn app_root() -> std::path::PathBuf {
    app_root_from_manifest_dir(env!("CARGO_MANIFEST_DIR"))
}
