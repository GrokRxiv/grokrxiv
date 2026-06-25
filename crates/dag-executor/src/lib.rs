//! Generic manifest-driven DAG execution contracts.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agenthero_dag_runtime::{
    is_safe_artifact_key, DagManifest, DagNode, DagNodeKind, DagNodeReport, DagNodeStatus, DagTool,
    OneOrMany, ToolExecutorKind,
};
use async_trait::async_trait;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::Instrument as _;

/// Input value key containing the current 1-based round for a `loop` node.
pub const LOOP_ROUND_INPUT: &str = "loop_round";
/// Input value key containing the declared max round count for a `loop` node.
pub const LOOP_MAX_ROUNDS_INPUT: &str = "loop_max_rounds";
/// Input value key containing the manifest node id for a `loop` node.
pub const LOOP_NODE_ID_INPUT: &str = "loop_node_id";
/// Input value key containing the current zero-based item index for a `map` node.
pub const MAP_INDEX_INPUT: &str = "map_index";
/// Input value key containing the declared max item count for a `map` node.
pub const MAP_MAX_ITEMS_INPUT: &str = "map_max_items";
/// Input value key containing the manifest node id for a `map` node.
pub const MAP_NODE_ID_INPUT: &str = "map_node_id";

const COMMAND_KILL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const AGENTHERO_BUDGET_UNITS_REMAINING: &str = "agenthero_budget_units_remaining";

/// Reference to an artifact stored outside the executor's JSON value map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// URI or path understood by the DAG app's artifact adapter.
    pub uri: String,
    /// Optional media type, for example `application/json` or `text/markdown`.
    #[serde(default)]
    pub media_type: Option<String>,
    /// App-specific metadata that remains visible to scheduler/report tooling.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Generic node input/output payload.
///
/// The executor stays app-agnostic by passing named JSON values and named
/// artifact references. DAG apps perform typed Rust conversion at their
/// boundary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DagIo {
    /// Named JSON values.
    #[serde(default)]
    pub values: BTreeMap<String, serde_json::Value>,
    /// Named artifact references.
    #[serde(default)]
    pub artifacts: BTreeMap<String, ArtifactRef>,
}

impl DagIo {
    fn merge(&mut self, other: DagIo) {
        self.values.extend(other.values);
        self.artifacts.extend(other.artifacts);
    }
}

/// Context passed to a DAG app for one node.
pub struct NodeExecutionContext<'a> {
    /// Manifest being executed.
    pub manifest: &'a DagManifest,
    /// Node being executed.
    pub node: &'a DagNode,
    /// Snapshot of values/artifacts available before this node starts.
    pub inputs: &'a DagIo,
}

/// Result returned by a DAG app for one node.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeExecutionResult {
    /// Node status.
    pub status: DagNodeStatus,
    /// Values/artifacts produced by the node.
    pub outputs: DagIo,
    /// Diagnostic values/artifacts produced by the node, such as logs.
    pub diagnostics: DagIo,
    /// Optional warning recorded in the run report.
    pub warning: Option<String>,
    /// Optional error recorded in the run report.
    pub error: Option<String>,
    /// Actual command invoked by the node, when applicable.
    pub command: Option<Vec<String>>,
    /// Actual process exit status, when applicable.
    pub exit_status: Option<i32>,
    /// Model identifier used by an LLM-backed node, when applicable.
    pub model: Option<String>,
    /// Stable hash of the prompt used by an LLM-backed node, when applicable.
    pub prompt_hash: Option<String>,
    /// App-owned structured audit details for the node attempt.
    pub trace: BTreeMap<String, serde_json::Value>,
}

impl NodeExecutionResult {
    /// Successful node result with no outputs.
    pub fn ok() -> Self {
        Self {
            status: DagNodeStatus::Ok,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: None,
            error: None,
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        }
    }

    /// Degraded node result with no outputs.
    pub fn degraded(warning: impl Into<String>) -> Self {
        Self {
            status: DagNodeStatus::Degraded,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: Some(warning.into()),
            error: None,
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        }
    }

    /// Add one JSON output value.
    pub fn with_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.outputs.values.insert(key.into(), value);
        self
    }

    /// Add one artifact output reference.
    pub fn with_artifact(mut self, key: impl Into<String>, artifact: ArtifactRef) -> Self {
        self.outputs.artifacts.insert(key.into(), artifact);
        self
    }

    /// Add one diagnostic artifact reference.
    pub fn with_diagnostic_artifact(
        mut self,
        key: impl Into<String>,
        artifact: ArtifactRef,
    ) -> Self {
        self.diagnostics.artifacts.insert(key.into(), artifact);
        self
    }

    /// Record the command actually invoked for this node.
    pub fn with_command(mut self, command: Vec<String>) -> Self {
        self.command = Some(command);
        self
    }

    /// Record the process exit status for this node.
    pub fn with_exit_status(mut self, exit_status: Option<i32>) -> Self {
        self.exit_status = exit_status;
        self
    }

    /// Record the model identifier used by an LLM-backed node.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Record the stable prompt hash used by an LLM-backed node.
    pub fn with_prompt_hash(mut self, prompt_hash: impl Into<String>) -> Self {
        self.prompt_hash = Some(prompt_hash.into());
        self
    }

    /// Record one structured audit value on the node report and events.
    pub fn with_trace_value(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.trace.insert(key.into(), value);
        self
    }

    /// Record a node error while preserving a structured node result.
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }
}

/// App-side node dispatcher.
#[async_trait]
pub trait NodeHandler: Send + Sync {
    /// Execute one manifest node.
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult>;
}

/// Named DAG app that can execute nodes for one manifest id.
pub trait DagApp: NodeHandler {
    /// DAG type this app owns, matching `dags/<manifest_file>`.
    fn dag_type(&self) -> &'static str;

    /// Manifest filename for this app.
    fn manifest_file(&self) -> &'static str;

    /// Human-readable app label.
    fn app_name(&self) -> &'static str {
        self.dag_type()
    }
}

static NEXT_GENERIC_TOOL_RUNNER_ID: AtomicU64 = AtomicU64::new(0);

/// Generic AgentHero runner for command-backed tool nodes.
///
/// This runner deliberately knows how to invoke tools and record artifacts, but
/// not how to interpret domain-specific results. Apps can use it directly for
/// command-only DAGs or compose it inside an app-owned handler.
#[derive(Debug, Clone)]
pub struct GenericToolRunner {
    artifact_root: Arc<PathBuf>,
    run_id: Arc<String>,
    sequence: Arc<AtomicU64>,
    default_timeout: Duration,
}

impl GenericToolRunner {
    /// Build a generic tool runner whose node work directories live under
    /// `artifact_root/<runner-id>/<dag>/<node>/`.
    pub fn new(artifact_root: impl AsRef<Path>) -> Self {
        Self {
            artifact_root: Arc::new(artifact_root.as_ref().to_path_buf()),
            run_id: Arc::new(generate_runner_id()),
            sequence: Arc::new(AtomicU64::new(0)),
            default_timeout: Duration::from_secs(300),
        }
    }

    /// Override the timeout used when a tool manifest does not set
    /// `timeout_secs`.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Execute the node when it is a supported generic tool.
    pub async fn execute_supported_tool(
        &self,
        ctx: &NodeExecutionContext<'_>,
    ) -> Option<anyhow::Result<NodeExecutionResult>> {
        if ctx.node.kind != DagNodeKind::Tool {
            return None;
        }
        let tool_id = match ctx.node.tool.as_deref() {
            Some(tool_id) => tool_id,
            None => {
                return Some(Err(anyhow::anyhow!(
                    "tool node `{}` has no tool reference",
                    ctx.node.id
                )));
            }
        };
        let tool = match find_tool(ctx.manifest, tool_id) {
            Some(tool) => tool,
            None => {
                return Some(Err(anyhow::anyhow!(
                    "tool node `{}` references unknown tool `{tool_id}`",
                    ctx.node.id
                )));
            }
        };
        match tool.executor {
            ToolExecutorKind::ApprovalGate => Some(self.execute_approval_gate_tool(ctx, tool)),
            ToolExecutorKind::Http => Some(self.execute_http_tool(ctx, tool).await),
            executor if is_generic_command_tool(executor) => {
                Some(self.execute_command_tool(ctx, tool).await)
            }
            _ => None,
        }
    }

    fn execute_approval_gate_tool(
        &self,
        ctx: &NodeExecutionContext<'_>,
        tool: &DagTool,
    ) -> anyhow::Result<NodeExecutionResult> {
        let approved_key = tool_approval_key(&tool.id);
        let approved = ctx
            .inputs
            .values
            .get(&approved_key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if approved {
            Ok(NodeExecutionResult::ok().with_value(
                ctx.node.id.clone(),
                serde_json::json!({
                    "approved_key": approved_key,
                    "approved": true,
                    "tool": tool.id,
                }),
            ))
        } else {
            Ok(NodeExecutionResult {
                status: DagNodeStatus::AwaitingApproval,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: Some(format!(
                    "approval gate `{}` is waiting for `{approved_key}`",
                    tool.id
                )),
                error: None,
                command: None,
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            })
        }
    }

    async fn execute_http_tool(
        &self,
        ctx: &NodeExecutionContext<'_>,
        tool: &DagTool,
    ) -> anyhow::Result<NodeExecutionResult> {
        let command = tool
            .command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("http tool `{}` has no command", tool.id))?;
        if command.len() < 2 {
            anyhow::bail!("http tool `{}` command must be [METHOD, URL, ...]", tool.id);
        }

        let workdir = self.node_workdir(ctx).await?;
        let timeout_duration = tool
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        if let Some(error) = unsupported_host_isolation_policy_error(tool) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if let Some(error) = filesystem_write_policy_error(tool, ctx.node) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if let Some(error) = budget_policy_error(tool, ctx.inputs) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if !command[0].eq_ignore_ascii_case("GET") {
            let error = format!(
                "http tool `{}` only supports GET until unsafe methods have an explicit policy gate",
                tool.id
            );
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if tool
            .policy
            .as_ref()
            .is_some_and(|policy| !policy.network.allow)
        {
            let error = format!(
                "http tool `{}` network policy denies outbound requests",
                tool.id
            );
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if approval_required(tool, ctx.inputs) {
            return Ok(NodeExecutionResult {
                status: DagNodeStatus::AwaitingApproval,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: Some(format!(
                    "http tool `{}` is waiting for approval key `{}`",
                    tool.id,
                    tool_approval_key(&tool.id)
                )),
                error: None,
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            });
        }

        let method = Method::from_bytes(command[0].as_bytes())
            .map_err(|err| anyhow::anyhow!("http tool `{}` invalid method: {err}", tool.id))?;
        let url = &command[1];
        let client = reqwest::Client::builder()
            .timeout(timeout_duration)
            .build()?;
        let response = match client.request(method, url).send().await {
            Ok(response) => response,
            Err(err) => {
                let error = format!("http tool `{}` request failed: {err}", tool.id);
                let status = if ctx.node.required {
                    DagNodeStatus::Failed
                } else {
                    DagNodeStatus::Degraded
                };
                write_status_artifact(&workdir, &command, None, status, Some(error.as_str()))
                    .await?;
                let mut result = NodeExecutionResult {
                    status,
                    outputs: DagIo::default(),
                    diagnostics: DagIo::default(),
                    warning: (!ctx.node.required).then(|| error.clone()),
                    error: ctx.node.required.then_some(error),
                    command: Some(command),
                    exit_status: None,
                    model: None,
                    prompt_hash: None,
                    trace: BTreeMap::new(),
                };
                result = result.with_diagnostic_artifact(
                    format!("logs/{}/status.json", ctx.node.id),
                    artifact_ref(&workdir.join("status.json")),
                );
                return Ok(result);
            }
        };
        let status_code = response.status().as_u16();
        let success = response.status().is_success();
        let headers = response.headers().clone();
        let body = response.bytes().await?;

        let node_status = if success {
            DagNodeStatus::Ok
        } else if ctx.node.required {
            DagNodeStatus::Failed
        } else {
            DagNodeStatus::Degraded
        };
        let error =
            (!success).then(|| format!("http tool `{}` returned status {}", tool.id, status_code));

        if let Some(output_name) = ctx.node.outputs.first() {
            let output_path = resolve_output_path(&workdir, output_name)?;
            if let Some(parent) = output_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&output_path, &body).await?;
        }
        let headers_json = serde_json::json!({
            "status": status_code,
            "headers": headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_string(),
                        value.to_str().unwrap_or_default().to_string(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        });
        tokio::fs::write(
            workdir.join("headers.json"),
            serde_json::to_vec_pretty(&headers_json)?,
        )
        .await?;
        write_status_artifact(
            &workdir,
            &command,
            Some(i32::from(status_code)),
            node_status,
            error.as_deref(),
        )
        .await?;

        let mut result = NodeExecutionResult {
            status: node_status,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: (node_status == DagNodeStatus::Degraded).then(|| {
                error
                    .clone()
                    .unwrap_or_else(|| format!("http tool `{}` degraded", tool.id))
            }),
            error: (node_status == DagNodeStatus::Failed).then(|| {
                error
                    .clone()
                    .unwrap_or_else(|| format!("http tool `{}` failed", tool.id))
            }),
            command: Some(command),
            exit_status: Some(i32::from(status_code)),
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        };
        for name in &ctx.node.outputs {
            let path = resolve_output_path(&workdir, name)?;
            if tokio::fs::metadata(&path).await.is_ok() {
                result = result.with_artifact(name.clone(), artifact_ref(&path));
            }
        }
        for name in ["headers.json", "status.json"] {
            result = result.with_diagnostic_artifact(
                format!("logs/{}/{}", ctx.node.id, name),
                artifact_ref(&workdir.join(name)),
            );
        }
        Ok(result)
    }

    async fn execute_command_tool(
        &self,
        ctx: &NodeExecutionContext<'_>,
        tool: &DagTool,
    ) -> anyhow::Result<NodeExecutionResult> {
        let command = tool
            .command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("tool `{}` has no command", tool.id))?;
        if command.is_empty() {
            anyhow::bail!("tool `{}` has an empty command", tool.id);
        }

        let workdir = self.node_workdir(ctx).await?;

        if let Some(error) = executor_command_boundary_error(tool, &command) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_model(ctx.inputs))
                    .flatten(),
                prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_prompt_hash(ctx.inputs))
                    .flatten(),
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if let Some(error) = unsupported_host_isolation_policy_error(tool) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_model(ctx.inputs))
                    .flatten(),
                prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_prompt_hash(ctx.inputs))
                    .flatten(),
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if let Some(error) = filesystem_write_policy_error(tool, ctx.node) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_model(ctx.inputs))
                    .flatten(),
                prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_prompt_hash(ctx.inputs))
                    .flatten(),
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if let Some(error) = budget_policy_error(tool, ctx.inputs) {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            write_status_artifact(&workdir, &command, None, status, Some(error.as_str())).await?;
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_model(ctx.inputs))
                    .flatten(),
                prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_prompt_hash(ctx.inputs))
                    .flatten(),
                trace: BTreeMap::new(),
            };
            result = result.with_diagnostic_artifact(
                format!("logs/{}/status.json", ctx.node.id),
                artifact_ref(&workdir.join("status.json")),
            );
            return Ok(result);
        }

        if approval_required(tool, ctx.inputs) {
            return Ok(NodeExecutionResult {
                status: DagNodeStatus::AwaitingApproval,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: Some(format!(
                    "tool `{}` is waiting for approval key `{}`",
                    tool.id,
                    tool_approval_key(&tool.id)
                )),
                error: None,
                command: Some(command),
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: BTreeMap::new(),
            });
        }

        let mut process = Command::new(&command[0]);
        process
            .args(&command[1..])
            .current_dir(&workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_command_process_group(&mut process);
        apply_tool_env(&mut process, ctx.manifest, ctx.node, tool, ctx.inputs);

        let timeout_duration = tool
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);
        let output = run_command_with_timeout(process, timeout_duration).await?;
        if output.timed_out {
            let status = if ctx.node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            tokio::fs::write(workdir.join("stdout.log"), &output.stdout).await?;
            tokio::fs::write(workdir.join("stderr.log"), &output.stderr).await?;
            write_status_artifact(&workdir, &command, None, status, Some("timed out")).await?;
            let error = format!(
                "tool `{}` timed out after {}s",
                tool.id,
                timeout_duration.as_secs()
            );
            let mut result = NodeExecutionResult {
                status,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: (!ctx.node.required).then(|| error.clone()),
                error: ctx.node.required.then_some(error),
                command: Some(command),
                exit_status: None,
                model: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_model(ctx.inputs))
                    .flatten(),
                prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                    .then(|| llm_prompt_hash(ctx.inputs))
                    .flatten(),
                trace: BTreeMap::new(),
            };
            for name in ["stdout.log", "stderr.log", "status.json"] {
                result = result.with_diagnostic_artifact(
                    format!("logs/{}/{}", ctx.node.id, name),
                    artifact_ref(&workdir.join(name)),
                );
            }
            return Ok(result);
        }

        let process_status = output
            .status
            .expect("non-timeout command run must include exit status");
        let exit_status = process_status.code();
        tokio::fs::write(workdir.join("stdout.log"), &output.stdout).await?;
        tokio::fs::write(workdir.join("stderr.log"), &output.stderr).await?;

        let mut missing_outputs = Vec::new();
        let mut materialized_outputs = BTreeMap::new();
        for name in &ctx.node.outputs {
            let path = resolve_output_path(&workdir, name)?;
            if tokio::fs::metadata(&path).await.is_ok() {
                materialized_outputs.insert(name.clone(), artifact_ref(&path));
            } else {
                missing_outputs.push(name.clone());
            }
        }

        let status = if process_status.success() && missing_outputs.is_empty() {
            DagNodeStatus::Ok
        } else if ctx.node.required {
            DagNodeStatus::Failed
        } else {
            DagNodeStatus::Degraded
        };
        let error = if !process_status.success() {
            Some(format!(
                "tool `{}` exited with status {}",
                tool.id,
                process_status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string())
            ))
        } else if !missing_outputs.is_empty() {
            Some(missing_declared_outputs_message(&tool.id, &missing_outputs))
        } else {
            None
        };
        write_status_artifact(&workdir, &command, exit_status, status, error.as_deref()).await?;

        let mut result = NodeExecutionResult {
            status,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: (status == DagNodeStatus::Degraded).then(|| {
                error
                    .clone()
                    .unwrap_or_else(|| format!("tool `{}` degraded", tool.id))
            }),
            error: (status == DagNodeStatus::Failed).then(|| {
                error
                    .clone()
                    .unwrap_or_else(|| format!("tool `{}` failed", tool.id))
            }),
            command: Some(command),
            exit_status,
            model: (tool.executor == ToolExecutorKind::Llm)
                .then(|| llm_model(ctx.inputs))
                .flatten(),
            prompt_hash: (tool.executor == ToolExecutorKind::Llm)
                .then(|| llm_prompt_hash(ctx.inputs))
                .flatten(),
            trace: BTreeMap::new(),
        };

        for (name, artifact) in materialized_outputs {
            result = result.with_artifact(name, artifact);
        }
        for name in ["stdout.log", "stderr.log", "status.json"] {
            result = result.with_diagnostic_artifact(
                format!("logs/{}/{}", ctx.node.id, name),
                artifact_ref(&workdir.join(name)),
            );
        }

        Ok(result)
    }

    async fn node_workdir(&self, ctx: &NodeExecutionContext<'_>) -> anyhow::Result<PathBuf> {
        let workdir = self
            .artifact_root
            .join(self.run_id.as_str())
            .join(ctx.manifest.id.as_str())
            .join(&ctx.node.id)
            .join(format!(
                "attempt-{}",
                self.sequence.fetch_add(1, Ordering::SeqCst) + 1
            ));
        tokio::fs::create_dir_all(&workdir).await?;
        Ok(workdir)
    }
}

struct CommandRunOutput {
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

async fn run_command_with_timeout(
    mut command: Command,
    timeout_duration: Duration,
) -> anyhow::Result<CommandRunOutput> {
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("spawned command stdout unavailable"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("spawned command stderr unavailable"))?;
    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });

    let (status, timed_out) = match timeout(timeout_duration, child.wait()).await {
        Ok(status) => (Some(status?), false),
        Err(_) => {
            kill_command_child(&mut child).await;
            (None, true)
        }
    };

    let stdout = stdout_task
        .await
        .map_err(|err| anyhow::anyhow!("join command stdout reader: {err}"))??;
    let stderr = stderr_task
        .await
        .map_err(|err| anyhow::anyhow!("join command stderr reader: {err}"))??;

    Ok(CommandRunOutput {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

async fn kill_command_child(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        kill_command_process_group(pid);
        match timeout(COMMAND_KILL_WAIT_TIMEOUT, child.wait()).await {
            Ok(Ok(_)) => return,
            Ok(Err(err)) => {
                tracing::warn!(
                    error = %err,
                    "wait for killed command process group failed; falling back to direct child kill"
                );
            }
            Err(_) => {
                tracing::warn!(
                    "timed out waiting for killed command process group; falling back to direct child kill"
                );
            }
        }
    }

    let _ = child.kill().await;
    match timeout(COMMAND_KILL_WAIT_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => {
            tracing::warn!(
                error = %err,
                "wait for killed command child failed"
            );
        }
        Err(_) => {
            tracing::warn!("timed out waiting for killed command child");
        }
    }
}

#[cfg(unix)]
fn kill_command_process_group(pid: u32) {
    let pgid = nix::unistd::Pid::from_raw(pid as i32);
    match nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to kill command process group"
            );
        }
    }
}

#[cfg(unix)]
fn configure_command_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_command_process_group(_command: &mut Command) {}

fn generate_runner_id() -> String {
    let local_id = NEXT_GENERIC_TOOL_RUNNER_ID.fetch_add(1, Ordering::SeqCst) + 1;
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("runner-{}-{epoch_nanos}-{local_id}", std::process::id())
}

#[async_trait]
impl NodeHandler for GenericToolRunner {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let node_id = ctx.node.id.clone();
        self.execute_supported_tool(&ctx).await.unwrap_or_else(|| {
            Err(anyhow::anyhow!(
                "node `{}` is not a supported generic command tool",
                node_id
            ))
        })
    }
}

/// Build a standard manifest-only success payload for app adapter smoke paths.
///
/// Production app adapters can call this for deterministic nodes while
/// overriding real tools, agents, verifiers, renderers, and publisher actions.
pub fn manifest_node_result(app_name: &str, dag_type: &str, node: &DagNode) -> NodeExecutionResult {
    NodeExecutionResult::ok().with_value(
        node.id.clone(),
        serde_json::json!({
            "app": app_name,
            "dag_type": dag_type,
            "node_id": node.id,
            "node_kind": node.kind.to_string(),
            "role": node.role.as_ref().map(ToString::to_string),
            "tool": node.tool,
            "dag_call": node.dag_type,
            "inputs": node.inputs,
            "outputs": node.outputs,
        }),
    )
}

/// Generic DAG execution report plus final JSON/artifact state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagExecutionReport {
    /// DAG type id.
    pub dag_type: agenthero_dag_runtime::DagTypeId,
    /// Manifest version that produced this execution.
    #[serde(default)]
    pub manifest_version: u32,
    /// Stable hash of the manifest content used for replay/provenance.
    #[serde(default)]
    pub manifest_hash: String,
    /// Overall run status.
    pub status: DagNodeStatus,
    /// Frozen input values/artifacts supplied to the run.
    #[serde(default)]
    pub input: DagIo,
    /// Per-node report entries in execution order.
    #[serde(default)]
    pub nodes: Vec<DagNodeReport>,
    /// Final values/artifacts after all executable nodes finish.
    #[serde(default)]
    pub outputs: DagIo,
    /// Generic event stream emitted during execution.
    #[serde(default)]
    pub events: Vec<DagExecutionEvent>,
}

impl DagExecutionReport {
    /// Return the status recorded for one node id.
    pub fn node_status(&self, node_id: &str) -> Option<DagNodeStatus> {
        self.nodes
            .iter()
            .rev()
            .find(|node| node.node_id == node_id)
            .map(|node| node.status)
    }
}

/// Generic execution event for observability consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagExecutionEvent {
    /// Event severity level.
    pub level: String,
    /// Stable event type, for example `node.completed`.
    pub event_type: String,
    /// Node id associated with this event, when applicable.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Human-readable event message.
    #[serde(default)]
    pub message: Option<String>,
    /// Structured event payload.
    #[serde(default)]
    pub payload: BTreeMap<String, serde_json::Value>,
}

type DagEventSink = Arc<dyn Fn(DagExecutionEvent) + Send + Sync>;

/// Cloneable operator cancellation signal for a DAG executor.
#[derive(Clone, Default)]
pub struct DagCancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl DagCancellationToken {
    /// Build a new cancellation token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation for all executor clones holding this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Return whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Manifest-driven executor for one DAG app handler.
#[derive(Clone)]
pub struct DagExecutor<H> {
    handler: H,
    event_sink: Option<DagEventSink>,
    cancellation_token: Option<DagCancellationToken>,
}

struct ConcurrentNodeOutcome<'a> {
    node: &'a DagNode,
    attempts: Vec<NodeAttemptOutcome>,
}

struct NodeAttemptOutcome {
    attempt: u32,
    result: anyhow::Result<NodeExecutionResult>,
    latency_ms: u64,
    started_event: DagExecutionEvent,
    terminal_event: DagExecutionEvent,
    retry_scheduled_event: Option<DagExecutionEvent>,
}

struct NormalizedNodeResult {
    status: DagNodeStatus,
    warning: Option<String>,
    error: Option<String>,
    produced: Option<DagIo>,
    diagnostics: DagIo,
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    model: Option<String>,
    prompt_hash: Option<String>,
    trace: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone)]
struct ReplayCheckpoint {
    outputs: DagIo,
    node_reports: HashMap<String, DagNodeReport>,
    child_reports: HashMap<String, Vec<DagNodeReport>>,
}

#[derive(Clone, Default)]
struct NodeExecutionProvenance {
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    model: Option<String>,
    prompt_hash: Option<String>,
    diagnostic_refs: BTreeMap<String, String>,
    trace: BTreeMap<String, serde_json::Value>,
}

impl<H> DagExecutor<H>
where
    H: NodeHandler + Clone + Send + Sync + 'static,
{
    /// Build an executor using the supplied app-side handler.
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            event_sink: None,
            cancellation_token: None,
        }
    }

    /// Attach a live event sink that receives structured execution events as
    /// the executor observes them.
    pub fn with_event_sink<F>(mut self, sink: F) -> Self
    where
        F: Fn(DagExecutionEvent) + Send + Sync + 'static,
    {
        self.event_sink = Some(Arc::new(sink));
        self
    }

    /// Attach a cooperative cancellation token checked before queued work is
    /// scheduled. Running node handlers are allowed to finish; unstarted nodes
    /// are reported as skipped with explicit cancellation trace data.
    pub fn with_cancellation_token(mut self, token: DagCancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Execute a validated manifest.
    pub async fn execute(
        &self,
        manifest: &DagManifest,
        input: DagIo,
    ) -> anyhow::Result<DagExecutionReport> {
        self.execute_with_checkpoint(manifest, input, None).await
    }

    /// Execute a validated manifest, replaying completed node outputs from a
    /// prior report when the checkpoint manifest hash still matches.
    pub async fn execute_with_checkpoint(
        &self,
        manifest: &DagManifest,
        input: DagIo,
        checkpoint: Option<&DagExecutionReport>,
    ) -> anyhow::Result<DagExecutionReport> {
        manifest.validate()?;

        let manifest_hash = manifest_hash(manifest)?;
        let replay_checkpoint = prepare_replay_checkpoint(manifest, &manifest_hash, checkpoint)?;
        let frozen_input = if let Some(checkpoint) = checkpoint {
            let mut frozen = checkpoint.input.clone();
            frozen.merge(input.clone());
            frozen
        } else {
            input.clone()
        };
        let deps = dependency_map(manifest);
        let node_by_id: HashMap<&str, &DagNode> = manifest
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let mut statuses: HashMap<String, DagNodeStatus> = HashMap::new();
        let mut branch_selections: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut outputs = if let (Some(checkpoint), Some(replay_checkpoint)) =
            (checkpoint, replay_checkpoint.as_ref())
        {
            let mut outputs = checkpoint.input.clone();
            outputs.merge(replay_checkpoint.outputs.clone());
            outputs.merge(input);
            outputs
        } else {
            input
        };
        let mut reports = Vec::new();
        let started_event = dag_started_event(manifest, &manifest_hash, &frozen_input);
        emit_to_sink(&self.event_sink, &started_event);
        let mut events = vec![started_event];

        for layer in manifest.execution_layers()? {
            if self.is_cancelled() {
                return Ok(self.cancelled_execution_report(
                    manifest,
                    manifest_hash,
                    frozen_input,
                    reports,
                    outputs,
                    events,
                    &statuses,
                ));
            }

            let mut layer_nodes: Vec<&DagNode> = layer
                .iter()
                .map(|node_id| {
                    node_by_id.get(node_id.as_str()).copied().ok_or_else(|| {
                        anyhow::anyhow!("manifest layer referenced missing node `{node_id}`")
                    })
                })
                .collect::<anyhow::Result<_>>()?;

            if let Some(replay_checkpoint) = replay_checkpoint.as_ref() {
                let mut remaining = Vec::with_capacity(layer_nodes.len());
                for node in layer_nodes {
                    if let Some(checkpoint_report) = replay_checkpoint.node_reports.get(&node.id) {
                        statuses.insert(node.id.clone(), checkpoint_report.status);
                        if node.kind == DagNodeKind::Branch {
                            if let Some(selected) =
                                branch_selection_from_replay_report(checkpoint_report)
                            {
                                branch_selections.insert(node.id.clone(), selected);
                            }
                        }
                        if let Some(child_reports) = replay_checkpoint.child_reports.get(&node.id) {
                            for child_report in child_reports {
                                let replayed = replayed_synthetic_child_report(child_report);
                                push_and_emit_node_report_events(
                                    &mut events,
                                    &self.event_sink,
                                    manifest,
                                    &manifest_hash,
                                    &replayed,
                                    &frozen_input,
                                );
                                reports.push(replayed);
                            }
                        }
                        let replayed =
                            replayed_node_report(manifest, node, checkpoint_report, &outputs);
                        push_and_emit_node_report_events(
                            &mut events,
                            &self.event_sink,
                            manifest,
                            &manifest_hash,
                            &replayed,
                            &frozen_input,
                        );
                        reports.push(replayed);
                    } else {
                        remaining.push(node);
                    }
                }
                layer_nodes = remaining;
                if layer_nodes.is_empty() {
                    continue;
                }
            }

            if can_execute_layer_concurrently(
                manifest,
                &layer_nodes,
                &deps,
                &statuses,
                &branch_selections,
            ) {
                let snapshot = outputs.clone();
                let outcomes = self
                    .execute_ordinary_nodes_concurrently(
                        manifest,
                        &manifest_hash,
                        &layer_nodes,
                        &snapshot,
                    )
                    .await?;
                let mut awaiting_approval = false;
                let mut failed = false;

                for outcome in outcomes {
                    let node = outcome.node;
                    let attempt_count = outcome.attempts.len();
                    for (index, attempt) in outcome.attempts.into_iter().enumerate() {
                        let is_final_attempt = index + 1 == attempt_count;
                        events.push(attempt.started_event.clone());
                        events.push(attempt.terminal_event.clone());
                        if let Some(event) = attempt.retry_scheduled_event.clone() {
                            events.push(event);
                        }
                        let normalized = normalize_node_result(node, attempt.result);
                        let output_refs =
                            produced_output_refs(normalized.status, normalized.produced.as_ref());
                        let diagnostic_refs = diagnostic_refs(&normalized.diagnostics);

                        if is_final_attempt {
                            if matches!(
                                normalized.status,
                                DagNodeStatus::Ok | DagNodeStatus::Degraded
                            ) {
                                if let Some(produced) = normalized.produced {
                                    outputs.merge(produced);
                                }
                                consume_node_budget(&mut outputs, manifest, node);
                            }
                            statuses.insert(node.id.clone(), normalized.status);
                            awaiting_approval |=
                                normalized.status == DagNodeStatus::AwaitingApproval;
                            failed |= normalized.status == DagNodeStatus::Failed;
                        }

                        reports.push(node_report(
                            manifest,
                            node,
                            attempt.attempt,
                            normalized.status,
                            node_executor_label(manifest, node),
                            &snapshot,
                            normalized.warning,
                            normalized.error,
                            Some(attempt.latency_ms),
                            merge_trace(
                                BTreeMap::from([(
                                    "scheduler".to_string(),
                                    serde_json::json!("tokio_concurrent_layer"),
                                )]),
                                normalized.trace,
                            ),
                            output_refs,
                            diagnostic_refs,
                            normalized.command,
                            normalized.exit_status,
                            normalized.model,
                            normalized.prompt_hash,
                        ));
                    }
                }

                if awaiting_approval && !failed {
                    return Ok(self.execution_report(
                        manifest,
                        manifest_hash,
                        DagNodeStatus::AwaitingApproval,
                        frozen_input,
                        reports,
                        outputs,
                        events,
                    ));
                }

                continue;
            }

            for node_id in layer {
                if self.is_cancelled() {
                    return Ok(self.cancelled_execution_report(
                        manifest,
                        manifest_hash,
                        frozen_input,
                        reports,
                        outputs,
                        events,
                        &statuses,
                    ));
                }

                let node = node_by_id.get(node_id.as_str()).copied().ok_or_else(|| {
                    anyhow::anyhow!("manifest layer referenced missing node `{node_id}`")
                })?;

                if is_unselected_by_branch(node, &deps, &branch_selections) {
                    statuses.insert(node.id.clone(), DagNodeStatus::Skipped);
                    reports.push(node_report(
                        manifest,
                        node,
                        1,
                        DagNodeStatus::Skipped,
                        None,
                        &outputs,
                        Some("branch not selected".to_string()),
                        None,
                        Some(0),
                        BTreeMap::from([(
                            "skip_reason".to_string(),
                            serde_json::json!("branch_not_selected"),
                        )]),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    continue;
                }

                if has_blocking_dependency(node, &deps, &statuses) {
                    statuses.insert(node.id.clone(), DagNodeStatus::Skipped);
                    reports.push(node_report(
                        manifest,
                        node,
                        1,
                        DagNodeStatus::Skipped,
                        None,
                        &outputs,
                        None,
                        Some(
                            "required dependency failed, was skipped, or awaits approval"
                                .to_string(),
                        ),
                        Some(0),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    continue;
                }

                if node.kind == DagNodeKind::Branch {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (node_result, selected) = evaluate_branch(node, &snapshot);
                    let trace = branch_trace(node, &snapshot, selected.as_ref());
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    let normalized = normalize_node_result(node, node_result);
                    let output_refs =
                        produced_output_refs(normalized.status, normalized.produced.as_ref());
                    let diagnostic_refs = diagnostic_refs(&normalized.diagnostics);

                    if matches!(
                        normalized.status,
                        DagNodeStatus::Ok | DagNodeStatus::Degraded
                    ) {
                        if let Some(selected) = selected {
                            branch_selections.insert(node.id.clone(), selected);
                        }
                        if let Some(produced) = normalized.produced {
                            outputs.merge(produced);
                        }
                        consume_node_budget(&mut outputs, manifest, node);
                    }

                    statuses.insert(node.id.clone(), normalized.status);
                    reports.push(node_report(
                        manifest,
                        node,
                        1,
                        normalized.status,
                        node_executor_label(manifest, node),
                        &snapshot,
                        normalized.warning,
                        normalized.error,
                        Some(latency_ms),
                        trace,
                        output_refs,
                        diagnostic_refs,
                        normalized.command,
                        normalized.exit_status,
                        normalized.model,
                        normalized.prompt_hash,
                    ));
                    continue;
                }

                if node.kind == DagNodeKind::Loop {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (status, warning, error, produced, mut round_reports, provenance) =
                        self.execute_loop_node(manifest, node, &snapshot).await;
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    let output_refs = produced_output_refs(status, produced.as_ref());

                    if let Some(produced) = produced {
                        outputs.merge(produced);
                    }
                    consume_node_budget(&mut outputs, manifest, node);

                    statuses.insert(node.id.clone(), status);
                    for report in &round_reports {
                        push_and_emit_event(
                            &mut events,
                            &self.event_sink,
                            node_report_started_event(
                                manifest,
                                &manifest_hash,
                                report,
                                &frozen_input,
                            ),
                        );
                        push_and_emit_event(
                            &mut events,
                            &self.event_sink,
                            node_report_event(manifest, &manifest_hash, report, &frozen_input),
                        );
                    }
                    reports.append(&mut round_reports);
                    let aggregate_report = node_report(
                        manifest,
                        node,
                        1,
                        status,
                        node_executor_label(manifest, node),
                        &snapshot,
                        warning,
                        error,
                        Some(latency_ms),
                        provenance.trace,
                        output_refs,
                        provenance.diagnostic_refs,
                        provenance.command,
                        provenance.exit_status,
                        provenance.model,
                        provenance.prompt_hash,
                    );
                    push_and_emit_event(
                        &mut events,
                        &self.event_sink,
                        node_report_started_event(
                            manifest,
                            &manifest_hash,
                            &aggregate_report,
                            &frozen_input,
                        ),
                    );
                    push_and_emit_event(
                        &mut events,
                        &self.event_sink,
                        node_report_event(
                            manifest,
                            &manifest_hash,
                            &aggregate_report,
                            &frozen_input,
                        ),
                    );
                    reports.push(aggregate_report);
                    if status == DagNodeStatus::AwaitingApproval {
                        return Ok(self.execution_report(
                            manifest,
                            manifest_hash,
                            DagNodeStatus::AwaitingApproval,
                            frozen_input,
                            reports,
                            outputs,
                            events,
                        ));
                    }
                    continue;
                }

                if node.kind == DagNodeKind::Map {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (status, warning, error, produced, mut item_reports, provenance) =
                        self.execute_map_node(manifest, node, &snapshot).await;
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    let output_refs = produced_output_refs(status, produced.as_ref());

                    if let Some(produced) = produced {
                        outputs.merge(produced);
                    }
                    consume_node_budget(&mut outputs, manifest, node);

                    statuses.insert(node.id.clone(), status);
                    reports.append(&mut item_reports);
                    reports.push(node_report(
                        manifest,
                        node,
                        1,
                        status,
                        node_executor_label(manifest, node),
                        &snapshot,
                        warning,
                        error,
                        Some(latency_ms),
                        provenance.trace,
                        output_refs,
                        provenance.diagnostic_refs,
                        provenance.command,
                        provenance.exit_status,
                        provenance.model,
                        provenance.prompt_hash,
                    ));
                    if status == DagNodeStatus::AwaitingApproval {
                        return Ok(self.execution_report(
                            manifest,
                            manifest_hash,
                            DagNodeStatus::AwaitingApproval,
                            frozen_input,
                            reports,
                            outputs,
                            events,
                        ));
                    }
                    continue;
                }

                let started = Instant::now();
                let snapshot = outputs.clone();
                let node_result = if node.kind == DagNodeKind::Gate {
                    evaluate_gate(node, &statuses)
                } else if node.kind == DagNodeKind::Approval {
                    evaluate_approval(node, &snapshot)
                } else {
                    let attempts = execute_handler_node_attempts(
                        self.handler.clone(),
                        manifest.clone(),
                        manifest_hash.clone(),
                        node.clone(),
                        snapshot.clone(),
                        self.event_sink.clone(),
                    )
                    .await;
                    let attempt_count = attempts.len();
                    for (index, attempt) in attempts.into_iter().enumerate() {
                        let is_final_attempt = index + 1 == attempt_count;
                        events.push(attempt.started_event.clone());
                        events.push(attempt.terminal_event.clone());
                        if let Some(event) = attempt.retry_scheduled_event.clone() {
                            events.push(event);
                        }
                        let normalized = normalize_node_result(node, attempt.result);
                        let output_refs =
                            produced_output_refs(normalized.status, normalized.produced.as_ref());
                        let diagnostic_refs = diagnostic_refs(&normalized.diagnostics);

                        if is_final_attempt {
                            if matches!(
                                normalized.status,
                                DagNodeStatus::Ok | DagNodeStatus::Degraded
                            ) {
                                if let Some(produced) = normalized.produced {
                                    outputs.merge(produced);
                                }
                                consume_node_budget(&mut outputs, manifest, node);
                            }

                            statuses.insert(node.id.clone(), normalized.status);
                        }

                        reports.push(node_report(
                            manifest,
                            node,
                            attempt.attempt,
                            normalized.status,
                            node_executor_label(manifest, node),
                            &snapshot,
                            normalized.warning,
                            normalized.error,
                            Some(attempt.latency_ms),
                            normalized.trace,
                            output_refs,
                            diagnostic_refs,
                            normalized.command,
                            normalized.exit_status,
                            normalized.model,
                            normalized.prompt_hash,
                        ));

                        if is_final_attempt && normalized.status == DagNodeStatus::AwaitingApproval
                        {
                            return Ok(self.execution_report(
                                manifest,
                                manifest_hash,
                                DagNodeStatus::AwaitingApproval,
                                frozen_input,
                                reports,
                                outputs,
                                events,
                            ));
                        }
                    }
                    continue;
                };
                let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

                let normalized = normalize_node_result(node, node_result);
                let output_refs =
                    produced_output_refs(normalized.status, normalized.produced.as_ref());
                let diagnostic_refs = diagnostic_refs(&normalized.diagnostics);

                if matches!(
                    normalized.status,
                    DagNodeStatus::Ok | DagNodeStatus::Degraded
                ) {
                    if let Some(produced) = normalized.produced {
                        outputs.merge(produced);
                    }
                    consume_node_budget(&mut outputs, manifest, node);
                }

                statuses.insert(node.id.clone(), normalized.status);
                reports.push(node_report(
                    manifest,
                    node,
                    1,
                    normalized.status,
                    node_executor_label(manifest, node),
                    &snapshot,
                    normalized.warning,
                    normalized.error,
                    Some(latency_ms),
                    merge_trace(approval_trace(node, &snapshot), normalized.trace),
                    output_refs,
                    diagnostic_refs,
                    normalized.command,
                    normalized.exit_status,
                    normalized.model,
                    normalized.prompt_hash,
                ));

                if normalized.status == DagNodeStatus::AwaitingApproval {
                    return Ok(self.execution_report(
                        manifest,
                        manifest_hash,
                        DagNodeStatus::AwaitingApproval,
                        frozen_input,
                        reports,
                        outputs,
                        events,
                    ));
                }
            }
        }

        let status = overall_status(&reports);
        Ok(self.execution_report(
            manifest,
            manifest_hash,
            status,
            frozen_input,
            reports,
            outputs,
            events,
        ))
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .as_ref()
            .map(DagCancellationToken::is_cancelled)
            .unwrap_or(false)
    }

    fn execution_report(
        &self,
        manifest: &DagManifest,
        manifest_hash: String,
        status: DagNodeStatus,
        input: DagIo,
        nodes: Vec<DagNodeReport>,
        outputs: DagIo,
        events: Vec<DagExecutionEvent>,
    ) -> DagExecutionReport {
        let terminal_event = dag_terminal_event(manifest, &manifest_hash, status, nodes.len());
        let mut terminal_event = with_runtime_identity(terminal_event, &input);
        normalize_agenthero_trace_event(&mut terminal_event);
        let mut report_events = execution_events(manifest, &manifest_hash, &events, &nodes, &input);
        emit_to_sink(&self.event_sink, &terminal_event);
        report_events.push(terminal_event);
        DagExecutionReport {
            dag_type: manifest.id.clone(),
            manifest_version: manifest.version,
            manifest_hash,
            status,
            input,
            nodes,
            outputs,
            events: report_events,
        }
    }

    fn cancelled_execution_report(
        &self,
        manifest: &DagManifest,
        manifest_hash: String,
        input: DagIo,
        mut reports: Vec<DagNodeReport>,
        outputs: DagIo,
        mut events: Vec<DagExecutionEvent>,
        statuses: &HashMap<String, DagNodeStatus>,
    ) -> DagExecutionReport {
        for node in &manifest.nodes {
            if statuses.contains_key(&node.id) {
                continue;
            }
            let warning = "operator cancellation requested".to_string();
            reports.push(node_report(
                manifest,
                node,
                1,
                DagNodeStatus::Skipped,
                node_executor_label(manifest, node),
                &outputs,
                Some(warning.clone()),
                None,
                Some(0),
                BTreeMap::from([
                    ("cancelled".to_string(), serde_json::json!(true)),
                    (
                        "skip_reason".to_string(),
                        serde_json::json!("operator_cancelled"),
                    ),
                ]),
                BTreeMap::new(),
                BTreeMap::new(),
                None,
                None,
                None,
                None,
            ));
            let event = with_manifest_identity(
                node_status_event(
                    node.id.clone(),
                    node.kind.to_string(),
                    1,
                    DagNodeStatus::Skipped,
                    Some(warning),
                ),
                manifest,
                &manifest_hash,
            );
            let mut event = event;
            add_node_static_audit_payload(&mut event.payload, manifest, node);
            push_and_emit_event(&mut events, &self.event_sink, event);
        }

        let mut terminal_event =
            dag_cancelled_event(manifest, &manifest_hash, reports.len(), &input);
        normalize_agenthero_trace_event(&mut terminal_event);
        let mut report_events =
            execution_events(manifest, &manifest_hash, &events, &reports, &input);
        emit_to_sink(&self.event_sink, &terminal_event);
        report_events.push(terminal_event);
        DagExecutionReport {
            dag_type: manifest.id.clone(),
            manifest_version: manifest.version,
            manifest_hash,
            status: DagNodeStatus::Skipped,
            input,
            nodes: reports,
            outputs,
            events: report_events,
        }
    }

    async fn execute_ordinary_nodes_concurrently<'a>(
        &self,
        manifest: &DagManifest,
        manifest_hash: &str,
        nodes: &[&'a DagNode],
        inputs: &DagIo,
    ) -> anyhow::Result<Vec<ConcurrentNodeOutcome<'a>>> {
        let limit = manifest
            .concurrency
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(nodes.len())
            .min(nodes.len());
        let semaphore = Arc::new(Semaphore::new(limit));
        let mut handles = Vec::with_capacity(nodes.len());

        for (index, node) in nodes.iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await?;
            let handler = self.handler.clone();
            let manifest_owned = manifest.clone();
            let manifest_hash_owned = manifest_hash.to_string();
            let node_owned = (*node).clone();
            let inputs_owned = inputs.clone();
            let event_sink = self.event_sink.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                let attempts = execute_handler_node_attempts(
                    handler,
                    manifest_owned,
                    manifest_hash_owned,
                    node_owned,
                    inputs_owned,
                    event_sink,
                )
                .await;
                (index, attempts)
            }));
        }

        let mut by_index: Vec<Option<Vec<NodeAttemptOutcome>>> =
            (0..nodes.len()).map(|_| None).collect();
        for handle in handles {
            let (index, attempts) = handle.await?;
            by_index[index] = Some(attempts);
        }

        by_index
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                let attempts = outcome.ok_or_else(|| {
                    anyhow::anyhow!("concurrent node `{}` did not return", nodes[index].id)
                })?;
                Ok(ConcurrentNodeOutcome {
                    node: nodes[index],
                    attempts,
                })
            })
            .collect()
    }

    async fn execute_loop_node(
        &self,
        manifest: &DagManifest,
        node: &DagNode,
        inputs: &DagIo,
    ) -> (
        DagNodeStatus,
        Option<String>,
        Option<String>,
        Option<DagIo>,
        Vec<DagNodeReport>,
        NodeExecutionProvenance,
    ) {
        let Some(policy) = node.loop_policy.as_ref() else {
            return (
                DagNodeStatus::Failed,
                None,
                Some(format!("loop node `{}` has no loop policy", node.id)),
                None,
                Vec::new(),
                NodeExecutionProvenance::default(),
            );
        };

        let mut visible_inputs = inputs.clone();
        let mut combined_outputs = DagIo::default();
        let mut round_reports = Vec::new();
        let mut saw_degraded = false;
        let mut last_provenance = NodeExecutionProvenance::default();

        for round in 1..=policy.max_rounds {
            let mut round_inputs = visible_inputs.clone();
            round_inputs
                .values
                .insert(LOOP_ROUND_INPUT.to_string(), serde_json::json!(round));
            round_inputs.values.insert(
                LOOP_MAX_ROUNDS_INPUT.to_string(),
                serde_json::json!(policy.max_rounds),
            );
            round_inputs
                .values
                .insert(LOOP_NODE_ID_INPUT.to_string(), serde_json::json!(node.id));

            let started = Instant::now();
            let node_result =
                execute_handler_node_once(&self.handler, manifest, node, &round_inputs).await;
            let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

            match node_result {
                Ok(result) => {
                    let normalized = normalize_node_result(node, Ok(result));
                    let status = normalized.status;
                    saw_degraded |= status == DagNodeStatus::Degraded;
                    let continue_requested = normalized
                        .produced
                        .as_ref()
                        .and_then(|produced| {
                            produced
                                .values
                                .get(&policy.continue_key)
                                .and_then(serde_json::Value::as_bool)
                        })
                        .unwrap_or(false);
                    let warning = normalized.warning.clone();
                    let error = normalized.error.clone();
                    let output_refs = produced_output_refs(status, normalized.produced.as_ref());
                    let round_diagnostic_refs = diagnostic_refs(&normalized.diagnostics);
                    last_provenance = NodeExecutionProvenance {
                        command: normalized.command.clone(),
                        exit_status: normalized.exit_status,
                        model: normalized.model.clone(),
                        prompt_hash: normalized.prompt_hash.clone(),
                        diagnostic_refs: round_diagnostic_refs.clone(),
                        trace: normalized.trace.clone(),
                    };

                    if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                        if let Some(produced) = normalized.produced {
                            visible_inputs.merge(produced.clone());
                            combined_outputs.merge(produced);
                        }
                    }

                    round_reports.push(loop_round_report(
                        manifest,
                        node,
                        round,
                        status,
                        warning.clone(),
                        error.clone(),
                        latency_ms,
                        output_refs,
                        round_diagnostic_refs,
                        normalized.trace,
                        normalized.command,
                        normalized.exit_status,
                        normalized.model,
                        normalized.prompt_hash,
                    ));

                    if status == DagNodeStatus::AwaitingApproval {
                        return (
                            DagNodeStatus::AwaitingApproval,
                            warning,
                            None,
                            Some(combined_outputs),
                            round_reports,
                            last_provenance,
                        );
                    }

                    if status == DagNodeStatus::Failed {
                        return (
                            DagNodeStatus::Failed,
                            warning,
                            Some(error.unwrap_or_else(|| {
                                format!("loop node `{}` failed in round {round}", node.id)
                            })),
                            Some(combined_outputs),
                            round_reports,
                            last_provenance,
                        );
                    }

                    if !continue_requested {
                        let final_status = if saw_degraded {
                            DagNodeStatus::Degraded
                        } else {
                            DagNodeStatus::Ok
                        };
                        return (
                            final_status,
                            warning,
                            None,
                            Some(combined_outputs),
                            round_reports,
                            last_provenance,
                        );
                    }

                    if round == policy.max_rounds {
                        let message = format!(
                            "loop node `{}` exhausted max_rounds={} while `{}` remained true",
                            node.id, policy.max_rounds, policy.continue_key
                        );
                        let status = if node.required {
                            DagNodeStatus::Failed
                        } else {
                            DagNodeStatus::Degraded
                        };
                        return (
                            status,
                            (!node.required).then_some(message.clone()),
                            node.required.then_some(message),
                            Some(combined_outputs),
                            round_reports,
                            last_provenance,
                        );
                    }
                }
                Err(err) if node.required => {
                    let error = format!("{err:#}");
                    round_reports.push(loop_round_report(
                        manifest,
                        node,
                        round,
                        DagNodeStatus::Failed,
                        None,
                        Some(error.clone()),
                        latency_ms,
                        BTreeMap::new(),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    return (
                        DagNodeStatus::Failed,
                        None,
                        Some(error),
                        Some(combined_outputs),
                        round_reports,
                        last_provenance,
                    );
                }
                Err(err) => {
                    let warning = format!("{err:#}");
                    round_reports.push(loop_round_report(
                        manifest,
                        node,
                        round,
                        DagNodeStatus::Degraded,
                        Some(warning.clone()),
                        None,
                        latency_ms,
                        BTreeMap::new(),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    return (
                        DagNodeStatus::Degraded,
                        Some(warning),
                        None,
                        Some(combined_outputs),
                        round_reports,
                        last_provenance,
                    );
                }
            }
        }

        (
            if saw_degraded {
                DagNodeStatus::Degraded
            } else {
                DagNodeStatus::Ok
            },
            None,
            None,
            Some(combined_outputs),
            round_reports,
            last_provenance,
        )
    }

    async fn execute_map_node(
        &self,
        manifest: &DagManifest,
        node: &DagNode,
        inputs: &DagIo,
    ) -> (
        DagNodeStatus,
        Option<String>,
        Option<String>,
        Option<DagIo>,
        Vec<DagNodeReport>,
        NodeExecutionProvenance,
    ) {
        let Some(policy) = node.map.as_ref() else {
            return (
                DagNodeStatus::Failed,
                None,
                Some(format!("map node `{}` has no map policy", node.id)),
                None,
                Vec::new(),
                NodeExecutionProvenance::default(),
            );
        };
        let Some(items) = inputs
            .values
            .get(&policy.items_key)
            .and_then(serde_json::Value::as_array)
        else {
            let message = format!(
                "map node `{}` expected array input `{}`",
                node.id, policy.items_key
            );
            let status = if node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            return (
                status,
                (!node.required).then_some(message.clone()),
                node.required.then_some(message),
                None,
                Vec::new(),
                NodeExecutionProvenance::default(),
            );
        };
        if items.len() > policy.max_items as usize {
            let message = format!(
                "map node `{}` received {} items, above max_items={}",
                node.id,
                items.len(),
                policy.max_items
            );
            let status = if node.required {
                DagNodeStatus::Failed
            } else {
                DagNodeStatus::Degraded
            };
            return (
                status,
                (!node.required).then_some(message.clone()),
                node.required.then_some(message),
                None,
                Vec::new(),
                NodeExecutionProvenance::default(),
            );
        }

        let mut visible_inputs = inputs.clone();
        let mut combined_outputs = DagIo::default();
        let mut item_reports = Vec::new();
        let mut saw_degraded = false;
        let mut last_provenance = NodeExecutionProvenance::default();

        for (index, item) in items.iter().enumerate() {
            let mut item_inputs = visible_inputs.clone();
            item_inputs
                .values
                .insert(policy.item_key.clone(), item.clone());
            item_inputs
                .values
                .insert(policy.index_key.clone(), serde_json::json!(index));
            item_inputs
                .values
                .insert(MAP_INDEX_INPUT.to_string(), serde_json::json!(index));
            item_inputs.values.insert(
                MAP_MAX_ITEMS_INPUT.to_string(),
                serde_json::json!(policy.max_items),
            );
            item_inputs
                .values
                .insert(MAP_NODE_ID_INPUT.to_string(), serde_json::json!(node.id));

            let started = Instant::now();
            let node_result =
                execute_handler_node_once(&self.handler, manifest, node, &item_inputs).await;
            let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

            match node_result {
                Ok(result) => {
                    let normalized = normalize_node_result(node, Ok(result));
                    let status = normalized.status;
                    saw_degraded |= status == DagNodeStatus::Degraded;
                    let warning = normalized.warning.clone();
                    let error = normalized.error.clone();
                    let output_refs = produced_output_refs(status, normalized.produced.as_ref());
                    let item_diagnostic_refs = diagnostic_refs(&normalized.diagnostics);
                    last_provenance = NodeExecutionProvenance {
                        command: normalized.command.clone(),
                        exit_status: normalized.exit_status,
                        model: normalized.model.clone(),
                        prompt_hash: normalized.prompt_hash.clone(),
                        diagnostic_refs: item_diagnostic_refs.clone(),
                        trace: normalized.trace.clone(),
                    };

                    if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                        if let Some(produced) = normalized.produced {
                            visible_inputs.merge(produced.clone());
                            combined_outputs.merge(produced);
                        }
                    }
                    item_reports.push(map_item_report(
                        manifest,
                        node,
                        index,
                        status,
                        warning.clone(),
                        error.clone(),
                        latency_ms,
                        output_refs,
                        item_diagnostic_refs,
                        normalized.trace,
                        normalized.command,
                        normalized.exit_status,
                        normalized.model,
                        normalized.prompt_hash,
                    ));
                    if status == DagNodeStatus::AwaitingApproval {
                        return (
                            DagNodeStatus::AwaitingApproval,
                            warning,
                            None,
                            Some(combined_outputs),
                            item_reports,
                            last_provenance,
                        );
                    }
                    if status == DagNodeStatus::Failed {
                        return (
                            DagNodeStatus::Failed,
                            warning,
                            Some(error.unwrap_or_else(|| {
                                format!("map node `{}` failed at item {index}", node.id)
                            })),
                            Some(combined_outputs),
                            item_reports,
                            last_provenance,
                        );
                    }
                }
                Err(err) if node.required => {
                    let error = format!("{err:#}");
                    item_reports.push(map_item_report(
                        manifest,
                        node,
                        index,
                        DagNodeStatus::Failed,
                        None,
                        Some(error.clone()),
                        latency_ms,
                        BTreeMap::new(),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    return (
                        DagNodeStatus::Failed,
                        None,
                        Some(error),
                        Some(combined_outputs),
                        item_reports,
                        last_provenance,
                    );
                }
                Err(err) => {
                    let warning = format!("{err:#}");
                    item_reports.push(map_item_report(
                        manifest,
                        node,
                        index,
                        DagNodeStatus::Degraded,
                        Some(warning.clone()),
                        None,
                        latency_ms,
                        BTreeMap::new(),
                        BTreeMap::new(),
                        BTreeMap::new(),
                        None,
                        None,
                        None,
                        None,
                    ));
                    return (
                        DagNodeStatus::Degraded,
                        Some(warning),
                        None,
                        Some(combined_outputs),
                        item_reports,
                        last_provenance,
                    );
                }
            }
        }

        (
            if saw_degraded {
                DagNodeStatus::Degraded
            } else {
                DagNodeStatus::Ok
            },
            None,
            None,
            Some(combined_outputs),
            item_reports,
            last_provenance,
        )
    }
}

fn loop_round_report(
    manifest: &DagManifest,
    node: &DagNode,
    round: u32,
    status: DagNodeStatus,
    warning: Option<String>,
    error: Option<String>,
    latency_ms: u64,
    output_refs: BTreeMap<String, String>,
    diagnostic_refs: BTreeMap<String, String>,
    trace: BTreeMap<String, serde_json::Value>,
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    model: Option<String>,
    prompt_hash: Option<String>,
) -> DagNodeReport {
    DagNodeReport {
        node_id: format!("{}#round-{round}", node.id),
        kind: node.kind.to_string(),
        status,
        attempt: round,
        role: node.role.as_ref().map(ToString::to_string),
        tool: node.tool.clone(),
        child_dag_type: node.dag_type.clone(),
        required: node.required,
        executor: node_executor_label(manifest, node),
        model,
        prompt_hash,
        command,
        exit_status,
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        input_refs: BTreeMap::new(),
        output_refs,
        diagnostic_refs,
        policy: BTreeMap::from([("loop_round".to_string(), serde_json::json!(round))]),
        warning,
        error,
        latency_ms: Some(latency_ms),
        trace: merge_trace(
            BTreeMap::from([("loop_round".to_string(), serde_json::json!(round))]),
            trace,
        ),
    }
}

fn map_item_report(
    manifest: &DagManifest,
    node: &DagNode,
    index: usize,
    status: DagNodeStatus,
    warning: Option<String>,
    error: Option<String>,
    latency_ms: u64,
    output_refs: BTreeMap<String, String>,
    diagnostic_refs: BTreeMap<String, String>,
    trace: BTreeMap<String, serde_json::Value>,
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    model: Option<String>,
    prompt_hash: Option<String>,
) -> DagNodeReport {
    DagNodeReport {
        node_id: format!("{}#item-{index}", node.id),
        kind: node.kind.to_string(),
        status,
        attempt: 1,
        role: node.role.as_ref().map(ToString::to_string),
        tool: node.tool.clone(),
        child_dag_type: node.dag_type.clone(),
        required: node.required,
        executor: node_executor_label(manifest, node),
        model,
        prompt_hash,
        command,
        exit_status,
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        input_refs: BTreeMap::new(),
        output_refs,
        diagnostic_refs,
        policy: BTreeMap::from([("map_index".to_string(), serde_json::json!(index))]),
        warning,
        error,
        latency_ms: Some(latency_ms),
        trace: merge_trace(
            BTreeMap::from([("map_index".to_string(), serde_json::json!(index))]),
            trace,
        ),
    }
}

fn node_report(
    manifest: &DagManifest,
    node: &DagNode,
    attempt: u32,
    status: DagNodeStatus,
    executor: Option<String>,
    available_inputs: &DagIo,
    warning: Option<String>,
    error: Option<String>,
    latency_ms: Option<u64>,
    trace: BTreeMap<String, serde_json::Value>,
    output_refs: BTreeMap<String, String>,
    diagnostic_refs: BTreeMap<String, String>,
    command: Option<Vec<String>>,
    exit_status: Option<i32>,
    model: Option<String>,
    prompt_hash: Option<String>,
) -> DagNodeReport {
    DagNodeReport {
        node_id: node.id.clone(),
        kind: node.kind.to_string(),
        status,
        attempt,
        role: node.role.as_ref().map(ToString::to_string),
        tool: node.tool.clone(),
        child_dag_type: node.dag_type.clone(),
        required: node.required,
        executor,
        model,
        prompt_hash,
        command: command.or_else(|| node_command(manifest, node)),
        exit_status,
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        input_refs: input_refs(node, available_inputs),
        output_refs,
        diagnostic_refs,
        policy: node_policy_trace(manifest, node),
        warning,
        error,
        latency_ms,
        trace,
    }
}

fn merge_trace(
    mut base: BTreeMap<String, serde_json::Value>,
    extra: BTreeMap<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    let mut app_trace = serde_json::Map::new();
    for (key, value) in extra {
        if base.contains_key(&key) {
            app_trace.insert(key, value);
        } else {
            base.insert(key, value);
        }
    }
    if !app_trace.is_empty() {
        match base
            .entry("app_trace".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        {
            serde_json::Value::Object(existing) => {
                existing.extend(app_trace);
            }
            existing => {
                *existing = serde_json::Value::Object(app_trace);
            }
        }
    }
    base
}

fn prepare_replay_checkpoint(
    manifest: &DagManifest,
    current_manifest_hash: &str,
    checkpoint: Option<&DagExecutionReport>,
) -> anyhow::Result<Option<ReplayCheckpoint>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };
    if checkpoint.dag_type != manifest.id {
        anyhow::bail!(
            "checkpoint DAG `{}` does not match current DAG `{}`",
            checkpoint.dag_type,
            manifest.id
        );
    }
    if checkpoint.manifest_hash != current_manifest_hash {
        anyhow::bail!(
            "checkpoint manifest hash `{}` does not match current manifest hash `{}`",
            checkpoint.manifest_hash,
            current_manifest_hash
        );
    }

    let node_by_id: HashMap<&str, &DagNode> = manifest
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut node_reports = HashMap::new();
    for report in checkpoint.nodes.iter().rev() {
        if node_reports.contains_key(&report.node_id) {
            continue;
        }
        let Some(node) = node_by_id.get(report.node_id.as_str()).copied() else {
            continue;
        };
        if !is_replayable_status(report.status) {
            continue;
        }
        validate_checkpoint_node_report(node, report)?;
        node_reports.insert(report.node_id.clone(), report.clone());
    }

    let mut child_reports: HashMap<String, Vec<DagNodeReport>> = HashMap::new();
    for report in &checkpoint.nodes {
        let Some(parent_id) = replay_child_parent_node_id(&report.node_id) else {
            continue;
        };
        let Some(node) = node_by_id.get(parent_id).copied() else {
            continue;
        };
        if !node_reports.contains_key(parent_id) || !is_replayable_status(report.status) {
            continue;
        }
        validate_checkpoint_node_report(node, report)?;
        child_reports
            .entry(parent_id.to_string())
            .or_default()
            .push(report.clone());
    }

    let outputs =
        checkpoint_replay_outputs(checkpoint, &node_reports, &child_reports, &node_by_id)?;

    Ok(Some(ReplayCheckpoint {
        outputs,
        node_reports,
        child_reports,
    }))
}

fn is_replayable_status(status: DagNodeStatus) -> bool {
    matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded)
}

fn checkpoint_replay_outputs(
    checkpoint: &DagExecutionReport,
    node_reports: &HashMap<String, DagNodeReport>,
    child_reports: &HashMap<String, Vec<DagNodeReport>>,
    node_by_id: &HashMap<&str, &DagNode>,
) -> anyhow::Result<DagIo> {
    let mut safe_value_keys = BTreeSet::new();
    let mut outputs = DagIo::default();

    for (node_id, report) in node_reports {
        let Some(node) = node_by_id.get(node_id.as_str()).copied() else {
            continue;
        };
        safe_value_keys.insert(node.id.clone());
        safe_value_keys.extend(node.outputs.iter().cloned());
        for (key, uri) in &report.output_refs {
            let Some(artifact) = checkpoint.outputs.artifacts.get(key) else {
                anyhow::bail!(
                    "checkpoint artifact `{key}` from node `{}` is missing from frozen outputs",
                    report.node_id
                );
            };
            if artifact.uri != *uri {
                anyhow::bail!(
                    "checkpoint artifact `{key}` from node `{}` points to `{uri}` but frozen outputs contain `{}`",
                    report.node_id,
                    artifact.uri
                );
            }
            validate_checkpoint_artifact_integrity(key, &report.node_id, artifact)?;
            outputs.artifacts.insert(key.clone(), artifact.clone());
        }
    }
    for (parent_id, reports) in child_reports {
        let Some(node) = node_by_id.get(parent_id.as_str()).copied() else {
            continue;
        };
        for report in reports {
            safe_value_keys.insert(node.id.clone());
            safe_value_keys.extend(node.outputs.iter().cloned());
            for (key, uri) in &report.output_refs {
                let Some(artifact) = checkpoint.outputs.artifacts.get(key) else {
                    anyhow::bail!(
                        "checkpoint artifact `{key}` from node `{}` is missing from frozen outputs",
                        report.node_id
                    );
                };
                if artifact.uri != *uri {
                    anyhow::bail!(
                        "checkpoint artifact `{key}` from node `{}` points to `{uri}` but frozen outputs contain `{}`",
                        report.node_id,
                        artifact.uri
                    );
                }
                validate_checkpoint_artifact_integrity(key, &report.node_id, artifact)?;
                outputs.artifacts.insert(key.clone(), artifact.clone());
            }
        }
    }

    for key in safe_value_keys {
        if let Some(value) = checkpoint.outputs.values.get(&key) {
            outputs.values.insert(key, value.clone());
        }
    }

    Ok(outputs)
}

fn validate_checkpoint_node_report(node: &DagNode, report: &DagNodeReport) -> anyhow::Result<()> {
    let expected_kind = node.kind.to_string();
    if report.kind != expected_kind {
        anyhow::bail!(
            "checkpoint node `{}` kind `{}` does not match manifest kind `{}`",
            report.node_id,
            report.kind,
            expected_kind
        );
    }

    let mut outputs = DagIo::default();
    for (key, uri) in &report.output_refs {
        outputs.artifacts.insert(
            key.clone(),
            ArtifactRef {
                uri: uri.clone(),
                media_type: None,
                metadata: BTreeMap::new(),
            },
        );
    }
    validate_node_artifact_output_contract(node, &outputs)?;
    Ok(())
}

fn replay_child_parent_node_id(node_id: &str) -> Option<&str> {
    node_id
        .split_once("#round-")
        .map(|(parent, _)| parent)
        .or_else(|| node_id.split_once("#item-").map(|(parent, _)| parent))
}

fn replayed_node_report(
    manifest: &DagManifest,
    node: &DagNode,
    checkpoint_report: &DagNodeReport,
    available_inputs: &DagIo,
) -> DagNodeReport {
    let mut trace = checkpoint_report.trace.clone();
    trace.insert("replay".to_string(), serde_json::json!("checkpoint"));
    trace.insert(
        "checkpoint_attempt".to_string(),
        serde_json::json!(checkpoint_report.attempt),
    );

    node_report(
        manifest,
        node,
        checkpoint_report.attempt,
        checkpoint_report.status,
        checkpoint_report
            .executor
            .clone()
            .or_else(|| node_executor_label(manifest, node)),
        available_inputs,
        checkpoint_report.warning.clone(),
        checkpoint_report.error.clone(),
        Some(0),
        trace,
        checkpoint_report.output_refs.clone(),
        checkpoint_report.diagnostic_refs.clone(),
        checkpoint_report.command.clone(),
        checkpoint_report.exit_status,
        checkpoint_report.model.clone(),
        checkpoint_report.prompt_hash.clone(),
    )
}

fn replayed_synthetic_child_report(checkpoint_report: &DagNodeReport) -> DagNodeReport {
    let mut replayed = checkpoint_report.clone();
    replayed.latency_ms = Some(0);
    replayed
        .trace
        .insert("replay".to_string(), serde_json::json!("checkpoint"));
    replayed.trace.insert(
        "checkpoint_attempt".to_string(),
        serde_json::json!(checkpoint_report.attempt),
    );
    replayed
}

fn branch_selection_from_replay_report(report: &DagNodeReport) -> Option<BTreeSet<String>> {
    report
        .trace
        .get("selected")
        .and_then(serde_json::Value::as_array)
        .map(|selected| {
            selected
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
}

async fn execute_handler_node_attempts<H>(
    handler: H,
    manifest: DagManifest,
    manifest_hash: String,
    node: DagNode,
    inputs: DagIo,
    event_sink: Option<DagEventSink>,
) -> Vec<NodeAttemptOutcome>
where
    H: NodeHandler + Send + Sync + 'static,
{
    let max_attempts = node
        .retry
        .as_ref()
        .map(|retry| retry.max_attempts)
        .unwrap_or(1)
        .max(1);
    let backoff = node
        .retry
        .as_ref()
        .map(|retry| Duration::from_millis(retry.backoff_ms))
        .unwrap_or_default();
    let mut attempts = Vec::new();

    for attempt in 1..=max_attempts {
        let started_event = node_started_event(&manifest, &manifest_hash, &node, attempt, &inputs);
        emit_to_sink(&event_sink, &started_event);
        let started = Instant::now();
        let node_span = node_execution_span(&manifest, &manifest_hash, &node, &inputs, attempt);
        let result = execute_handler_node_once(&handler, &manifest, &node, &inputs)
            .instrument(node_span)
            .await;
        let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        let should_retry = attempt < max_attempts && should_retry_result(&node, &result);
        let terminal_event = node_terminal_event_for_result(
            &manifest,
            &manifest_hash,
            &node,
            attempt,
            latency_ms,
            &result,
            &inputs,
        );
        emit_to_sink(&event_sink, &terminal_event);
        let retry_scheduled_event = should_retry.then(|| {
            node_retry_scheduled_event(
                &manifest,
                &manifest_hash,
                &node,
                attempt,
                attempt.saturating_add(1),
                max_attempts,
                backoff.as_millis().try_into().unwrap_or(u64::MAX),
                &inputs,
            )
        });
        if let Some(event) = retry_scheduled_event.as_ref() {
            emit_to_sink(&event_sink, event);
        }
        attempts.push(NodeAttemptOutcome {
            attempt,
            result,
            latency_ms,
            started_event,
            terminal_event,
            retry_scheduled_event,
        });
        if !should_retry {
            break;
        }
        if !backoff.is_zero() {
            tokio::time::sleep(backoff).await;
        }
    }

    attempts
}

fn node_execution_span(
    manifest: &DagManifest,
    manifest_hash: &str,
    node: &DagNode,
    inputs: &DagIo,
    attempt: u32,
) -> tracing::Span {
    let app_run_id = input_string_value(inputs, "app_run_id").unwrap_or_default();
    let dag_run_id = input_string_value(inputs, "dag_run_id").unwrap_or_default();
    let lease_id = input_string_value(inputs, "lease_id").unwrap_or_default();
    let artifact_id = input_string_value(inputs, "artifact_id").unwrap_or_default();
    let executor = node_executor_label(manifest, node).unwrap_or_default();
    let tool_id = node.tool.as_deref().unwrap_or_default();
    let role = node
        .role
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    let child_dag_type = node.dag_type.as_deref().unwrap_or_default();

    tracing::info_span!(
        target: "agenthero::dag",
        "agenthero.node",
        app_run_id = %app_run_id,
        dag_run_id = %dag_run_id,
        node_id = %node.id,
        attempt = attempt,
        node_kind = %node.kind,
        kind = %node.kind,
        tool_id = %tool_id,
        tool = %tool_id,
        manifest_hash = %manifest_hash,
        dag_type = %manifest.id,
        manifest_version = manifest.version,
        artifact_id = %artifact_id,
        lease_id = %lease_id,
        status = "running",
        exit_status = 0_i64,
        duration_ms = 0_u64,
        executor = %executor,
        role = %role,
        child_dag_type = %child_dag_type,
        required = node.required,
    )
}

fn input_string_value(inputs: &DagIo, key: &str) -> Option<String> {
    inputs
        .values
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn should_retry_result(node: &DagNode, result: &anyhow::Result<NodeExecutionResult>) -> bool {
    node.retry.is_some()
        && match result {
            Ok(result) => matches!(
                normalize_success_status(result.status),
                DagNodeStatus::Failed | DagNodeStatus::Degraded
            ),
            Err(_) => true,
        }
}

async fn execute_handler_node_once<H>(
    handler: &H,
    manifest: &DagManifest,
    node: &DagNode,
    inputs: &DagIo,
) -> anyhow::Result<NodeExecutionResult>
where
    H: NodeHandler + Send + Sync,
{
    validate_node_input_contract(manifest, node, inputs)?;
    if let Some(result) = tool_isolation_preflight_result(manifest, node) {
        return Ok(result);
    }
    if let Some(result) = tool_approval_preflight_result(manifest, node, inputs) {
        return Ok(result);
    }
    let result = handler
        .execute_node(NodeExecutionContext {
            manifest,
            node,
            inputs,
        })
        .await?;
    if matches!(
        normalize_success_status(result.status),
        DagNodeStatus::Ok | DagNodeStatus::Degraded
    ) {
        validate_node_artifact_output_contract(node, &result.outputs)?;
        validate_node_output_contract(manifest, node, &result.outputs)?;
    }
    Ok(result)
}

fn tool_isolation_preflight_result(
    manifest: &DagManifest,
    node: &DagNode,
) -> Option<NodeExecutionResult> {
    let tool = node
        .tool
        .as_deref()
        .and_then(|tool_id| find_tool(manifest, tool_id))?;
    if is_generic_runtime_tool(tool.executor) {
        return None;
    }
    let error = unsupported_host_isolation_policy_error(tool)?;
    let status = if node.required {
        DagNodeStatus::Failed
    } else {
        DagNodeStatus::Degraded
    };
    Some(NodeExecutionResult {
        status,
        outputs: DagIo::default(),
        diagnostics: DagIo::default(),
        warning: (!node.required).then(|| error.clone()),
        error: node.required.then_some(error),
        command: tool.command.clone(),
        exit_status: None,
        model: None,
        prompt_hash: None,
        trace: BTreeMap::new(),
    })
}

fn tool_approval_preflight_result(
    manifest: &DagManifest,
    node: &DagNode,
    inputs: &DagIo,
) -> Option<NodeExecutionResult> {
    let tool = node
        .tool
        .as_deref()
        .and_then(|tool_id| find_tool(manifest, tool_id))?;
    if is_generic_runtime_tool(tool.executor) {
        return None;
    }
    if !approval_required(tool, inputs) {
        return None;
    }
    Some(NodeExecutionResult {
        status: DagNodeStatus::AwaitingApproval,
        outputs: DagIo::default(),
        diagnostics: DagIo::default(),
        warning: Some(format!(
            "tool `{}` is waiting for approval key `{}`",
            tool.id,
            tool_approval_key(&tool.id)
        )),
        error: None,
        command: tool.command.clone(),
        exit_status: None,
        model: None,
        prompt_hash: None,
        trace: BTreeMap::new(),
    })
}

fn validate_node_input_contract(
    manifest: &DagManifest,
    node: &DagNode,
    inputs: &DagIo,
) -> anyhow::Result<()> {
    if let Some((tool, schema)) =
        node_tool_schema(manifest, node, |tool| tool.input_schema.as_ref())
    {
        let contract_inputs = contract_input_io(node, inputs);
        validate_dag_io_schema("input schema", &tool.id, &node.id, schema, &contract_inputs)?;
    }
    Ok(())
}

fn validate_node_output_contract(
    manifest: &DagManifest,
    node: &DagNode,
    outputs: &DagIo,
) -> anyhow::Result<()> {
    if let Some((tool, schema)) =
        node_tool_schema(manifest, node, |tool| tool.output_schema.as_ref())
    {
        validate_dag_io_schema("output schema", &tool.id, &node.id, schema, outputs)?;
    }
    Ok(())
}

fn validate_node_artifact_output_contract(node: &DagNode, outputs: &DagIo) -> anyhow::Result<()> {
    let declared: BTreeSet<&str> = node.outputs.iter().map(String::as_str).collect();
    for key in outputs.artifacts.keys() {
        if !is_safe_artifact_key(key) {
            anyhow::bail!(
                "unsafe artifact output key `{key}` returned by node `{}`",
                node.id
            );
        }
        if !declared.contains(key.as_str()) {
            anyhow::bail!(
                "undeclared artifact output `{key}` returned by node `{}`",
                node.id
            );
        }
    }
    Ok(())
}

fn node_tool_schema<'a>(
    manifest: &'a DagManifest,
    node: &DagNode,
    schema: impl Fn(&'a DagTool) -> Option<&'a serde_yaml::Value>,
) -> Option<(&'a DagTool, &'a serde_yaml::Value)> {
    let tool = node
        .tool
        .as_deref()
        .and_then(|tool_id| find_tool(manifest, tool_id))?;
    schema(tool).map(|schema| (tool, schema))
}

fn contract_input_io(node: &DagNode, inputs: &DagIo) -> DagIo {
    if node.inputs.is_empty() {
        return inputs.clone();
    }

    let mut allowed: BTreeSet<&str> = node.inputs.iter().map(String::as_str).collect();
    match node.kind {
        DagNodeKind::Loop => {
            allowed.insert(LOOP_ROUND_INPUT);
            allowed.insert(LOOP_MAX_ROUNDS_INPUT);
            allowed.insert(LOOP_NODE_ID_INPUT);
        }
        DagNodeKind::Map => {
            allowed.insert(MAP_INDEX_INPUT);
            allowed.insert(MAP_MAX_ITEMS_INPUT);
            allowed.insert(MAP_NODE_ID_INPUT);
            if let Some(policy) = &node.map {
                allowed.insert(policy.item_key.as_str());
                allowed.insert(policy.index_key.as_str());
            }
        }
        _ => {}
    }

    DagIo {
        values: inputs
            .values
            .iter()
            .filter(|(key, _)| allowed.contains(key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        artifacts: inputs
            .artifacts
            .iter()
            .filter(|(key, _)| allowed.contains(key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}

fn validate_dag_io_schema(
    boundary: &str,
    tool_id: &str,
    node_id: &str,
    schema: &serde_yaml::Value,
    io: &DagIo,
) -> anyhow::Result<()> {
    let schema_json = serde_json::to_value(schema)
        .map_err(|err| anyhow::anyhow!("{boundary} for tool `{tool_id}` is not JSON: {err}"))?;
    let io_json = serde_json::to_value(io)?;
    validate_contract_schema(&schema_json, &io_json, "$").map_err(|err| {
        anyhow::anyhow!("{boundary} for tool `{tool_id}` rejected node `{node_id}` DagIo: {err}")
    })
}

fn validate_contract_schema(
    schema: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    let Some(schema_object) = schema.as_object() else {
        return Err(format!("{path} schema must be an object"));
    };

    if let Some(type_schema) = schema_object.get("type") {
        validate_schema_type(type_schema, value, path)?;
    }

    if let Some(enum_schema) = schema_object.get("enum") {
        let enum_values = enum_schema
            .as_array()
            .ok_or_else(|| format!("{path}.enum must be an array"))?;
        if enum_values.is_empty() {
            return Err(format!("{path}.enum must not be empty"));
        }
        if !enum_values.iter().any(|allowed| allowed == value) {
            return Err(format!(
                "{path} value is not one of the declared enum values"
            ));
        }
    }

    if let Some(minimum_schema) = schema_object.get("minimum") {
        let minimum = minimum_schema
            .as_f64()
            .ok_or_else(|| format!("{path}.minimum must be a number"))?;
        if value.as_f64().is_some_and(|actual| actual < minimum) {
            return Err(format!("{path} must be >= minimum {minimum}"));
        }
    }

    if let Some(maximum_schema) = schema_object.get("maximum") {
        let maximum = maximum_schema
            .as_f64()
            .ok_or_else(|| format!("{path}.maximum must be a number"))?;
        if value.as_f64().is_some_and(|actual| actual > maximum) {
            return Err(format!("{path} must be <= maximum {maximum}"));
        }
    }

    if let Some(min_items_schema) = schema_object.get("minItems") {
        let min_items = min_items_schema
            .as_u64()
            .ok_or_else(|| format!("{path}.minItems must be an integer"))?;
        if value
            .as_array()
            .is_some_and(|array| (array.len() as u64) < min_items)
        {
            return Err(format!("{path} must contain at least minItems {min_items}"));
        }
    }

    if let Some(max_items_schema) = schema_object.get("maxItems") {
        let max_items = max_items_schema
            .as_u64()
            .ok_or_else(|| format!("{path}.maxItems must be an integer"))?;
        if value
            .as_array()
            .is_some_and(|array| (array.len() as u64) > max_items)
        {
            return Err(format!("{path} must contain at most maxItems {max_items}"));
        }
    }

    if let Some(min_length_schema) = schema_object.get("minLength") {
        let min_length = min_length_schema
            .as_u64()
            .ok_or_else(|| format!("{path}.minLength must be an integer"))?;
        if value
            .as_str()
            .map(|value| value.chars().count() as u64)
            .is_some_and(|actual| actual < min_length)
        {
            return Err(format!(
                "{path} must contain at least minLength {min_length}"
            ));
        }
    }

    if let Some(max_length_schema) = schema_object.get("maxLength") {
        let max_length = max_length_schema
            .as_u64()
            .ok_or_else(|| format!("{path}.maxLength must be an integer"))?;
        if value
            .as_str()
            .map(|value| value.chars().count() as u64)
            .is_some_and(|actual| actual > max_length)
        {
            return Err(format!(
                "{path} must contain at most maxLength {max_length}"
            ));
        }
    }

    if let Some(required_schema) = schema_object.get("required") {
        let required = required_schema
            .as_array()
            .ok_or_else(|| format!("{path}.required must be an array"))?;
        if let Some(object) = value.as_object() {
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{path}.required contains a non-string field"))?;
                if !object.contains_key(field) {
                    return Err(format!("{path} missing required field `{field}`"));
                }
            }
        }
    }

    if let Some(properties_schema) = schema_object.get("properties") {
        let properties = properties_schema
            .as_object()
            .ok_or_else(|| format!("{path}.properties must be an object"))?;
        if let Some(object) = value.as_object() {
            for (field, field_schema) in properties {
                if let Some(field_value) = object.get(field) {
                    validate_contract_schema(
                        field_schema,
                        field_value,
                        &format!("{path}.{field}"),
                    )?;
                }
            }
        }
    }

    if let Some(items_schema) = schema_object.get("items") {
        if !items_schema.is_object() {
            return Err(format!("{path}.items must be a schema object"));
        }
        if let Some(array) = value.as_array() {
            for (index, item) in array.iter().enumerate() {
                validate_contract_schema(items_schema, item, &format!("{path}[{index}]"))?;
            }
        }
    }

    if let Some(additional_properties) = schema_object.get("additionalProperties") {
        let additional_properties = additional_properties
            .as_bool()
            .ok_or_else(|| format!("{path}.additionalProperties must be a boolean"))?;
        if additional_properties {
            return Ok(());
        }
        let Some(object) = value.as_object() else {
            return Ok(());
        };
        let allowed = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|properties| properties.keys().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        for field in object.keys() {
            if !allowed.contains(field) {
                return Err(format!("{path} contains undeclared field `{field}`"));
            }
        }
    }

    Ok(())
}

fn validate_schema_type(
    expected_type: &serde_json::Value,
    value: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    let types = match expected_type {
        serde_json::Value::String(expected_type) => vec![expected_type.as_str()],
        serde_json::Value::Array(types) => types
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| format!("{path} type array contains a non-string entry"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(format!(
                "{path} schema type must be a string or string array"
            ))
        }
    };
    if types.is_empty() {
        return Err(format!("{path} schema type array must not be empty"));
    }
    for expected in &types {
        validate_schema_type_name(expected, path)?;
    }

    if types
        .iter()
        .any(|expected| schema_type_matches(expected, value))
    {
        return Ok(());
    }

    Err(format!("{} must be {}", path, types.join(" or ")))
}

fn validate_schema_type_name(expected_type: &str, path: &str) -> Result<(), String> {
    match expected_type {
        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null" => Ok(()),
        other => Err(format!("{path} declares unsupported schema type `{other}`")),
    }
}

fn schema_type_matches(expected_type: &str, value: &serde_json::Value) -> bool {
    match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn manifest_hash(manifest: &DagManifest) -> anyhow::Result<String> {
    let bytes = serde_json::to_vec(manifest)?;
    Ok(fnv1a64(&bytes))
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes.iter().copied() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn llm_model(inputs: &DagIo) -> Option<String> {
    ["model", "llm_model", "agenthero_model"]
        .into_iter()
        .find_map(|key| inputs.values.get(key).and_then(serde_json::Value::as_str))
        .map(ToString::to_string)
}

fn llm_prompt_hash(inputs: &DagIo) -> Option<String> {
    if let Some(hash) = inputs
        .values
        .get("prompt_hash")
        .and_then(serde_json::Value::as_str)
    {
        return Some(hash.to_string());
    }
    ["prompt", "llm_prompt", "system_prompt"]
        .into_iter()
        .find_map(|key| inputs.values.get(key))
        .map(|prompt| fnv1a64(prompt.to_string().as_bytes()))
}

fn node_command(manifest: &DagManifest, node: &DagNode) -> Option<Vec<String>> {
    node.tool.as_deref().and_then(|tool_id| {
        manifest
            .tools
            .iter()
            .find(|tool| tool.id == tool_id)
            .and_then(|tool| tool.command.clone())
    })
}

fn find_tool<'a>(manifest: &'a DagManifest, tool_id: &str) -> Option<&'a DagTool> {
    manifest.tools.iter().find(|tool| tool.id == tool_id)
}

fn is_generic_runtime_tool(executor: ToolExecutorKind) -> bool {
    matches!(
        executor,
        ToolExecutorKind::ApprovalGate | ToolExecutorKind::Http
    ) || is_generic_command_tool(executor)
}

fn is_generic_command_tool(executor: ToolExecutorKind) -> bool {
    matches!(
        executor,
        ToolExecutorKind::Cli
            | ToolExecutorKind::Shell
            | ToolExecutorKind::Python
            | ToolExecutorKind::RustBinary
            | ToolExecutorKind::Llm
            | ToolExecutorKind::Lean
            | ToolExecutorKind::Haskell
            | ToolExecutorKind::Docker
            | ToolExecutorKind::Wasm
    )
}

fn executor_command_boundary_error(tool: &DagTool, command: &[String]) -> Option<String> {
    let program = command.first()?;
    let program_name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program.as_str());
    let normalized = program_name.strip_suffix(".exe").unwrap_or(program_name);
    let normalized = normalized.to_ascii_lowercase();
    let expected = executor_command_expectation(tool.executor)?;
    if expected.allows(&normalized) {
        return executor_isolation_flag_error(tool, command);
    }
    Some(format!(
        "tool `{}` executor `{}` requires command {}, got `{program_name}`",
        tool.id, tool.executor, expected.description
    ))
}

struct ExecutorCommandExpectation {
    description: &'static str,
    allowed: &'static [&'static str],
    allowed_prefixes: &'static [&'static str],
    allowed_suffixes: &'static [&'static str],
    denied: &'static [&'static str],
    denied_prefixes: &'static [&'static str],
}

impl ExecutorCommandExpectation {
    fn allows(&self, program: &str) -> bool {
        if self.denied.contains(&program)
            || self
                .denied_prefixes
                .iter()
                .any(|prefix| program.starts_with(prefix))
        {
            return false;
        }

        let has_allow_list = !self.allowed.is_empty()
            || !self.allowed_prefixes.is_empty()
            || !self.allowed_suffixes.is_empty();
        !has_allow_list
            || self.allowed.contains(&program)
            || self
                .allowed_prefixes
                .iter()
                .any(|prefix| program.starts_with(prefix))
            || self
                .allowed_suffixes
                .iter()
                .any(|suffix| program.ends_with(suffix))
    }
}

fn executor_command_expectation(executor: ToolExecutorKind) -> Option<ExecutorCommandExpectation> {
    match executor {
        ToolExecutorKind::Shell => Some(ExecutorCommandExpectation {
            description: "to start with a shell executable",
            allowed: &["sh", "bash", "zsh", "dash", "fish", "pwsh", "powershell"],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::Python => Some(ExecutorCommandExpectation {
            description: "to start with a Python interpreter",
            allowed: &["python", "python3", "pypy", "pypy3"],
            allowed_prefixes: &["python3.", "pypy3."],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::RustBinary => Some(ExecutorCommandExpectation {
            description: "to start with a direct compiled binary, not a shell or interpreter",
            allowed: &[],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[
                "sh",
                "bash",
                "zsh",
                "dash",
                "fish",
                "pwsh",
                "powershell",
                "python",
                "python3",
                "pypy",
                "pypy3",
                "node",
                "npm",
                "npx",
                "bun",
                "deno",
                "ruby",
                "perl",
                "cargo",
            ],
            denied_prefixes: &["python3.", "pypy3."],
        }),
        ToolExecutorKind::Llm => Some(ExecutorCommandExpectation {
            description: "to start with an LLM adapter or model CLI",
            allowed: &[
                "llm-adapter",
                "agenthero-llm",
                "openai",
                "codex",
                "claude",
                "agy",
                "ollama",
            ],
            allowed_prefixes: &[
                "llm-",
                "agenthero-llm-",
                "openai-",
                "codex-",
                "claude-",
                "agy-",
                "ollama-",
            ],
            allowed_suffixes: &["-llm-adapter", "-llm-runner"],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::Lean => Some(ExecutorCommandExpectation {
            description: "to start with `lean` or `lake`",
            allowed: &["lean", "lake"],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::Haskell => Some(ExecutorCommandExpectation {
            description: "to start with a Haskell toolchain executable",
            allowed: &["cabal", "stack", "ghc", "ghci", "runhaskell", "runghc"],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::Docker => Some(ExecutorCommandExpectation {
            description: "to start with `docker`",
            allowed: &["docker"],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        ToolExecutorKind::Wasm => Some(ExecutorCommandExpectation {
            description: "to start with a WebAssembly runtime",
            allowed: &["wasmtime", "wasmer", "wasm3"],
            allowed_prefixes: &[],
            allowed_suffixes: &[],
            denied: &[],
            denied_prefixes: &[],
        }),
        _ => None,
    }
}

fn executor_isolation_flag_error(tool: &DagTool, command: &[String]) -> Option<String> {
    match tool.executor {
        ToolExecutorKind::Docker => docker_isolation_flag_error(command),
        ToolExecutorKind::Wasm => wasm_isolation_flag_error(command),
        _ => None,
    }
    .map(|flag| {
        format!(
            "tool `{}` executor `{}` rejected unsafe isolation flag `{flag}`",
            tool.id, tool.executor
        )
    })
}

fn docker_isolation_flag_error(command: &[String]) -> Option<String> {
    let args = command.get(1..).unwrap_or_default();
    for (index, arg) in args.iter().enumerate() {
        let next = args.get(index + 1).map(String::as_str);
        let flag = arg.as_str();
        if matches!(
            flag,
            "--privileged"
                | "--cap-add"
                | "--device"
                | "--pid"
                | "--ipc"
                | "--uts"
                | "--network"
                | "--net"
                | "--volume"
                | "--mount"
                | "-v"
                | "--security-opt"
        ) {
            if flag == "--network" || flag == "--net" {
                if next == Some("host") {
                    return Some(format!("{flag}=host"));
                }
                continue;
            }
            return Some(flag.to_string());
        }
        for unsafe_prefix in [
            "--cap-add=",
            "--device=",
            "--pid=host",
            "--ipc=host",
            "--uts=host",
            "--network=host",
            "--net=host",
            "--volume=",
            "--mount=",
            "--security-opt=",
        ] {
            if flag.starts_with(unsafe_prefix) {
                return Some(flag.to_string());
            }
        }
    }
    None
}

fn wasm_isolation_flag_error(command: &[String]) -> Option<String> {
    let args = command.get(1..).unwrap_or_default();
    for arg in args {
        let flag = arg.as_str();
        if matches!(
            flag,
            "--dir" | "--mapdir" | "--tcplisten" | "--addr-pool" | "--inherit-env" | "-S"
        ) {
            return Some(flag.to_string());
        }
        for unsafe_prefix in ["--dir=", "--mapdir=", "--tcplisten=", "--addr-pool=", "-S"] {
            if flag.starts_with(unsafe_prefix) {
                return Some(flag.to_string());
            }
        }
    }
    None
}

fn unsupported_host_isolation_policy_error(tool: &DagTool) -> Option<String> {
    let policy = tool.policy.as_ref()?;
    let network_denied = !policy.network.allow;
    let filesystem_restricted =
        !policy.filesystem.read.is_empty() || !policy.filesystem.write.is_empty();
    if !network_denied && !filesystem_restricted {
        return None;
    }

    match tool.executor {
        ToolExecutorKind::ApprovalGate => None,
        ToolExecutorKind::Http => (!policy.filesystem.read.is_empty()).then(|| {
            format!(
                "tool `{}` policy requires isolated runner; http executor cannot enforce filesystem read restrictions",
                tool.id
            )
        }),
        executor => Some(format!(
            "tool `{}` policy requires isolated runner; host executor `{executor:?}` cannot enforce network/filesystem restrictions",
            tool.id
        )),
    }
}

fn approval_required(tool: &DagTool, inputs: &DagIo) -> bool {
    let Some(policy) = tool.policy.as_ref() else {
        return false;
    };
    policy.approval_required
        && !inputs
            .values
            .get(&tool_approval_key(&tool.id))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

fn tool_approval_key(tool_id: &str) -> String {
    format!("approval/{tool_id}")
}

fn missing_declared_outputs_message(tool_id: &str, outputs: &[String]) -> String {
    if outputs.len() == 1 {
        return format!(
            "tool `{}` missing declared output artifact `{}`",
            tool_id, outputs[0]
        );
    }
    let names = outputs
        .iter()
        .map(|output| format!("`{output}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("tool `{tool_id}` missing declared output artifacts {names}")
}

fn filesystem_write_policy_error(tool: &DagTool, node: &DagNode) -> Option<String> {
    let policy = tool.policy.as_ref()?;
    if policy.filesystem.write.is_empty() {
        return None;
    }
    node.outputs
        .iter()
        .find(|output| !filesystem_write_allows(&policy.filesystem.write, output))
        .map(|output| {
            format!(
                "tool `{}` filesystem write policy denies declared output `{}`",
                tool.id, output
            )
        })
}

fn filesystem_write_allows(allowed_roots: &[String], output: &str) -> bool {
    allowed_roots.iter().any(|root| {
        root == "."
            || (is_safe_artifact_key(root)
                && (output == root
                    || output
                        .strip_prefix(root)
                        .is_some_and(|rest| rest.starts_with('/'))))
    })
}

fn budget_policy_error(tool: &DagTool, inputs: &DagIo) -> Option<String> {
    let requested = tool.policy.as_ref()?.budget_units?;
    let remaining = remaining_budget_units(inputs)?;
    (requested > remaining).then(|| {
        format!(
            "tool `{}` budget policy requires {requested} units but only {remaining} remain",
            tool.id
        )
    })
}

fn node_budget_units(manifest: &DagManifest, node: &DagNode) -> Option<u64> {
    node.tool
        .as_deref()
        .and_then(|tool_id| find_tool(manifest, tool_id))
        .and_then(|tool| tool.policy.as_ref())
        .and_then(|policy| policy.budget_units)
}

fn consume_node_budget(outputs: &mut DagIo, manifest: &DagManifest, node: &DagNode) {
    let Some(consumed) = node_budget_units(manifest, node) else {
        return;
    };
    let Some(remaining) = remaining_budget_units(outputs) else {
        return;
    };
    outputs.values.insert(
        AGENTHERO_BUDGET_UNITS_REMAINING.to_string(),
        serde_json::json!(remaining.saturating_sub(consumed)),
    );
}

fn remaining_budget_units(inputs: &DagIo) -> Option<u64> {
    [
        AGENTHERO_BUDGET_UNITS_REMAINING,
        "budget_units_remaining",
        "budget_remaining",
    ]
    .iter()
    .find_map(|key| inputs.values.get(*key).and_then(json_to_budget_units))
}

fn json_to_budget_units(value: &serde_json::Value) -> Option<u64> {
    if let Some(units) = value.as_u64() {
        return Some(units);
    }
    if let Some(units) = value.as_i64() {
        return Some(units.max(0) as u64);
    }
    value.as_str()?.parse().ok()
}

fn apply_tool_env(
    command: &mut Command,
    manifest: &DagManifest,
    node: &DagNode,
    tool: &DagTool,
    inputs: &DagIo,
) {
    command.env("AGENTHERO_DAG_TYPE", manifest.id.as_str());
    command.env("AGENTHERO_NODE_ID", node.id.as_str());
    command.env("AGENTHERO_TOOL_ID", tool.id.as_str());
    command.env("AGENTHERO_EXECUTOR_KIND", tool.executor.to_string());
    if let Some(policy) = tool.policy.as_ref() {
        command.env(
            "AGENTHERO_NETWORK",
            if policy.network.allow {
                "allow"
            } else {
                "deny"
            },
        );
        if let Some(budget_units) = policy.budget_units {
            command.env("AGENTHERO_BUDGET_UNITS", budget_units.to_string());
        }
        if !policy.filesystem.read.is_empty() {
            command.env(
                "AGENTHERO_FS_READ",
                serde_json::to_string(&policy.filesystem.read).unwrap_or_else(|_| "[]".to_string()),
            );
        }
        if !policy.filesystem.write.is_empty() {
            command.env(
                "AGENTHERO_FS_WRITE",
                serde_json::to_string(&policy.filesystem.write)
                    .unwrap_or_else(|_| "[]".to_string()),
            );
        }
    }
    if tool.executor == ToolExecutorKind::Llm {
        if let Some(model) = llm_model(inputs) {
            command.env("AGENTHERO_LLM_MODEL", model);
        }
        if let Some(prompt_hash) = llm_prompt_hash(inputs) {
            command.env("AGENTHERO_LLM_PROMPT_HASH", prompt_hash);
        }
    }
}

async fn write_status_artifact(
    workdir: &Path,
    command: &[String],
    exit_status: Option<i32>,
    status: DagNodeStatus,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let status_json = serde_json::json!({
        "command": command,
        "exit_status": exit_status,
        "status": status,
        "error": error,
    });
    tokio::fs::write(
        workdir.join("status.json"),
        serde_json::to_vec_pretty(&status_json)?,
    )
    .await?;
    Ok(())
}

fn artifact_ref(path: &Path) -> ArtifactRef {
    ArtifactRef {
        uri: path.to_string_lossy().to_string(),
        media_type: media_type_for_path(path),
        metadata: artifact_integrity_metadata(path),
    }
}

fn artifact_integrity_metadata(path: &Path) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    if let Ok(integrity) = local_artifact_integrity(path) {
        metadata.insert("sha256".to_string(), serde_json::json!(integrity.sha256));
        metadata.insert(
            "size_bytes".to_string(),
            serde_json::json!(integrity.size_bytes),
        );
    }
    metadata
}

fn validate_checkpoint_artifact_integrity(
    key: &str,
    node_id: &str,
    artifact: &ArtifactRef,
) -> anyhow::Result<()> {
    let expected_sha256 = artifact
        .metadata
        .get("sha256")
        .and_then(serde_json::Value::as_str);
    let expected_size = artifact
        .metadata
        .get("size_bytes")
        .and_then(serde_json::Value::as_u64);
    if expected_sha256.is_none() && expected_size.is_none() {
        return Ok(());
    }
    let Some(path) = local_artifact_path(&artifact.uri) else {
        return Ok(());
    };
    let integrity = local_artifact_integrity(&path).map_err(|err| {
        anyhow::anyhow!(
            "checkpoint artifact `{key}` from node `{node_id}` content drift: cannot read `{}`: {err}",
            artifact.uri
        )
    })?;
    if let Some(expected_size) = expected_size {
        if integrity.size_bytes != expected_size {
            anyhow::bail!(
                "checkpoint artifact `{key}` from node `{node_id}` content drift: expected size {expected_size}, found {}",
                integrity.size_bytes
            );
        }
    }
    if let Some(expected_sha256) = expected_sha256 {
        if integrity.sha256 != expected_sha256 {
            anyhow::bail!(
                "checkpoint artifact `{key}` from node `{node_id}` content drift: expected sha256 {expected_sha256}, found {}",
                integrity.sha256
            );
        }
    }
    Ok(())
}

fn local_artifact_path(uri: &str) -> Option<PathBuf> {
    if let Some(path) = uri.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }
    if uri.contains("://") {
        return None;
    }
    Some(PathBuf::from(uri))
}

fn local_artifact_integrity(path: &Path) -> anyhow::Result<LocalArtifactIntegrity> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        hasher.update(&buffer[..read]);
    }
    Ok(LocalArtifactIntegrity {
        sha256: format!("{:x}", hasher.finalize()),
        size_bytes,
    })
}

struct LocalArtifactIntegrity {
    sha256: String,
    size_bytes: u64,
}

fn resolve_output_path(workdir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if !is_safe_artifact_key(name) {
        anyhow::bail!("unsafe artifact output key `{name}`");
    }
    Ok(workdir.join(name))
}

fn media_type_for_path(path: &Path) -> Option<String> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => Some("application/json".to_string()),
        Some("jsonl") => Some("application/jsonl".to_string()),
        Some("log") | Some("txt") | Some("md") => Some("text/plain".to_string()),
        _ => None,
    }
}

fn input_refs(node: &DagNode, inputs: &DagIo) -> BTreeMap<String, String> {
    node.inputs
        .iter()
        .filter_map(|name| {
            inputs
                .artifacts
                .get(name)
                .map(|artifact| (name.clone(), artifact.uri.clone()))
        })
        .collect()
}

fn produced_output_refs(
    status: DagNodeStatus,
    outputs: Option<&DagIo>,
) -> BTreeMap<String, String> {
    if !matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
        return BTreeMap::new();
    }
    outputs
        .map(|outputs| {
            outputs
                .artifacts
                .iter()
                .map(|(name, artifact)| (name.clone(), artifact.uri.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn diagnostic_refs(diagnostics: &DagIo) -> BTreeMap<String, String> {
    diagnostics
        .artifacts
        .iter()
        .map(|(name, artifact)| (name.clone(), artifact.uri.clone()))
        .collect()
}

fn can_execute_layer_concurrently(
    manifest: &DagManifest,
    nodes: &[&DagNode],
    deps: &HashMap<String, Vec<String>>,
    statuses: &HashMap<String, DagNodeStatus>,
    branch_selections: &HashMap<String, BTreeSet<String>>,
) -> bool {
    nodes.len() > 1
        && nodes
            .iter()
            .all(|node| node_budget_units(manifest, node).is_none())
        && nodes.iter().all(|node| {
            is_ordinary_handler_node(node)
                && !is_unselected_by_branch(node, deps, branch_selections)
                && !has_blocking_dependency(node, deps, statuses)
        })
}

fn events_from_reports(
    manifest: &DagManifest,
    manifest_hash: &str,
    reports: &[DagNodeReport],
    inputs: &DagIo,
) -> Vec<DagExecutionEvent> {
    reports
        .iter()
        .map(|report| node_report_event(manifest, manifest_hash, report, inputs))
        .collect()
}

fn node_report_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    report: &DagNodeReport,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let mut event = with_manifest_identity(
        node_status_event(
            report.node_id.clone(),
            report.kind.clone(),
            report.attempt,
            report.status,
            report
                .error
                .clone()
                .or_else(|| report.warning.clone())
                .or_else(|| Some(format!("{} {:?}", report.node_id, report.status))),
        ),
        manifest,
        manifest_hash,
    );
    add_report_audit_payload(&mut event.payload, report);
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn node_report_started_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    report: &DagNodeReport,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let mut event = with_manifest_identity(
        DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.started".to_string(),
            node_id: Some(report.node_id.clone()),
            message: Some(format!("{} started", report.node_id)),
            payload: BTreeMap::from([
                ("node_id".to_string(), serde_json::json!(report.node_id)),
                ("node_kind".to_string(), serde_json::json!(report.kind)),
                ("kind".to_string(), serde_json::json!(report.kind)),
                ("attempt".to_string(), serde_json::json!(report.attempt)),
            ]),
        },
        manifest,
        manifest_hash,
    );
    add_report_static_audit_payload(&mut event.payload, report);
    add_report_trace_payload(&mut event.payload, report);
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn push_and_emit_node_report_events(
    events: &mut Vec<DagExecutionEvent>,
    sink: &Option<DagEventSink>,
    manifest: &DagManifest,
    manifest_hash: &str,
    report: &DagNodeReport,
    inputs: &DagIo,
) {
    push_and_emit_event(
        events,
        sink,
        node_report_started_event(manifest, manifest_hash, report, inputs),
    );
    push_and_emit_event(
        events,
        sink,
        node_report_event(manifest, manifest_hash, report, inputs),
    );
}

fn execution_events(
    manifest: &DagManifest,
    manifest_hash: &str,
    live_events: &[DagExecutionEvent],
    reports: &[DagNodeReport],
    inputs: &DagIo,
) -> Vec<DagExecutionEvent> {
    let mut events = live_events.to_vec();
    for event in &mut events {
        normalize_agenthero_trace_event(event);
    }
    let mut seen = events.iter().map(event_identity).collect::<BTreeSet<_>>();
    for mut event in events_from_reports(manifest, manifest_hash, reports, inputs) {
        normalize_agenthero_trace_event(&mut event);
        let identity = event_identity(&event);
        if seen.insert(identity.clone()) {
            events.push(event);
        } else if let Some(existing) = events
            .iter_mut()
            .find(|existing| event_identity(existing) == identity)
        {
            merge_event_payload(existing, &event);
        }
    }
    events
}

fn dag_started_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let mut event = DagExecutionEvent {
        level: "info".to_string(),
        event_type: "dag.started".to_string(),
        node_id: None,
        message: Some(format!("{} started", manifest.id)),
        payload: BTreeMap::from([
            ("dag_type".to_string(), serde_json::json!(manifest.id)),
            (
                "manifest_version".to_string(),
                serde_json::json!(manifest.version),
            ),
            (
                "manifest_hash".to_string(),
                serde_json::json!(manifest_hash),
            ),
        ]),
    };
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn dag_terminal_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    status: DagNodeStatus,
    node_count: usize,
) -> DagExecutionEvent {
    let event_type = match status {
        DagNodeStatus::AwaitingApproval => "dag.awaiting_approval",
        DagNodeStatus::Failed => "dag.failed",
        DagNodeStatus::Skipped => "dag.skipped",
        _ => "dag.completed",
    };
    let level = match status {
        DagNodeStatus::Failed => "error",
        DagNodeStatus::Degraded | DagNodeStatus::AwaitingApproval => "warn",
        _ => "info",
    };
    DagExecutionEvent {
        level: level.to_string(),
        event_type: event_type.to_string(),
        node_id: None,
        message: Some(format!("{} {:?}", manifest.id, status)),
        payload: BTreeMap::from([
            ("dag_type".to_string(), serde_json::json!(manifest.id)),
            (
                "manifest_version".to_string(),
                serde_json::json!(manifest.version),
            ),
            (
                "manifest_hash".to_string(),
                serde_json::json!(manifest_hash),
            ),
            ("status".to_string(), serde_json::json!(status)),
            ("node_count".to_string(), serde_json::json!(node_count)),
        ]),
    }
}

fn dag_cancelled_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    node_count: usize,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let mut event = DagExecutionEvent {
        level: "warn".to_string(),
        event_type: "dag.cancelled".to_string(),
        node_id: None,
        message: Some(format!("{} cancelled", manifest.id)),
        payload: BTreeMap::from([
            ("dag_type".to_string(), serde_json::json!(manifest.id)),
            (
                "manifest_version".to_string(),
                serde_json::json!(manifest.version),
            ),
            (
                "manifest_hash".to_string(),
                serde_json::json!(manifest_hash),
            ),
            ("status".to_string(), serde_json::json!("cancelled")),
            ("node_count".to_string(), serde_json::json!(node_count)),
        ]),
    };
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn node_terminal_event_for_result(
    manifest: &DagManifest,
    manifest_hash: &str,
    node: &DagNode,
    attempt: u32,
    latency_ms: u64,
    result: &anyhow::Result<NodeExecutionResult>,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let mut event = match result {
        Ok(result) => {
            let status = normalize_success_status(result.status);
            let mut event = with_manifest_identity(
                node_status_event(
                    node.id.clone(),
                    node.kind.to_string(),
                    attempt,
                    status,
                    result
                        .error
                        .clone()
                        .or_else(|| result.warning.clone())
                        .or_else(|| Some(format!("{} {:?}", node.id, status))),
                ),
                manifest,
                manifest_hash,
            );
            add_result_audit_payload(&mut event.payload, result);
            event
        }
        Err(err) if node.required => {
            let message = format!("{err:#}");
            let mut event = with_manifest_identity(
                node_status_event(
                    node.id.clone(),
                    node.kind.to_string(),
                    attempt,
                    DagNodeStatus::Failed,
                    Some(message.clone()),
                ),
                manifest,
                manifest_hash,
            );
            event
                .payload
                .insert("error".to_string(), serde_json::json!(message));
            event
        }
        Err(err) => {
            let message = format!("{err:#}");
            let mut event = with_manifest_identity(
                node_status_event(
                    node.id.clone(),
                    node.kind.to_string(),
                    attempt,
                    DagNodeStatus::Degraded,
                    Some(message.clone()),
                ),
                manifest,
                manifest_hash,
            );
            event
                .payload
                .insert("warning".to_string(), serde_json::json!(message));
            event
        }
    };
    add_node_static_audit_payload(&mut event.payload, manifest, node);
    event
        .payload
        .insert("latency_ms".to_string(), serde_json::json!(latency_ms));
    event
        .payload
        .insert("duration_ms".to_string(), serde_json::json!(latency_ms));
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn add_result_audit_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    result: &NodeExecutionResult,
) {
    if let Some(command) = result.command.as_ref() {
        payload.insert("command".to_string(), serde_json::json!(command));
    }
    if let Some(exit_status) = result.exit_status {
        payload.insert("exit_status".to_string(), serde_json::json!(exit_status));
    }
    if let Some(model) = result.model.as_ref() {
        payload.insert("model".to_string(), serde_json::json!(model));
    }
    if let Some(prompt_hash) = result.prompt_hash.as_ref() {
        payload.insert("prompt_hash".to_string(), serde_json::json!(prompt_hash));
    }
    if let Some(error) = result.error.as_ref() {
        payload.insert("error".to_string(), serde_json::json!(error));
    }
    if let Some(warning) = result.warning.as_ref() {
        payload.insert("warning".to_string(), serde_json::json!(warning));
    }
    let output_refs = artifact_refs_from_io(&result.outputs);
    if !output_refs.is_empty() {
        payload.insert("output_refs".to_string(), serde_json::json!(output_refs));
    }
    let diagnostic_refs = artifact_refs_from_io(&result.diagnostics);
    if !diagnostic_refs.is_empty() {
        payload.insert(
            "diagnostic_refs".to_string(),
            serde_json::json!(diagnostic_refs),
        );
    }
}

fn add_node_static_audit_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    manifest: &DagManifest,
    node: &DagNode,
) {
    payload.insert("required".to_string(), serde_json::json!(node.required));
    if let Some(role) = node.role.as_ref() {
        payload.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(tool) = node.tool.as_ref() {
        payload.insert("tool".to_string(), serde_json::json!(tool));
        payload.insert("tool_id".to_string(), serde_json::json!(tool));
    }
    if let Some(executor) = node_executor_label(manifest, node) {
        payload.insert("executor".to_string(), serde_json::json!(executor));
    }
    if let Some(child_dag_type) = node.dag_type.as_ref() {
        payload.insert(
            "child_dag_type".to_string(),
            serde_json::json!(child_dag_type),
        );
    }
    if let Some(approval) = node.approval.as_ref() {
        payload.insert("approval".to_string(), serde_json::json!(approval));
    }
}

fn add_report_audit_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    report: &DagNodeReport,
) {
    add_report_static_audit_payload(payload, report);
    add_report_trace_payload(payload, report);
    if let Some(command) = report.command.as_ref() {
        payload.insert("command".to_string(), serde_json::json!(command));
    }
    if let Some(exit_status) = report.exit_status {
        payload.insert("exit_status".to_string(), serde_json::json!(exit_status));
    }
    if let Some(model) = report.model.as_ref() {
        payload.insert("model".to_string(), serde_json::json!(model));
    }
    if let Some(prompt_hash) = report.prompt_hash.as_ref() {
        payload.insert("prompt_hash".to_string(), serde_json::json!(prompt_hash));
    }
    if let Some(error) = report.error.as_ref() {
        payload.insert("error".to_string(), serde_json::json!(error));
    }
    if let Some(warning) = report.warning.as_ref() {
        payload.insert("warning".to_string(), serde_json::json!(warning));
    }
    if !report.input_refs.is_empty() {
        payload.insert(
            "input_refs".to_string(),
            serde_json::json!(report.input_refs),
        );
    }
    if !report.output_refs.is_empty() {
        payload.insert(
            "output_refs".to_string(),
            serde_json::json!(report.output_refs),
        );
    }
    if !report.diagnostic_refs.is_empty() {
        payload.insert(
            "diagnostic_refs".to_string(),
            serde_json::json!(report.diagnostic_refs),
        );
    }
}

fn add_report_trace_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    report: &DagNodeReport,
) {
    if !report.trace.is_empty() {
        payload.insert("trace".to_string(), serde_json::json!(report.trace));
    }
}

fn add_report_static_audit_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    report: &DagNodeReport,
) {
    payload.insert("required".to_string(), serde_json::json!(report.required));
    if let Some(role) = report.role.as_ref() {
        payload.insert("role".to_string(), serde_json::json!(role));
    }
    if let Some(tool) = report.tool.as_ref() {
        payload.insert("tool".to_string(), serde_json::json!(tool));
        payload.insert("tool_id".to_string(), serde_json::json!(tool));
    }
    if let Some(executor) = report.executor.as_ref() {
        payload.insert("executor".to_string(), serde_json::json!(executor));
    }
    if let Some(child_dag_type) = report.child_dag_type.as_ref() {
        payload.insert(
            "child_dag_type".to_string(),
            serde_json::json!(child_dag_type),
        );
    }
}

fn merge_event_payload(existing: &mut DagExecutionEvent, event: &DagExecutionEvent) {
    existing.payload.extend(event.payload.clone());
    if existing.message.is_none() {
        existing.message = event.message.clone();
    }
}

fn with_manifest_identity(
    mut event: DagExecutionEvent,
    manifest: &DagManifest,
    manifest_hash: &str,
) -> DagExecutionEvent {
    add_manifest_identity_payload(&mut event.payload, manifest, manifest_hash);
    event
}

fn with_runtime_identity(mut event: DagExecutionEvent, inputs: &DagIo) -> DagExecutionEvent {
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn add_runtime_identity_payload(payload: &mut BTreeMap<String, serde_json::Value>, inputs: &DagIo) {
    for key in ["app_run_id", "dag_run_id", "artifact_id", "lease_id"] {
        payload.insert(
            key.to_string(),
            serde_json::json!(input_string_value(inputs, key).unwrap_or_default()),
        );
    }
}

fn add_manifest_identity_payload(
    payload: &mut BTreeMap<String, serde_json::Value>,
    manifest: &DagManifest,
    manifest_hash: &str,
) {
    payload.insert("dag_type".to_string(), serde_json::json!(manifest.id));
    payload.insert(
        "manifest_version".to_string(),
        serde_json::json!(manifest.version),
    );
    payload.insert(
        "manifest_hash".to_string(),
        serde_json::json!(manifest_hash),
    );
}

fn artifact_refs_from_io(io: &DagIo) -> BTreeMap<String, String> {
    io.artifacts
        .iter()
        .map(|(name, artifact)| (name.clone(), artifact.uri.clone()))
        .collect()
}

fn node_status_event(
    node_id: String,
    kind: String,
    attempt: u32,
    status: DagNodeStatus,
    message: Option<String>,
) -> DagExecutionEvent {
    let event_type = match status {
        DagNodeStatus::Pending => "node.queued",
        DagNodeStatus::Running => "node.started",
        DagNodeStatus::AwaitingApproval => "node.awaiting_approval",
        DagNodeStatus::Ok | DagNodeStatus::Degraded => "node.completed",
        DagNodeStatus::Failed => "node.failed",
        DagNodeStatus::Skipped => "node.skipped",
    };
    let level = match status {
        DagNodeStatus::Failed => "error",
        DagNodeStatus::Degraded | DagNodeStatus::AwaitingApproval => "warn",
        _ => "info",
    };
    DagExecutionEvent {
        level: level.to_string(),
        event_type: event_type.to_string(),
        node_id: Some(node_id.clone()),
        message,
        payload: BTreeMap::from([
            ("node_id".to_string(), serde_json::json!(node_id)),
            ("status".to_string(), serde_json::json!(status)),
            ("node_kind".to_string(), serde_json::json!(kind.clone())),
            ("kind".to_string(), serde_json::json!(kind)),
            ("attempt".to_string(), serde_json::json!(attempt)),
        ]),
    }
}

fn event_identity(event: &DagExecutionEvent) -> (String, Option<String>, Option<i64>) {
    (
        event.event_type.clone(),
        event.node_id.clone(),
        event
            .payload
            .get("attempt")
            .and_then(serde_json::Value::as_i64),
    )
}

fn node_started_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    node: &DagNode,
    attempt: u32,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let kind = node.kind.to_string();
    let mut event = with_manifest_identity(
        DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.started".to_string(),
            node_id: Some(node.id.clone()),
            message: Some(format!("{} started", node.id)),
            payload: BTreeMap::from([
                ("node_id".to_string(), serde_json::json!(node.id)),
                ("node_kind".to_string(), serde_json::json!(kind.clone())),
                ("kind".to_string(), serde_json::json!(kind)),
                ("attempt".to_string(), serde_json::json!(attempt)),
            ]),
        },
        manifest,
        manifest_hash,
    );
    add_node_static_audit_payload(&mut event.payload, manifest, node);
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn node_retry_scheduled_event(
    manifest: &DagManifest,
    manifest_hash: &str,
    node: &DagNode,
    attempt: u32,
    next_attempt: u32,
    max_attempts: u32,
    backoff_ms: u64,
    inputs: &DagIo,
) -> DagExecutionEvent {
    let kind = node.kind.to_string();
    let mut event = with_manifest_identity(
        DagExecutionEvent {
            level: "warn".to_string(),
            event_type: "node.retry_scheduled".to_string(),
            node_id: Some(node.id.clone()),
            message: Some(format!("{} retry scheduled", node.id)),
            payload: BTreeMap::from([
                ("node_id".to_string(), serde_json::json!(node.id)),
                ("node_kind".to_string(), serde_json::json!(kind.clone())),
                ("kind".to_string(), serde_json::json!(kind)),
                ("attempt".to_string(), serde_json::json!(attempt)),
                ("next_attempt".to_string(), serde_json::json!(next_attempt)),
                ("max_attempts".to_string(), serde_json::json!(max_attempts)),
                ("backoff_ms".to_string(), serde_json::json!(backoff_ms)),
            ]),
        },
        manifest,
        manifest_hash,
    );
    add_node_static_audit_payload(&mut event.payload, manifest, node);
    add_runtime_identity_payload(&mut event.payload, inputs);
    event
}

fn emit_to_sink(sink: &Option<DagEventSink>, event: &DagExecutionEvent) {
    trace_execution_event(event);
    if let Some(sink) = sink {
        sink(event.clone());
    }
}

fn push_and_emit_event(
    events: &mut Vec<DagExecutionEvent>,
    sink: &Option<DagEventSink>,
    mut event: DagExecutionEvent,
) {
    normalize_agenthero_trace_event(&mut event);
    emit_to_sink(sink, &event);
    events.push(event);
}

fn normalize_agenthero_trace_event(event: &mut DagExecutionEvent) {
    if !event.payload.contains_key("node_id") {
        match event.node_id.as_deref() {
            Some(node_id) => {
                event
                    .payload
                    .insert("node_id".to_string(), serde_json::json!(node_id));
            }
            None => {
                event
                    .payload
                    .insert("node_id".to_string(), serde_json::Value::Null);
            }
        }
    }
    insert_alias_or_null(&mut event.payload, "node_kind", &["kind"]);
    insert_alias_or_null(&mut event.payload, "tool_id", &["tool"]);
    insert_alias_or_null(&mut event.payload, "duration_ms", &["latency_ms"]);
    for field in [
        "app_run_id",
        "dag_run_id",
        "attempt",
        "manifest_hash",
        "artifact_id",
        "lease_id",
        "status",
        "exit_status",
    ] {
        event
            .payload
            .entry(field.to_string())
            .or_insert(serde_json::Value::Null);
    }
}

fn insert_alias_or_null(
    payload: &mut BTreeMap<String, serde_json::Value>,
    canonical: &str,
    aliases: &[&str],
) {
    if payload.contains_key(canonical) {
        return;
    }
    if let Some(value) = aliases.iter().find_map(|alias| {
        payload
            .get(*alias)
            .filter(|value| !value.is_null())
            .cloned()
    }) {
        payload.insert(canonical.to_string(), value);
    } else {
        payload.insert(canonical.to_string(), serde_json::Value::Null);
    }
}

fn trace_execution_event(event: &DagExecutionEvent) {
    let dag_type = payload_str(&event.payload, "dag_type").unwrap_or_default();
    let app_run_id = payload_str(&event.payload, "app_run_id").unwrap_or_default();
    let dag_run_id = payload_str(&event.payload, "dag_run_id").unwrap_or_default();
    let manifest_version = payload_u64(&event.payload, "manifest_version").unwrap_or_default();
    let manifest_hash = payload_str(&event.payload, "manifest_hash").unwrap_or_default();
    let node_id = event.node_id.as_deref().unwrap_or_default();
    let status = payload_str(&event.payload, "status").unwrap_or_default();
    let kind = payload_str(&event.payload, "kind").unwrap_or_default();
    let node_kind = payload_str(&event.payload, "node_kind").unwrap_or(kind);
    let attempt = payload_u64(&event.payload, "attempt").unwrap_or_default();
    let latency_ms = payload_u64(&event.payload, "latency_ms").unwrap_or_default();
    let duration_ms = payload_u64(&event.payload, "duration_ms").unwrap_or(latency_ms);
    let message = event.message.as_deref().unwrap_or_default();
    let command = payload_json_string(&event.payload, "command").unwrap_or_default();
    let exit_status = payload_i64(&event.payload, "exit_status").unwrap_or_default();
    let model = payload_str(&event.payload, "model").unwrap_or_default();
    let prompt_hash = payload_str(&event.payload, "prompt_hash").unwrap_or_default();
    let executor = payload_str(&event.payload, "executor").unwrap_or_default();
    let tool = payload_str(&event.payload, "tool").unwrap_or_default();
    let tool_id = payload_str(&event.payload, "tool_id").unwrap_or(tool);
    let role = payload_str(&event.payload, "role").unwrap_or_default();
    let child_dag_type = payload_str(&event.payload, "child_dag_type").unwrap_or_default();
    let required = payload_bool(&event.payload, "required").unwrap_or(false);
    let artifact_id = payload_str(&event.payload, "artifact_id").unwrap_or_default();
    let lease_id = payload_str(&event.payload, "lease_id").unwrap_or_default();
    let output_refs = payload_json_string(&event.payload, "output_refs").unwrap_or_default();
    let diagnostic_refs =
        payload_json_string(&event.payload, "diagnostic_refs").unwrap_or_default();
    let input_refs = payload_json_string(&event.payload, "input_refs").unwrap_or_default();
    let error = payload_str(&event.payload, "error").unwrap_or_default();
    let warning = payload_str(&event.payload, "warning").unwrap_or_default();

    match event.level.as_str() {
        "error" => tracing::error!(
            target: "agenthero::dag",
            event_type = event.event_type.as_str(),
            app_run_id,
            dag_run_id,
            dag_type,
            manifest_version,
            manifest_hash,
            node_id,
            status,
            kind,
            node_kind,
            attempt,
            latency_ms,
            duration_ms,
            message,
            command,
            exit_status,
            model,
            prompt_hash,
            executor,
            tool,
            tool_id,
            role,
            child_dag_type,
            required,
            artifact_id,
            lease_id,
            output_refs,
            diagnostic_refs,
            input_refs,
            error,
            warning,
            "agenthero execution event"
        ),
        "warn" => tracing::warn!(
            target: "agenthero::dag",
            event_type = event.event_type.as_str(),
            app_run_id,
            dag_run_id,
            dag_type,
            manifest_version,
            manifest_hash,
            node_id,
            status,
            kind,
            node_kind,
            attempt,
            latency_ms,
            duration_ms,
            message,
            command,
            exit_status,
            model,
            prompt_hash,
            executor,
            tool,
            tool_id,
            role,
            child_dag_type,
            required,
            artifact_id,
            lease_id,
            output_refs,
            diagnostic_refs,
            input_refs,
            error,
            warning,
            "agenthero execution event"
        ),
        _ => tracing::info!(
            target: "agenthero::dag",
            event_type = event.event_type.as_str(),
            app_run_id,
            dag_run_id,
            dag_type,
            manifest_version,
            manifest_hash,
            node_id,
            status,
            kind,
            node_kind,
            attempt,
            latency_ms,
            duration_ms,
            message,
            command,
            exit_status,
            model,
            prompt_hash,
            executor,
            tool,
            tool_id,
            role,
            child_dag_type,
            required,
            artifact_id,
            lease_id,
            output_refs,
            diagnostic_refs,
            input_refs,
            error,
            warning,
            "agenthero execution event"
        ),
    }
}

fn payload_str<'a>(payload: &'a BTreeMap<String, serde_json::Value>, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(serde_json::Value::as_str)
}

fn payload_u64(payload: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<u64> {
    payload.get(key).and_then(serde_json::Value::as_u64)
}

fn payload_i64(payload: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<i64> {
    payload.get(key).and_then(serde_json::Value::as_i64)
}

fn payload_bool(payload: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<bool> {
    payload.get(key).and_then(serde_json::Value::as_bool)
}

fn payload_json_string(payload: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| value.to_string()))
}

fn is_ordinary_handler_node(node: &DagNode) -> bool {
    !matches!(
        node.kind,
        DagNodeKind::Branch
            | DagNodeKind::Gate
            | DagNodeKind::Approval
            | DagNodeKind::Loop
            | DagNodeKind::Map
    )
}

fn node_policy_trace(
    manifest: &DagManifest,
    node: &DagNode,
) -> BTreeMap<String, serde_json::Value> {
    let mut policy = BTreeMap::new();
    if let Some(tool_id) = node.tool.as_deref() {
        if let Some(tool) = manifest.tools.iter().find(|tool| tool.id == tool_id) {
            policy.insert("tool".to_string(), tool_policy_trace(tool));
        }
    }
    if let Some(gate) = node.gate.as_ref() {
        policy.insert("gate".to_string(), serde_json::json!(gate));
    }
    if let Some(loop_policy) = node.loop_policy.as_ref() {
        policy.insert("loop".to_string(), serde_json::json!(loop_policy));
    }
    if let Some(branch) = node.branch.as_ref() {
        policy.insert("branch".to_string(), serde_json::json!(branch));
    }
    if let Some(map) = node.map.as_ref() {
        policy.insert("map".to_string(), serde_json::json!(map));
    }
    if let Some(approval) = node.approval.as_ref() {
        policy.insert("approval".to_string(), serde_json::json!(approval));
    }
    if let Some(retry) = node.retry.as_ref() {
        policy.insert("retry".to_string(), serde_json::json!(retry));
    }
    policy
}

fn tool_policy_trace(tool: &DagTool) -> serde_json::Value {
    let budget_units = tool.policy.as_ref().and_then(|policy| policy.budget_units);
    let approval_required = tool
        .policy
        .as_ref()
        .is_some_and(|policy| policy.approval_required);
    let network = tool
        .policy
        .as_ref()
        .map(|policy| serde_json::json!(policy.network))
        .unwrap_or_else(|| serde_json::json!({ "allow": true }));
    let filesystem = tool
        .policy
        .as_ref()
        .map(|policy| serde_json::json!(policy.filesystem))
        .unwrap_or_else(|| serde_json::json!({ "read": [], "write": [] }));

    serde_json::json!({
        "executor": tool.executor.to_string(),
        "timeout_secs": tool.timeout_secs,
        "budget_units": budget_units,
        "approval_required": approval_required,
        "network": network,
        "filesystem": filesystem,
    })
}

fn normalize_success_status(status: DagNodeStatus) -> DagNodeStatus {
    match status {
        DagNodeStatus::Ok
        | DagNodeStatus::Degraded
        | DagNodeStatus::Failed
        | DagNodeStatus::AwaitingApproval => status,
        DagNodeStatus::Pending | DagNodeStatus::Running | DagNodeStatus::Skipped => {
            DagNodeStatus::Ok
        }
    }
}

fn normalize_node_result(
    node: &DagNode,
    node_result: anyhow::Result<NodeExecutionResult>,
) -> NormalizedNodeResult {
    match node_result {
        Ok(result) => {
            let status = normalize_success_status(result.status);
            NormalizedNodeResult {
                status,
                warning: result.warning,
                error: result.error,
                produced: Some(result.outputs),
                diagnostics: result.diagnostics,
                command: result.command,
                exit_status: result.exit_status,
                model: result.model,
                prompt_hash: result.prompt_hash,
                trace: result.trace,
            }
        }
        Err(err) if node.required => NormalizedNodeResult {
            status: DagNodeStatus::Failed,
            warning: None,
            error: Some(format!("{err:#}")),
            produced: None,
            diagnostics: DagIo::default(),
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        },
        Err(err) => NormalizedNodeResult {
            status: DagNodeStatus::Degraded,
            warning: Some(format!("{err:#}")),
            error: None,
            produced: None,
            diagnostics: DagIo::default(),
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        },
    }
}

fn dependency_map(manifest: &DagManifest) -> HashMap<String, Vec<String>> {
    let mut deps: HashMap<String, Vec<String>> = manifest
        .nodes
        .iter()
        .map(|node| (node.id.clone(), Vec::new()))
        .collect();
    for edge in &manifest.edges {
        let froms = one_or_many_values(&edge.from);
        for to in one_or_many_values(&edge.to) {
            deps.entry(to).or_default().extend(froms.iter().cloned());
        }
    }
    deps
}

fn has_blocking_dependency(
    node: &DagNode,
    deps: &HashMap<String, Vec<String>>,
    statuses: &HashMap<String, DagNodeStatus>,
) -> bool {
    deps.get(&node.id).into_iter().flatten().any(|dep| {
        matches!(
            statuses.get(dep),
            Some(DagNodeStatus::Failed | DagNodeStatus::Skipped | DagNodeStatus::AwaitingApproval)
        )
    })
}

fn is_unselected_by_branch(
    node: &DagNode,
    deps: &HashMap<String, Vec<String>>,
    branch_selections: &HashMap<String, BTreeSet<String>>,
) -> bool {
    deps.get(&node.id).into_iter().flatten().any(|dep| {
        branch_selections
            .get(dep)
            .is_some_and(|selected| !selected.contains(&node.id))
    })
}

fn branch_trace(
    node: &DagNode,
    inputs: &DagIo,
    selected: Option<&BTreeSet<String>>,
) -> BTreeMap<String, serde_json::Value> {
    let mut trace = BTreeMap::new();
    if let Some(branch) = node.branch.as_ref() {
        trace.insert(
            "decision_key".to_string(),
            serde_json::json!(branch.decision_key),
        );
        trace.insert(
            "decision".to_string(),
            inputs
                .values
                .get(&branch.decision_key)
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(selected) = selected {
        trace.insert(
            "selected".to_string(),
            serde_json::json!(selected.iter().cloned().collect::<Vec<_>>()),
        );
    }
    trace
}

fn evaluate_branch(
    node: &DagNode,
    inputs: &DagIo,
) -> (
    anyhow::Result<NodeExecutionResult>,
    Option<BTreeSet<String>>,
) {
    let Some(branch) = node.branch.as_ref() else {
        return (
            Err(anyhow::anyhow!(
                "branch node `{}` has no branch policy",
                node.id
            )),
            None,
        );
    };
    let decision = inputs
        .values
        .get(&branch.decision_key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let targets = branch
        .cases
        .get(decision)
        .or_else(|| (!branch.default.is_empty()).then_some(&branch.default));
    let Some(targets) = targets else {
        return (
            Err(anyhow::anyhow!(
                "branch `{}` has no case for decision `{decision}` and no default",
                node.id
            )),
            None,
        );
    };
    let selected = targets.iter().cloned().collect::<BTreeSet<_>>();
    let result = NodeExecutionResult::ok().with_value(
        node.id.clone(),
        serde_json::json!({
            "decision_key": branch.decision_key,
            "decision": decision,
            "selected": selected.iter().cloned().collect::<Vec<_>>(),
        }),
    );
    (Ok(result), Some(selected))
}

fn evaluate_gate(
    node: &DagNode,
    statuses: &HashMap<String, DagNodeStatus>,
) -> anyhow::Result<NodeExecutionResult> {
    let gate = node
        .gate
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("gate node `{}` has no gate policy", node.id))?;
    let min_usable = gate.min_usable.unwrap_or(1) as usize;
    let usable = gate
        .sources
        .iter()
        .filter(|source| {
            matches!(
                statuses.get(source.as_str()),
                Some(DagNodeStatus::Ok | DagNodeStatus::Degraded)
            )
        })
        .count();
    if usable >= min_usable {
        Ok(NodeExecutionResult::ok().with_value(
            node.id.clone(),
            serde_json::json!({
                "min_usable": min_usable,
                "usable": usable,
                "sources": gate.sources,
            }),
        ))
    } else {
        anyhow::bail!(
            "gate `{}` requires {} usable sources, found {}",
            node.id,
            min_usable,
            usable
        )
    }
}

fn evaluate_approval(node: &DagNode, inputs: &DagIo) -> anyhow::Result<NodeExecutionResult> {
    let approval = node
        .approval
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("approval node `{}` has no approval policy", node.id))?;
    let approved = inputs
        .values
        .get(&approval.approved_key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if approved {
        Ok(NodeExecutionResult::ok().with_value(
            node.id.clone(),
            serde_json::json!({
                "approved_key": approval.approved_key,
                "approved": true,
            }),
        ))
    } else {
        Ok(NodeExecutionResult {
            status: DagNodeStatus::AwaitingApproval,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: Some(format!(
                "approval node `{}` is waiting for `{}`",
                node.id, approval.approved_key
            )),
            error: None,
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: BTreeMap::new(),
        })
    }
}

fn approval_trace(node: &DagNode, inputs: &DagIo) -> BTreeMap<String, serde_json::Value> {
    let mut trace = BTreeMap::new();
    if let Some(approval) = node.approval.as_ref() {
        trace.insert(
            "approved_key".to_string(),
            serde_json::json!(approval.approved_key),
        );
        trace.insert(
            "approved".to_string(),
            serde_json::json!(inputs
                .values
                .get(&approval.approved_key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)),
        );
    }
    trace
}

fn node_executor_label(manifest: &DagManifest, node: &DagNode) -> Option<String> {
    match node.kind {
        DagNodeKind::Tool => node
            .tool
            .as_deref()
            .and_then(|tool_id| find_tool(manifest, tool_id))
            .map(|tool| tool.executor.to_string()),
        DagNodeKind::Agent | DagNodeKind::Synthesizer | DagNodeKind::Verify => {
            node.role.as_ref().map(ToString::to_string)
        }
        DagNodeKind::DagCall => node.dag_type.clone(),
        DagNodeKind::Loop => node
            .tool
            .clone()
            .or_else(|| node.role.as_ref().map(ToString::to_string))
            .or_else(|| node.dag_type.clone()),
        DagNodeKind::Map => node
            .tool
            .clone()
            .or_else(|| node.role.as_ref().map(ToString::to_string))
            .or_else(|| node.dag_type.clone()),
        _ => None,
    }
}

fn overall_status(reports: &[DagNodeReport]) -> DagNodeStatus {
    let mut final_statuses = BTreeMap::new();
    for report in reports {
        final_statuses.insert(report.node_id.as_str(), report.status);
    }
    if final_statuses
        .values()
        .any(|status| *status == DagNodeStatus::Failed)
    {
        DagNodeStatus::Failed
    } else if final_statuses
        .values()
        .any(|status| *status == DagNodeStatus::AwaitingApproval)
    {
        DagNodeStatus::AwaitingApproval
    } else if final_statuses
        .values()
        .any(|status| *status == DagNodeStatus::Degraded)
    {
        DagNodeStatus::Degraded
    } else {
        DagNodeStatus::Ok
    }
}

fn one_or_many_values(values: &OneOrMany) -> Vec<String> {
    match values {
        OneOrMany::One(value) => vec![value.clone()],
        OneOrMany::Many(values) => values.clone(),
    }
}
