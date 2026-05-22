//! Generic manifest-driven DAG execution contracts.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use agenthero_dag_runtime::{
    DagManifest, DagNode, DagNodeKind, DagNodeReport, DagNodeStatus, OneOrMany,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
    /// Optional warning recorded in the run report.
    pub warning: Option<String>,
}

impl NodeExecutionResult {
    /// Successful node result with no outputs.
    pub fn ok() -> Self {
        Self {
            status: DagNodeStatus::Ok,
            outputs: DagIo::default(),
            warning: None,
        }
    }

    /// Degraded node result with no outputs.
    pub fn degraded(warning: impl Into<String>) -> Self {
        Self {
            status: DagNodeStatus::Degraded,
            outputs: DagIo::default(),
            warning: Some(warning.into()),
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
    /// Overall run status.
    pub status: DagNodeStatus,
    /// Per-node report entries in execution order.
    #[serde(default)]
    pub nodes: Vec<DagNodeReport>,
    /// Final values/artifacts after all executable nodes finish.
    #[serde(default)]
    pub outputs: DagIo,
}

impl DagExecutionReport {
    /// Return the status recorded for one node id.
    pub fn node_status(&self, node_id: &str) -> Option<DagNodeStatus> {
        self.nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .map(|node| node.status)
    }
}

/// Manifest-driven executor for one DAG app handler.
#[derive(Debug, Clone)]
pub struct DagExecutor<H> {
    handler: H,
}

impl<H> DagExecutor<H>
where
    H: NodeHandler,
{
    /// Build an executor using the supplied app-side handler.
    pub fn new(handler: H) -> Self {
        Self { handler }
    }

    /// Execute a validated manifest.
    pub async fn execute(
        &self,
        manifest: &DagManifest,
        input: DagIo,
    ) -> anyhow::Result<DagExecutionReport> {
        manifest.validate()?;

        let deps = dependency_map(manifest);
        let node_by_id: HashMap<&str, &DagNode> = manifest
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let mut statuses: HashMap<String, DagNodeStatus> = HashMap::new();
        let mut outputs = input;
        let mut reports = Vec::new();

        for layer in manifest.execution_layers()? {
            for node_id in layer {
                let node = node_by_id.get(node_id.as_str()).copied().ok_or_else(|| {
                    anyhow::anyhow!("manifest layer referenced missing node `{node_id}`")
                })?;

                if has_failed_dependency(node, &deps, &statuses) {
                    statuses.insert(node.id.clone(), DagNodeStatus::Skipped);
                    reports.push(DagNodeReport {
                        node_id: node.id.clone(),
                        kind: node.kind.to_string(),
                        status: DagNodeStatus::Skipped,
                        executor: None,
                        inputs: node.inputs.clone(),
                        outputs: node.outputs.clone(),
                        warning: None,
                        error: Some("required dependency failed or was skipped".to_string()),
                        latency_ms: Some(0),
                    });
                    continue;
                }

                let started = Instant::now();
                let snapshot = outputs.clone();
                let node_result = if node.kind == DagNodeKind::Gate {
                    evaluate_gate(node, &statuses)
                } else {
                    self.handler
                        .execute_node(NodeExecutionContext {
                            manifest,
                            node,
                            inputs: &snapshot,
                        })
                        .await
                };
                let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

                let (status, warning, error, produced) = match node_result {
                    Ok(result) => {
                        let status = normalize_success_status(result.status);
                        (status, result.warning, None, Some(result.outputs))
                    }
                    Err(err) if node.required => {
                        (DagNodeStatus::Failed, None, Some(format!("{err:#}")), None)
                    }
                    Err(err) => (
                        DagNodeStatus::Degraded,
                        Some(format!("{err:#}")),
                        None,
                        None,
                    ),
                };

                if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                    if let Some(produced) = produced {
                        outputs.merge(produced);
                    }
                }

                statuses.insert(node.id.clone(), status);
                reports.push(DagNodeReport {
                    node_id: node.id.clone(),
                    kind: node.kind.to_string(),
                    status,
                    executor: node_executor_label(node),
                    inputs: node.inputs.clone(),
                    outputs: node.outputs.clone(),
                    warning,
                    error,
                    latency_ms: Some(latency_ms),
                });
            }
        }

        let status = overall_status(&reports);
        Ok(DagExecutionReport {
            dag_type: manifest.id.clone(),
            status,
            nodes: reports,
            outputs,
        })
    }
}

fn normalize_success_status(status: DagNodeStatus) -> DagNodeStatus {
    match status {
        DagNodeStatus::Ok | DagNodeStatus::Degraded | DagNodeStatus::Failed => status,
        DagNodeStatus::Pending | DagNodeStatus::Running | DagNodeStatus::Skipped => {
            DagNodeStatus::Ok
        }
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

fn has_failed_dependency(
    node: &DagNode,
    deps: &HashMap<String, Vec<String>>,
    statuses: &HashMap<String, DagNodeStatus>,
) -> bool {
    deps.get(&node.id).into_iter().flatten().any(|dep| {
        matches!(
            statuses.get(dep),
            Some(DagNodeStatus::Failed | DagNodeStatus::Skipped)
        )
    })
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

fn node_executor_label(node: &DagNode) -> Option<String> {
    match node.kind {
        DagNodeKind::Tool => node.tool.clone(),
        DagNodeKind::Agent | DagNodeKind::Synthesizer | DagNodeKind::Verify => {
            node.role.as_ref().map(ToString::to_string)
        }
        DagNodeKind::DagCall => node.dag_type.clone(),
        _ => None,
    }
}

fn overall_status(reports: &[DagNodeReport]) -> DagNodeStatus {
    if reports.iter().any(|report| {
        matches!(
            report.status,
            DagNodeStatus::Failed | DagNodeStatus::Skipped
        )
    }) {
        DagNodeStatus::Failed
    } else if reports
        .iter()
        .any(|report| report.status == DagNodeStatus::Degraded)
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
