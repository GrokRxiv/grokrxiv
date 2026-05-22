//! Paper extraction DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use agenthero_dag_executor::{
    manifest_node_result, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};
use async_trait::async_trait;

/// DAG app adapter for `paper-extract`.
#[derive(Debug, Clone, Default)]
pub struct PaperExtractDagApp;

impl DagApp for PaperExtractDagApp {
    fn dag_type(&self) -> &'static str {
        "paper-extract"
    }

    fn manifest_file(&self) -> &'static str {
        "paper-extract.yaml"
    }
}

#[async_trait]
impl NodeHandler for PaperExtractDagApp {
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
