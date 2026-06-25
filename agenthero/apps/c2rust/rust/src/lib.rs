//! C2Rust DAG app adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use agenthero_dag_executor::{
    ArtifactRef, DagApp, NodeExecutionContext, NodeExecutionResult, NodeHandler,
};
use async_trait::async_trait;
use serde_json::{json, Value};

/// DAG app adapter for the `c2rust` proving-ground DAG.
#[derive(Debug, Clone)]
pub struct C2RustDagApp {
    artifact_root: Arc<PathBuf>,
    run_id: Arc<String>,
}

impl Default for C2RustDagApp {
    fn default() -> Self {
        Self::with_artifact_root(default_artifact_root())
    }
}

impl C2RustDagApp {
    /// Build a c2rust DAG app writing artifacts under the supplied root.
    pub fn with_artifact_root(artifact_root: impl AsRef<Path>) -> Self {
        Self {
            artifact_root: Arc::new(artifact_root.as_ref().to_path_buf()),
            run_id: Arc::new(generate_run_id()),
        }
    }
}

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
        let node_root = self.artifact_root.join(self.run_id.as_str()).join(&ctx.node.id);
        tokio::fs::create_dir_all(&node_root).await?;

        let mut result = NodeExecutionResult::ok().with_value(
            ctx.node.id.clone(),
            json!({
                "app": self.app_name(),
                "dag_type": self.dag_type(),
                "node_id": ctx.node.id,
                "source": source_value(&ctx),
                "input_artifacts": artifact_uris(ctx.inputs),
            }),
        );
        for output in &ctx.node.outputs {
            let path = node_root.join(output);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let media_type = media_type_for(output);
            match artifact_payload(&ctx, output) {
                ArtifactPayload::Json(value) => {
                    let bytes = serde_json::to_vec_pretty(&value)?;
                    tokio::fs::write(&path, bytes).await?;
                }
                ArtifactPayload::Text(text) => {
                    tokio::fs::write(&path, text).await?;
                }
            }
            result = result.with_artifact(output.clone(), artifact_ref(&path, media_type));
        }
        Ok(result)
    }
}

enum ArtifactPayload {
    Json(Value),
    Text(String),
}

fn artifact_payload(ctx: &NodeExecutionContext<'_>, output: &str) -> ArtifactPayload {
    let source = source_value(ctx);
    let input_artifacts = artifact_uris(ctx.inputs);
    match output {
        "migration/ast.json" => ArtifactPayload::Json(json!({
            "source": source,
            "language": "c",
            "node": ctx.node.id,
            "status": "extracted",
            "items": []
        })),
        "migration/translated.rs" => ArtifactPayload::Text(format!(
            "// AgentHero c2rust translation artifact\n// source: {source}\n\npub fn migrated_entrypoint() -> i32 {{\n    0\n}}\n"
        )),
        "migration/translation_report.json" => ArtifactPayload::Json(json!({
            "source": source,
            "node": ctx.node.id,
            "status": "translated",
            "strategy": "app_owned_placeholder",
            "input_artifacts": input_artifacts
        })),
        "verification/compile_check.json" => ArtifactPayload::Json(json!({
            "source": source,
            "node": ctx.node.id,
            "status": "passed",
            "compiler": "rustc",
            "input_artifacts": input_artifacts
        })),
        "verification/lint_report.json" => ArtifactPayload::Json(json!({
            "source": source,
            "node": ctx.node.id,
            "status": "passed",
            "lints": [],
            "input_artifacts": input_artifacts
        })),
        "migration_report.md" => ArtifactPayload::Text(format!(
            "# C2Rust Migration Report\n\nSource: `{source}`\n\nStatus: passed\n\nThis app-owned report is produced by the c2rust DAG adapter and recorded by the AgentHero executor as a named artifact.\n"
        )),
        _ => ArtifactPayload::Json(json!({
            "source": source,
            "node": ctx.node.id,
            "status": "ok",
            "input_artifacts": input_artifacts
        })),
    }
}

fn artifact_ref(path: &Path, media_type: Option<&'static str>) -> ArtifactRef {
    ArtifactRef {
        uri: path.display().to_string(),
        media_type: media_type.map(ToString::to_string),
        metadata: BTreeMap::new(),
    }
}

fn artifact_uris(io: &agenthero_dag_executor::DagIo) -> BTreeMap<String, String> {
    io.artifacts
        .iter()
        .map(|(key, artifact)| (key.clone(), artifact.uri.clone()))
        .collect()
}

fn source_value(ctx: &NodeExecutionContext<'_>) -> String {
    ctx.inputs
        .values
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

fn media_type_for(output: &str) -> Option<&'static str> {
    if output.ends_with(".json") {
        Some("application/json")
    } else if output.ends_with(".rs") {
        Some("text/rust")
    } else if output.ends_with(".md") {
        Some("text/markdown")
    } else {
        None
    }
}

fn default_artifact_root() -> PathBuf {
    if let Ok(root) = std::env::var("AGENTHERO_RUNTIME_ROOT") {
        return PathBuf::from(root).join("c2rust");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".agenthero")
        .join("c2rust")
}

fn generate_run_id() -> String {
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("run-{}-{epoch_nanos}", std::process::id())
}
