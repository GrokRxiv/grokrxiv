//! c-to-rust DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use grokrxiv_dag_executor::{
    manifest_node_result, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};

/// DAG app adapter for the `c-to-rust` proving-ground DAG.
#[derive(Debug, Clone, Default)]
pub struct CToRustDagApp;

impl DagApp for CToRustDagApp {
    fn dag_type(&self) -> &'static str {
        "c-to-rust"
    }

    fn manifest_file(&self) -> &'static str {
        "c-to-rust.yaml"
    }

    fn app_name(&self) -> &'static str {
        "c-to-rust"
    }
}

#[async_trait]
impl NodeHandler for CToRustDagApp {
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
