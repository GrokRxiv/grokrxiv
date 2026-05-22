//! C2Rust DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use agenthero_dag_executor::{
    manifest_node_result, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};
use async_trait::async_trait;

/// DAG app adapter for the `c2rust` proving-ground DAG.
#[derive(Debug, Clone, Default)]
pub struct C2RustDagApp;

impl DagApp for C2RustDagApp {
    fn dag_type(&self) -> &'static str {
        "c2rust"
    }

    fn manifest_file(&self) -> &'static str {
        "c2rust.yaml"
    }

    fn app_name(&self) -> &'static str {
        "c2rust"
    }
}

#[async_trait]
impl NodeHandler for C2RustDagApp {
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
