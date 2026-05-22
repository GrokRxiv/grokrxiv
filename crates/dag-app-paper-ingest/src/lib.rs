//! Paper ingest DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use agenthero_dag_executor::{
    manifest_node_result, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};
use async_trait::async_trait;

/// DAG app adapter for `paper-ingest`.
#[derive(Debug, Clone, Default)]
pub struct PaperIngestDagApp;

impl DagApp for PaperIngestDagApp {
    fn dag_type(&self) -> &'static str {
        "paper-ingest"
    }

    fn manifest_file(&self) -> &'static str {
        "paper-ingest.yaml"
    }
}

#[async_trait]
impl NodeHandler for PaperIngestDagApp {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(manifest_node_result(
            self.app_name(),
            self.dag_type(),
            ctx.node,
        ))
    }
}
