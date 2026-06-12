//! Generic manifest-driven DAG execution contracts.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use agenthero_dag_runtime::{
    DagManifest, DagNode, DagNodeKind, DagNodeReport, DagNodeStatus, OneOrMany,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
        let mut branch_selections: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut outputs = input;
        let mut reports = Vec::new();

        for layer in manifest.execution_layers()? {
            for node_id in layer {
                let node = node_by_id.get(node_id.as_str()).copied().ok_or_else(|| {
                    anyhow::anyhow!("manifest layer referenced missing node `{node_id}`")
                })?;

                if is_unselected_by_branch(node, &deps, &branch_selections) {
                    statuses.insert(node.id.clone(), DagNodeStatus::Skipped);
                    reports.push(DagNodeReport {
                        node_id: node.id.clone(),
                        kind: node.kind.to_string(),
                        status: DagNodeStatus::Skipped,
                        executor: None,
                        inputs: node.inputs.clone(),
                        outputs: node.outputs.clone(),
                        warning: Some("branch not selected".to_string()),
                        error: None,
                        latency_ms: Some(0),
                        trace: BTreeMap::from([(
                            "skip_reason".to_string(),
                            serde_json::json!("branch_not_selected"),
                        )]),
                    });
                    continue;
                }

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
                        trace: BTreeMap::new(),
                    });
                    continue;
                }

                if node.kind == DagNodeKind::Branch {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (node_result, selected) = evaluate_branch(node, &snapshot);
                    let trace = branch_trace(node, &snapshot, selected.as_ref());
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    let (status, warning, error, produced) =
                        normalize_node_result(node, node_result);

                    if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                        if let Some(selected) = selected {
                            branch_selections.insert(node.id.clone(), selected);
                        }
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
                        trace,
                    });
                    continue;
                }

                if node.kind == DagNodeKind::Loop {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (status, warning, error, produced, mut round_reports) =
                        self.execute_loop_node(manifest, node, &snapshot).await;
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

                    if let Some(produced) = produced {
                        outputs.merge(produced);
                    }

                    statuses.insert(node.id.clone(), status);
                    reports.append(&mut round_reports);
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
                        trace: BTreeMap::new(),
                    });
                    continue;
                }

                if node.kind == DagNodeKind::Map {
                    let started = Instant::now();
                    let snapshot = outputs.clone();
                    let (status, warning, error, produced, mut item_reports) =
                        self.execute_map_node(manifest, node, &snapshot).await;
                    let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

                    if let Some(produced) = produced {
                        outputs.merge(produced);
                    }

                    statuses.insert(node.id.clone(), status);
                    reports.append(&mut item_reports);
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
                        trace: BTreeMap::new(),
                    });
                    continue;
                }

                let started = Instant::now();
                let snapshot = outputs.clone();
                let node_result = if node.kind == DagNodeKind::Gate {
                    evaluate_gate(node, &statuses)
                } else if node.kind == DagNodeKind::Approval {
                    evaluate_approval(node, &snapshot)
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

                let (status, warning, error, produced) = normalize_node_result(node, node_result);

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
                    trace: approval_trace(node, &snapshot),
                });

                if status == DagNodeStatus::AwaitingApproval {
                    return Ok(DagExecutionReport {
                        dag_type: manifest.id.clone(),
                        status: DagNodeStatus::AwaitingApproval,
                        nodes: reports,
                        outputs,
                    });
                }
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
    ) {
        let Some(policy) = node.loop_policy.as_ref() else {
            return (
                DagNodeStatus::Failed,
                None,
                Some(format!("loop node `{}` has no loop policy", node.id)),
                None,
                Vec::new(),
            );
        };

        let mut visible_inputs = inputs.clone();
        let mut combined_outputs = DagIo::default();
        let mut round_reports = Vec::new();
        let mut saw_degraded = false;

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
            let node_result = self
                .handler
                .execute_node(NodeExecutionContext {
                    manifest,
                    node,
                    inputs: &round_inputs,
                })
                .await;
            let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

            match node_result {
                Ok(result) => {
                    let status = normalize_success_status(result.status);
                    saw_degraded |= status == DagNodeStatus::Degraded;
                    let continue_requested = result
                        .outputs
                        .values
                        .get(&policy.continue_key)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let warning = result.warning.clone();
                    let produced = result.outputs;

                    if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                        visible_inputs.merge(produced.clone());
                        combined_outputs.merge(produced);
                    }

                    round_reports.push(loop_round_report(
                        node,
                        round,
                        status,
                        warning.clone(),
                        None,
                        latency_ms,
                    ));

                    if status == DagNodeStatus::Failed {
                        return (
                            DagNodeStatus::Failed,
                            warning,
                            Some(format!("loop node `{}` failed in round {round}", node.id)),
                            Some(combined_outputs),
                            round_reports,
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
                        );
                    }
                }
                Err(err) if node.required => {
                    let error = format!("{err:#}");
                    round_reports.push(loop_round_report(
                        node,
                        round,
                        DagNodeStatus::Failed,
                        None,
                        Some(error.clone()),
                        latency_ms,
                    ));
                    return (
                        DagNodeStatus::Failed,
                        None,
                        Some(error),
                        Some(combined_outputs),
                        round_reports,
                    );
                }
                Err(err) => {
                    let warning = format!("{err:#}");
                    round_reports.push(loop_round_report(
                        node,
                        round,
                        DagNodeStatus::Degraded,
                        Some(warning.clone()),
                        None,
                        latency_ms,
                    ));
                    return (
                        DagNodeStatus::Degraded,
                        Some(warning),
                        None,
                        Some(combined_outputs),
                        round_reports,
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
    ) {
        let Some(policy) = node.map.as_ref() else {
            return (
                DagNodeStatus::Failed,
                None,
                Some(format!("map node `{}` has no map policy", node.id)),
                None,
                Vec::new(),
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
            );
        }

        let mut visible_inputs = inputs.clone();
        let mut combined_outputs = DagIo::default();
        let mut item_reports = Vec::new();
        let mut saw_degraded = false;

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
            let node_result = self
                .handler
                .execute_node(NodeExecutionContext {
                    manifest,
                    node,
                    inputs: &item_inputs,
                })
                .await;
            let latency_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

            match node_result {
                Ok(result) => {
                    let status = normalize_success_status(result.status);
                    saw_degraded |= status == DagNodeStatus::Degraded;
                    let warning = result.warning.clone();
                    let produced = result.outputs;
                    if matches!(status, DagNodeStatus::Ok | DagNodeStatus::Degraded) {
                        visible_inputs.merge(produced.clone());
                        combined_outputs.merge(produced);
                    }
                    item_reports.push(map_item_report(
                        node,
                        index,
                        status,
                        warning.clone(),
                        None,
                        latency_ms,
                    ));
                    if status == DagNodeStatus::Failed {
                        return (
                            DagNodeStatus::Failed,
                            warning,
                            Some(format!("map node `{}` failed at item {index}", node.id)),
                            Some(combined_outputs),
                            item_reports,
                        );
                    }
                }
                Err(err) if node.required => {
                    let error = format!("{err:#}");
                    item_reports.push(map_item_report(
                        node,
                        index,
                        DagNodeStatus::Failed,
                        None,
                        Some(error.clone()),
                        latency_ms,
                    ));
                    return (
                        DagNodeStatus::Failed,
                        None,
                        Some(error),
                        Some(combined_outputs),
                        item_reports,
                    );
                }
                Err(err) => {
                    let warning = format!("{err:#}");
                    item_reports.push(map_item_report(
                        node,
                        index,
                        DagNodeStatus::Degraded,
                        Some(warning.clone()),
                        None,
                        latency_ms,
                    ));
                    return (
                        DagNodeStatus::Degraded,
                        Some(warning),
                        None,
                        Some(combined_outputs),
                        item_reports,
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
        )
    }
}

fn loop_round_report(
    node: &DagNode,
    round: u32,
    status: DagNodeStatus,
    warning: Option<String>,
    error: Option<String>,
    latency_ms: u64,
) -> DagNodeReport {
    DagNodeReport {
        node_id: format!("{}#round-{round}", node.id),
        kind: node.kind.to_string(),
        status,
        executor: node_executor_label(node),
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        warning,
        error,
        latency_ms: Some(latency_ms),
        trace: BTreeMap::from([("loop_round".to_string(), serde_json::json!(round))]),
    }
}

fn map_item_report(
    node: &DagNode,
    index: usize,
    status: DagNodeStatus,
    warning: Option<String>,
    error: Option<String>,
    latency_ms: u64,
) -> DagNodeReport {
    DagNodeReport {
        node_id: format!("{}#item-{index}", node.id),
        kind: node.kind.to_string(),
        status,
        executor: node_executor_label(node),
        inputs: node.inputs.clone(),
        outputs: node.outputs.clone(),
        warning,
        error,
        latency_ms: Some(latency_ms),
        trace: BTreeMap::from([("map_index".to_string(), serde_json::json!(index))]),
    }
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
) -> (DagNodeStatus, Option<String>, Option<String>, Option<DagIo>) {
    match node_result {
        Ok(result) => {
            let status = normalize_success_status(result.status);
            (status, result.warning, None, Some(result.outputs))
        }
        Err(err) if node.required => (DagNodeStatus::Failed, None, Some(format!("{err:#}")), None),
        Err(err) => (
            DagNodeStatus::Degraded,
            Some(format!("{err:#}")),
            None,
            None,
        ),
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
            warning: Some(format!(
                "approval node `{}` is waiting for `{}`",
                node.id, approval.approved_key
            )),
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

fn node_executor_label(node: &DagNode) -> Option<String> {
    match node.kind {
        DagNodeKind::Tool => node.tool.clone(),
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
    if reports
        .iter()
        .any(|report| report.status == DagNodeStatus::Failed)
    {
        DagNodeStatus::Failed
    } else if reports
        .iter()
        .any(|report| report.status == DagNodeStatus::AwaitingApproval)
    {
        DagNodeStatus::AwaitingApproval
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
