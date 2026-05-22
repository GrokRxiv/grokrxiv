//! Paper review DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use grokrxiv_dag_executor::{
    manifest_node_result, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};

/// DAG app adapter for `paper-review`.
#[derive(Debug, Clone, Default)]
pub struct PaperReviewDagApp;

impl DagApp for PaperReviewDagApp {
    fn dag_type(&self) -> &'static str {
        "paper-review"
    }

    fn manifest_file(&self) -> &'static str {
        "paper-review.yaml"
    }
}

#[async_trait]
impl NodeHandler for PaperReviewDagApp {
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
