//! Process adapter for the GrokRxiv DAG app.

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse};
use agenthero_dag_executor::{
    manifest_node_result, DagExecutionReport, DagExecutor, NodeExecutionContext,
    NodeExecutionResult, NodeHandler,
};
use agenthero_dag_runtime::DagManifest;
use async_trait::async_trait;

#[derive(Debug, Clone)]
struct GrokrxivAdapter {
    app_name: &'static str,
}

#[async_trait]
impl NodeHandler for GrokrxivAdapter {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(manifest_node_result(
            self.app_name,
            ctx.manifest.id.as_str(),
            ctx.node,
        ))
    }
}

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
    if request.app != "grokrxiv" {
        anyhow::bail!("grokrxiv adapter received app `{}`", request.app);
    }
    let manifest_path = app_root().join("dags").join(format!("{}.yaml", request.dag_type));
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|err| anyhow::anyhow!("load {}: {err}", manifest_path.display()))?;
    if manifest.id.as_str() != request.dag_type {
        anyhow::bail!(
            "manifest {} id `{}` does not match request dag_type `{}`",
            manifest_path.display(),
            manifest.id,
            request.dag_type
        );
    }
    DagExecutor::new(GrokrxivAdapter {
        app_name: "grokrxiv",
    })
    .execute(&manifest, request.input.clone())
    .await
}

fn app_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
