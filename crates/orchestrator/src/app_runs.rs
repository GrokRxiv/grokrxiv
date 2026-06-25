//! DB-backed app-run repository and HTTP DTOs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read as _;
use std::path::PathBuf;

use agenthero_dag_executor::{DagExecutionReport, DagIo};
use agenthero_dag_runtime::DagNodeStatus;
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serde::{ser::SerializeStruct as _, Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const APP_RUN_EVENT_DETAIL_LIMIT: usize = 500;
const LIVE_NODE_SUMMARY_LIMIT: usize = 500;
const LIVE_NODE_EVENT_TYPES: &[&str] = &[
    "node.queued",
    "node.started",
    "node.retry_scheduled",
    "node.awaiting_approval",
    "node.completed",
    "node.failed",
    "node.skipped",
    "node.cancelled",
];
const LIVE_ACTION_EVENT_TYPES: &[&str] = &[
    "app_action.started",
    "app_action.awaiting_approval",
    "app_action.completed",
    "app_action.failed",
    "app_action.cancelled",
];

/// Request body for enqueueing an app action run.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppRunRequest {
    /// Action-specific args after the app command path.
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial DAG input.
    #[serde(default)]
    pub input: DagIo,
    /// Whether this run should be plan-only.
    #[serde(default)]
    pub dry_run: bool,
    /// Whether the adapter should emit JSON-oriented output.
    #[serde(default = "default_json")]
    pub json: bool,
}

/// Stored app-run input payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredAppRunInput {
    /// Action-specific args after the app command path.
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial DAG input.
    #[serde(default)]
    pub input: DagIo,
    /// Whether this run should be plan-only.
    #[serde(default)]
    pub dry_run: bool,
    /// Whether the adapter should emit JSON-oriented output.
    #[serde(default = "default_json")]
    pub json: bool,
    /// Latest DAG execution report used as a replay checkpoint on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<DagExecutionReport>,
    /// Scheduler retry policy captured when the run was queued.
    #[serde(default)]
    pub retry: StoredAppRunRetry,
}

impl Default for StoredAppRunInput {
    fn default() -> Self {
        Self {
            args: Vec::new(),
            input: DagIo::default(),
            dry_run: false,
            json: default_json(),
            checkpoint: None,
            retry: StoredAppRunRetry::default(),
        }
    }
}

/// Stored retry policy for one app run.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct StoredAppRunRetry {
    /// Maximum worker attempts before auto-retry stops.
    pub max_attempts: i32,
}

impl Default for StoredAppRunRetry {
    fn default() -> Self {
        Self { max_attempts: 2 }
    }
}

/// App-run row returned by list/detail APIs.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunRecord {
    /// Run id.
    pub id: Uuid,
    /// Product app id.
    pub app_id: String,
    /// App action id.
    pub action_id: String,
    /// Run state.
    pub state: String,
    /// Stored input.
    pub input: serde_json::Value,
    /// Stored output.
    pub output: serde_json::Value,
    /// Optional error code.
    pub error_code: Option<String>,
    /// Optional error message.
    pub error_message: Option<String>,
    /// Optional retryability marker.
    pub error_retryable: Option<bool>,
    /// Number of worker attempts that have claimed this run.
    pub attempt: i32,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Start timestamp.
    pub started_at: Option<DateTime<Utc>>,
    /// Finish timestamp.
    pub finished_at: Option<DateTime<Utc>>,
}

/// App-run row returned by list APIs with shallow monitor data.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunListItem {
    /// Top-level app-run row.
    #[serde(flatten)]
    pub run: AppRunRecord,
    /// Shallow app-neutral monitor summary for this run.
    pub observability: AppRunListObservabilitySummary,
}

/// Shallow durable observability counters for one app run.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunObservabilitySummary {
    /// Number of durable events recorded for the app run.
    pub event_count: usize,
    /// Whether the durable app-run log file exists.
    pub log_exists: bool,
    /// Durable log file size in bytes, when readable.
    pub log_bytes: Option<u64>,
}

/// Shallow observability summary for one app run in list APIs.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunListObservabilitySummary {
    /// Number of durable events recorded for the app run.
    pub event_count: usize,
    /// Whether the durable app-run log file exists.
    pub log_exists: bool,
    /// Durable log file size in bytes, when readable.
    pub log_bytes: Option<u64>,
    /// App-neutral monitor and audit links for this run.
    pub links: AppRunObservabilityLinks,
}

/// Claimed app run for scheduler workers.
#[derive(Debug, Clone)]
pub struct ClaimedAppRun {
    /// Run id.
    pub id: Uuid,
    /// Worker id that claimed this run.
    pub worker_id: Uuid,
    /// Product app id.
    pub app_id: String,
    /// App action id.
    pub action_id: String,
    /// Stored run input.
    pub input: StoredAppRunInput,
    /// Durable DAG run id assigned before adapter execution.
    pub dag_run_id: Uuid,
    /// Worker lease id.
    pub lease_id: Uuid,
    /// Attempt number assigned to this claim.
    pub attempt: i32,
}

/// Runtime identity known once a scheduler worker has claimed an app run.
#[derive(Debug, Clone, Copy)]
pub struct AppRunRuntimeIdentity {
    /// Durable DAG run id assigned before adapter execution.
    pub dag_run_id: Uuid,
    /// Worker lease id for this app-run attempt.
    pub lease_id: Uuid,
}

/// Metadata returned after queueing a replay from a persisted app run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppRunReplayQueued {
    /// Newly queued replay app-run id.
    pub id: Uuid,
    /// Source app-run id used as the checkpoint source.
    pub source_run_id: Uuid,
    /// App id copied from the source run.
    pub app_id: String,
    /// Action id copied from the source run.
    pub action_id: String,
    /// Checkpoint DAG type.
    pub dag_type: String,
    /// Checkpoint manifest hash.
    pub manifest_hash: String,
}

/// Metadata returned when planning a replay without queueing a new app run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppRunReplayPlan {
    /// Source app-run id used as the checkpoint source.
    pub source_run_id: Uuid,
    /// App id copied from the source run.
    pub app_id: String,
    /// Action id copied from the source run.
    pub action_id: String,
    /// Checkpoint DAG type.
    pub dag_type: String,
    /// Checkpoint manifest hash.
    pub manifest_hash: String,
}

/// App-run event row.
#[derive(Debug, Clone)]
pub struct AppRunEvent {
    /// Event id.
    pub id: i64,
    /// Event level.
    pub level: String,
    /// Event type.
    pub event_type: String,
    /// Optional human message.
    pub message: Option<String>,
    /// Event payload.
    pub payload: serde_json::Value,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

impl Serialize for AppRunEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AppRunEvent", 8)?;
        let node_id = payload_string(&self.payload, "node_id");
        let attempt = payload_i32(&self.payload, "attempt");
        state.serialize_field("id", &self.id)?;
        state.serialize_field("level", &self.level)?;
        state.serialize_field("event_type", &self.event_type)?;
        state.serialize_field("node_id", &node_id)?;
        state.serialize_field("attempt", &attempt)?;
        state.serialize_field("message", &self.message)?;
        state.serialize_field("payload", &self.payload)?;
        state.serialize_field("created_at", &self.created_at)?;
        state.end()
    }
}

/// Latest DAG-level observability for one app run.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunDagSummary {
    /// DAG run id.
    pub id: Uuid,
    /// DAG type.
    pub dag_type: String,
    /// Manifest version persisted for this run.
    pub manifest_version: Option<i32>,
    /// Stable manifest hash persisted for this run.
    pub manifest_hash: Option<String>,
    /// DAG run state.
    pub state: String,
    /// Frozen DAG input persisted for replay and audit.
    pub input: serde_json::Value,
    /// DAG output persisted for replay and audit.
    pub output: serde_json::Value,
    /// DAG run start timestamp.
    pub started_at: Option<DateTime<Utc>>,
    /// DAG run finish timestamp.
    pub finished_at: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Node-level observability for one persisted DAG run.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunNodeSummary {
    /// Node run id.
    pub id: Uuid,
    /// Manifest node id.
    pub node_id: String,
    /// Manifest node kind.
    pub node_kind: String,
    /// Node state.
    pub state: String,
    /// Attempt number.
    pub attempt: i32,
    /// Runner label.
    pub runner: Option<String>,
    /// LLM model id, when applicable.
    pub model: Option<String>,
    /// Prompt hash, when applicable.
    pub prompt_hash: Option<String>,
    /// Command invoked by the node, when applicable.
    pub command: serde_json::Value,
    /// Process or protocol exit status, when applicable.
    pub exit_status: Option<i32>,
    /// App-owned role id, when applicable.
    pub role: Option<String>,
    /// Tool id, when applicable.
    pub tool: Option<String>,
    /// Child DAG type, when applicable.
    pub child_dag_type: Option<String>,
    /// Whether the node is required by the DAG policy.
    pub required: bool,
    /// Input artifact references.
    pub input_refs: serde_json::Value,
    /// Output artifact references.
    pub output_refs: serde_json::Value,
    /// Diagnostic artifact references.
    pub diagnostic_refs: serde_json::Value,
    /// Node policy/provenance payload.
    pub policy: serde_json::Value,
    /// Persisted node input payload with artifact integrity snapshots.
    pub input: serde_json::Value,
    /// Persisted node output payload with artifact integrity snapshots.
    pub output: serde_json::Value,
    /// Node error message, when present.
    pub error_message: Option<String>,
    /// Node latency in milliseconds, when present.
    pub latency_ms: Option<i32>,
    /// Node start timestamp.
    pub started_at: Option<DateTime<Utc>>,
    /// Node finish timestamp.
    pub finished_at: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Artifact-level observability for one persisted DAG run.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunArtifactSummary {
    /// Artifact row id.
    pub id: Uuid,
    /// Artifact name.
    pub name: String,
    /// Artifact URI or path.
    pub uri: String,
    /// Optional media type.
    pub media_type: Option<String>,
    /// Optional sha256.
    pub sha256: Option<String>,
    /// Optional size in bytes.
    pub size_bytes: Option<i64>,
    /// Optional schema reference.
    pub schema_ref: Option<String>,
    /// Artifact metadata.
    pub metadata: serde_json::Value,
    /// Producing node id, when known.
    pub node_id: Option<String>,
    /// Producing node attempt, when known.
    pub attempt: Option<i32>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

/// Latest event-derived state for one node in an app run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppRunLiveNodeSummary {
    /// Manifest node id.
    pub node_id: String,
    /// Event-derived node state.
    pub state: String,
    /// Latest lifecycle event type for this node.
    pub event_type: String,
    /// Latest lifecycle event level.
    pub level: String,
    /// Attempt number from the event payload.
    pub attempt: i32,
    /// Manifest node kind, when reported by the executor.
    pub node_kind: Option<String>,
    /// Status from the event payload, when present.
    pub status: Option<String>,
    /// Human-readable event message.
    pub message: Option<String>,
    /// Latest event payload.
    pub payload: serde_json::Value,
    /// Latest event row id.
    pub event_id: i64,
    /// Latest event timestamp.
    pub updated_at: DateTime<Utc>,
}

/// App-neutral observability envelope for one app run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AppRunObservability {
    /// Latest persisted DAG run.
    pub latest_dag_run: Option<AppRunDagSummary>,
    /// Event-derived latest state per node, available during live execution.
    pub live_nodes: Vec<AppRunLiveNodeSummary>,
    /// Node attempts for the latest DAG run.
    pub nodes: Vec<AppRunNodeSummary>,
    /// Artifacts for the latest DAG run.
    pub artifacts: Vec<AppRunArtifactSummary>,
}

/// App-neutral determinism and replay readiness for one app run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppRunDeterminismSummary {
    /// Stable manifest hash for the latest DAG run.
    pub manifest_hash: Option<String>,
    /// Stable hash of the persisted frozen DAG input.
    pub frozen_input_hash: Option<String>,
    /// Stable hash of the persisted DAG output.
    pub dag_output_hash: Option<String>,
    /// Whether the app-run output carries a DAG report checkpoint.
    pub checkpoint_available: bool,
    /// Persisted node attempts in the latest DAG run.
    pub node_attempts: usize,
    /// Node attempts with persisted input hashes.
    pub node_input_hashes: usize,
    /// Node attempts with persisted output hashes.
    pub node_output_hashes: usize,
    /// Artifact rows for the latest DAG run.
    pub artifacts: usize,
    /// Artifact rows with sha256 integrity.
    pub artifacts_with_sha256: usize,
    /// Artifact rows missing sha256 integrity.
    pub artifacts_missing_sha256: usize,
    /// Whether checkpoint replay has enough persisted identity and node hashes.
    pub replay_ready: bool,
    /// Whether output comparison has enough frozen hashes and artifact integrity.
    pub compare_ready: bool,
}

/// Per-field comparison flags for two persisted app runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppRunComparisonChecks {
    /// App ids match.
    pub same_app: bool,
    /// Action ids match.
    pub same_action: bool,
    /// Latest DAG types match.
    pub same_dag_type: bool,
    /// Manifest hashes match.
    pub same_manifest_hash: bool,
    /// Frozen DAG input hashes match.
    pub same_frozen_input_hash: bool,
    /// DAG output hashes match.
    pub same_dag_output_hash: bool,
    /// Node output hash sets match.
    pub same_node_outputs: bool,
    /// Artifact sha256 sets match.
    pub same_artifacts: bool,
    /// Frozen DAG inputs match after AgentHero runtime identity fields are normalized.
    pub same_normalized_frozen_input_hash: bool,
    /// DAG outputs match after AgentHero runtime identity fields and artifact paths are normalized.
    pub same_normalized_dag_output_hash: bool,
    /// Node work products match after artifact paths and diagnostic paths are normalized.
    pub same_normalized_node_outputs: bool,
}

impl AppRunComparisonChecks {
    fn all(&self) -> bool {
        self.same_app
            && self.same_action
            && self.same_dag_type
            && self.same_manifest_hash
            && self.same_frozen_input_hash
            && self.same_dag_output_hash
            && self.same_node_outputs
            && self.same_artifacts
    }

    fn work_product_all(&self) -> bool {
        self.same_app
            && self.same_action
            && self.same_dag_type
            && self.same_manifest_hash
            && self.same_normalized_frozen_input_hash
            && self.same_normalized_dag_output_hash
            && self.same_normalized_node_outputs
            && self.same_artifacts
    }
}

/// Stable identity and determinism summary for one side of an app-run compare.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppRunComparisonSide {
    /// App run id.
    pub run_id: Uuid,
    /// App id.
    pub app_id: String,
    /// Action id.
    pub action_id: String,
    /// App-run state.
    pub state: String,
    /// Latest DAG run id, when available.
    pub dag_run_id: Option<Uuid>,
    /// Latest DAG type, when available.
    pub dag_type: Option<String>,
    /// Determinism summary from persisted runtime rows.
    pub determinism: AppRunDeterminismSummary,
    /// Frozen input hash with AgentHero runtime identity fields normalized.
    pub normalized_frozen_input_hash: Option<String>,
    /// DAG output hash with AgentHero runtime identity fields and artifact paths normalized.
    pub normalized_dag_output_hash: Option<String>,
}

/// One field-level difference between two app runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppRunComparisonDifference {
    /// Compared field or stable collection key.
    pub field: String,
    /// Left-side value.
    pub left: serde_json::Value,
    /// Right-side value.
    pub right: serde_json::Value,
}

/// App-neutral deterministic comparison of two app runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppRunComparison {
    /// Left app-run summary.
    pub left: AppRunComparisonSide,
    /// Right app-run summary.
    pub right: AppRunComparisonSide,
    /// Whether both runs have enough persisted hashes for output comparison.
    pub compare_ready: bool,
    /// Whether the runs are compare-ready and all comparison checks match.
    pub matches: bool,
    /// Whether deterministic work products match after runtime identity/path normalization.
    pub work_product_matches: bool,
    /// Individual comparison checks.
    pub checks: AppRunComparisonChecks,
    /// Field-level differences.
    pub differences: Vec<AppRunComparisonDifference>,
    /// Field-level work-product differences after runtime identity/path normalization.
    pub work_product_differences: Vec<AppRunComparisonDifference>,
}

/// App-neutral policy and isolation summary for one app run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppRunPolicySummary {
    /// Persisted node attempts in the latest DAG run.
    pub node_attempts: usize,
    /// Node attempts that carry any policy/provenance policy data.
    pub nodes_with_policy: usize,
    /// Nodes with an explicit timeout limit recorded in policy snapshots.
    pub timeout_limited_nodes: usize,
    /// Nodes with an explicit budget policy.
    pub budget_limited_nodes: usize,
    /// Sum of requested budget units across node attempts.
    pub budget_units_requested: u64,
    /// Manifest approval nodes.
    pub approval_gates: usize,
    /// Tool nodes requiring operator approval.
    pub approval_required_tools: usize,
    /// Tool nodes whose network policy denies outbound access.
    pub network_denied_nodes: usize,
    /// Tool nodes with read/write filesystem policy restrictions.
    pub filesystem_restricted_nodes: usize,
    /// Nodes that require an isolated runner to enforce their policy.
    pub isolation_required_nodes: usize,
    /// Nodes with retry policy attached.
    pub retry_policies: usize,
    /// Nodes that failed before execution because a policy denied the run.
    pub policy_denied_nodes: usize,
}

/// App-neutral monitor and audit surfaces for one app run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppRunObservabilityLinks {
    /// CLI status command.
    pub status_command: String,
    /// CLI durable log follow command.
    pub logs_command: String,
    /// HTTP durable log snapshot path.
    pub logs_path: String,
    /// CLI durable event follow command.
    pub events_command: String,
    /// HTTP durable event snapshot path.
    pub events_path: String,
    /// HTTP Server-Sent Events stream path.
    pub event_stream_path: String,
    /// HTTP Prometheus metrics path.
    pub metrics_path: String,
    /// Prometheus labels operators can use to filter metrics for this app run.
    pub metrics_labels: BTreeMap<String, String>,
    /// Runtime-local app-run log path.
    pub log_path: String,
    /// Mandatory trace fields emitted by AgentHero event payloads.
    pub trace_fields: &'static [&'static str],
    /// Durable event payload contract for monitor clients.
    pub event_contract: serde_json::Value,
    /// Durable log payload contract for monitor clients.
    pub log_contract: serde_json::Value,
    /// Server-Sent Events stream contract for web/TUI monitor clients.
    pub stream_contract: serde_json::Value,
}

impl AppRunObservabilityLinks {
    /// Build monitor and audit links for an app run.
    pub fn for_run(run_id: Uuid) -> Self {
        Self::for_run_context(run_id, None, None, None)
    }

    /// Build monitor and audit links with app/action/DAG metric labels.
    pub fn for_run_context(
        run_id: Uuid,
        app_id: Option<&str>,
        action_id: Option<&str>,
        dag_type: Option<&str>,
    ) -> Self {
        Self {
            status_command: format!("agh app status {run_id}"),
            logs_command: format!("agh app logs {run_id} --follow"),
            logs_path: format!("/app-runs/{run_id}/logs"),
            events_command: format!("agh app events {run_id} --follow"),
            events_path: format!("/app-runs/{run_id}/events"),
            event_stream_path: format!("/app-runs/{run_id}/events/stream"),
            metrics_path: "/metrics".to_string(),
            metrics_labels: metric_labels(app_id, action_id, dag_type),
            log_path: crate::dag_apps::app_run_log_path(run_id)
                .to_string_lossy()
                .to_string(),
            trace_fields: agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS,
            event_contract: json!({
                "trace_fields": agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS,
            }),
            log_contract: agenthero_agent_runtime::agenthero_log_contract(),
            stream_contract: json!({
                "format": "server_sent_events",
                "cursor_parameter": "after_id",
                "limit_parameter": "limit",
                "event_id_field": "id",
                "data": "AppRunEvent JSON",
            }),
        }
    }
}

fn metric_labels(
    app_id: Option<&str>,
    action_id: Option<&str>,
    dag_type: Option<&str>,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    if let Some(app_id) = app_id.filter(|value| !value.is_empty()) {
        labels.insert("app".to_string(), app_id.to_string());
    }
    if let Some(action_id) = action_id.filter(|value| !value.is_empty()) {
        labels.insert("action".to_string(), action_id.to_string());
    }
    if let Some(dag_type) = dag_type.filter(|value| !value.is_empty()) {
        labels.insert("dag_type".to_string(), dag_type.to_string());
    }
    labels
}

/// Full app-run detail for HTTP/status surfaces.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunDetail {
    /// Top-level app-run row.
    #[serde(flatten)]
    pub run: AppRunRecord,
    /// Latest persisted DAG run.
    pub latest_dag_run: Option<AppRunDagSummary>,
    /// Event-derived latest state per node, available during live execution.
    pub live_nodes: Vec<AppRunLiveNodeSummary>,
    /// Node attempts for the latest DAG run.
    pub nodes: Vec<AppRunNodeSummary>,
    /// Artifacts for the latest DAG run.
    pub artifacts: Vec<AppRunArtifactSummary>,
    /// Determinism and replay audit summary for the latest DAG run.
    pub determinism: AppRunDeterminismSummary,
    /// Policy and isolation audit summary for the latest DAG run.
    pub policies: AppRunPolicySummary,
    /// Shallow durable event/log counters for dashboards and operators.
    pub observability_summary: AppRunObservabilitySummary,
    /// App-neutral monitor and audit surfaces for this run.
    pub observability: AppRunObservabilityLinks,
    /// Recent durable event stream for the app run.
    pub recent_events: Vec<AppRunEvent>,
    /// Durable event stream for the app run. Kept as a compatibility alias for
    /// clients created before status/detail surfaces standardized on
    /// `recent_events`.
    pub events: Vec<AppRunEvent>,
}

/// Decision for an expired worker lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiredLeaseDecision {
    /// Requeue the run for one more worker attempt.
    Requeue,
    /// Mark the run as a terminal system failure.
    SystemFailed,
}

/// Result of one expired-lease recovery pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseRecoverySummary {
    /// App runs requeued for one more attempt.
    pub requeued: usize,
    /// App runs marked `system_failed`.
    pub system_failed: usize,
}

/// Decide how to handle an expired lease for a run that has already started
/// `attempt` worker attempts.
pub fn expired_lease_decision(attempt: i32, max_attempts: i32) -> ExpiredLeaseDecision {
    if attempt < max_attempts.max(1) {
        ExpiredLeaseDecision::Requeue
    } else {
        ExpiredLeaseDecision::SystemFailed
    }
}

fn default_json() -> bool {
    true
}

fn approval_requeue_input(
    mut input: StoredAppRunInput,
    approved_key: &str,
    current_attempt: i32,
    checkpoint: Option<DagExecutionReport>,
) -> StoredAppRunInput {
    input
        .input
        .values
        .insert(approved_key.to_string(), json!(true));
    if checkpoint.is_some() {
        input.checkpoint = checkpoint;
    }
    input.retry.max_attempts = input.retry.max_attempts.max(current_attempt + 1);
    input
}

fn approval_resume_replay_is_safe(completed_node_attempts: i64, has_checkpoint: bool) -> bool {
    completed_node_attempts == 0 || has_checkpoint
}

#[cfg(test)]
fn worker_completion_should_update_run(state: &str) -> bool {
    state == "running"
}

fn checkpoint_from_adapter_output(
    output: &serde_json::Value,
) -> anyhow::Result<Option<DagExecutionReport>> {
    Ok(output
        .get("report")
        .filter(|report| !report.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()?)
}

fn checkpoint_accepts_approval_key(checkpoint: &DagExecutionReport, approved_key: &str) -> bool {
    checkpoint
        .nodes
        .iter()
        .filter(|node| node.status == DagNodeStatus::AwaitingApproval)
        .any(|node| node_report_accepts_approval_key(node, approved_key))
}

fn node_report_accepts_approval_key(
    node: &agenthero_dag_runtime::DagNodeReport,
    approved_key: &str,
) -> bool {
    if node
        .policy
        .get("approval")
        .and_then(|approval| approval.get("approved_key"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|key| key == approved_key)
    {
        return true;
    }
    let Some(tool_id) = node.tool.as_deref() else {
        return false;
    };
    if node.executor.as_deref() == Some("approval_gate") {
        return approved_key == format!("approval/{tool_id}");
    }
    node.policy
        .get("tool")
        .and_then(|tool| tool.get("approval_required"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && approved_key == format!("approval/{tool_id}")
}

/// Insert a queued app run.
pub async fn insert_queued(
    pool: &PgPool,
    app_id: &str,
    action_id: &str,
    request: AppRunRequest,
) -> anyhow::Result<Uuid> {
    let retry_policy = crate::dag_apps::app_action_retry_policy(app_id, action_id)?;
    let input = serde_json::to_value(StoredAppRunInput {
        args: request.args,
        input: request.input,
        dry_run: request.dry_run,
        json: request.json,
        checkpoint: None,
        retry: StoredAppRunRetry {
            max_attempts: retry_policy.max_attempts,
        },
    })?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "insert into app_runs (app_id, action_id, state, input) \
         values ($1, $2, 'queued', $3) returning id",
    )
    .bind(app_id)
    .bind(action_id)
    .bind(input)
    .fetch_one(pool)
    .await?;
    insert_event(
        pool,
        id,
        "info",
        "app_run.queued",
        Some("app run queued"),
        json!({ "retry": { "max_attempts": retry_policy.max_attempts } }),
    )
    .await?;
    Ok(id)
}

/// Insert a queued replay app run from one persisted source app run.
pub async fn insert_replay_queued(
    pool: &PgPool,
    source_run_id: Uuid,
) -> anyhow::Result<Option<AppRunReplayQueued>> {
    let Some(source) = get_run(pool, source_run_id).await? else {
        return Ok(None);
    };
    let retry_policy = crate::dag_apps::app_action_retry_policy(&source.app_id, &source.action_id)?;
    let retry = StoredAppRunRetry {
        max_attempts: retry_policy.max_attempts,
    };
    let replay_input = replay_input_from_completed_run(&source, retry)?;
    let plan = replay_plan_from_input(&source, &replay_input);
    let input = serde_json::to_value(&replay_input)?;
    let id = sqlx::query_scalar::<_, Uuid>(
        "insert into app_runs (app_id, action_id, state, input) \
         values ($1, $2, 'queued', $3) returning id",
    )
    .bind(&source.app_id)
    .bind(&source.action_id)
    .bind(input)
    .fetch_one(pool)
    .await?;
    insert_event(
        pool,
        id,
        "info",
        "app_run.replay_queued",
        Some("app run replay queued"),
        json!({
            "source_run_id": source_run_id,
            "replay": true,
            "checkpoint_dag_type": plan.dag_type,
            "checkpoint_manifest_hash": plan.manifest_hash,
            "retry": { "max_attempts": retry.max_attempts },
        }),
    )
    .await?;
    Ok(Some(AppRunReplayQueued {
        id,
        source_run_id: plan.source_run_id,
        app_id: plan.app_id,
        action_id: plan.action_id,
        dag_type: plan.dag_type,
        manifest_hash: plan.manifest_hash,
    }))
}

/// Plan a checkpoint replay without mutating app-run state.
pub async fn plan_replay_queued(
    pool: &PgPool,
    source_run_id: Uuid,
) -> anyhow::Result<Option<AppRunReplayPlan>> {
    let Some(source) = get_run(pool, source_run_id).await? else {
        return Ok(None);
    };
    let retry_policy = crate::dag_apps::app_action_retry_policy(&source.app_id, &source.action_id)?;
    let replay_input = replay_input_from_completed_run(
        &source,
        StoredAppRunRetry {
            max_attempts: retry_policy.max_attempts,
        },
    )?;
    Ok(Some(replay_plan_from_input(&source, &replay_input)))
}

fn replay_plan_from_input(
    source: &AppRunRecord,
    replay_input: &StoredAppRunInput,
) -> AppRunReplayPlan {
    let checkpoint = replay_input
        .checkpoint
        .as_ref()
        .expect("replay input helper requires checkpoint");
    AppRunReplayPlan {
        source_run_id: source.id,
        app_id: source.app_id.clone(),
        action_id: source.action_id.clone(),
        dag_type: checkpoint.dag_type.to_string(),
        manifest_hash: checkpoint.manifest_hash.clone(),
    }
}

fn replay_input_from_completed_run(
    source: &AppRunRecord,
    retry: StoredAppRunRetry,
) -> anyhow::Result<StoredAppRunInput> {
    let checkpoint = checkpoint_from_adapter_output(&source.output)?
        .ok_or_else(|| anyhow::anyhow!("app run {} has no replay checkpoint report", source.id))?;
    let mut input: StoredAppRunInput = serde_json::from_value(source.input.clone())
        .with_context(|| format!("parse stored input for app run {}", source.id))?;
    input.checkpoint = Some(checkpoint);
    input.retry = retry;
    Ok(input)
}

/// Register or refresh a worker node row.
pub async fn register_worker(pool: &PgPool, name: &str) -> anyhow::Result<Uuid> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "insert into worker_nodes (name, capabilities, state, last_heartbeat_at) \
         values ($1, '{}'::jsonb, 'online', now()) \
         on conflict (name) do update set \
           state = 'online', last_heartbeat_at = now(), updated_at = now() \
         returning id",
    )
    .bind(name)
    .fetch_one(pool)
    .await?)
}

/// Refresh a worker heartbeat while it is polling or executing work.
pub async fn heartbeat_worker(pool: &PgPool, worker_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "update worker_nodes set state = 'online', last_heartbeat_at = now(), updated_at = now() \
         where id = $1",
    )
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Extend an active app-run lease for long-running app actions.
pub async fn renew_lease(pool: &PgPool, lease_id: Uuid) -> anyhow::Result<()> {
    sqlx::query(
        "update worker_leases \
         set leased_until = now() + interval '15 minutes', updated_at = now() \
         where id = $1 and state = 'leased'",
    )
    .bind(lease_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Return the DAG type captured in the queued app-run input contract.
pub fn dag_type_for_claimed_run(input: &StoredAppRunInput) -> Option<&str> {
    input
        .input
        .values
        .get("dag_type")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

/// Reserve a live DAG run row before adapter execution starts.
///
/// This makes app/DAG status queryable while a worker is still running and
/// attaches the worker lease to the durable DAG identity used by the adapter.
pub async fn reserve_dag_run(
    pool: &PgPool,
    app_run_id: Uuid,
    lease_id: Uuid,
    dag_run_id: Uuid,
    dag_type: &str,
    input: &DagIo,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "insert into dag_runs \
         (id, app_run_id, dag_type, state, input, started_at, finished_at) \
         values ($1, $2, $3, 'running', $4, now(), null) \
         on conflict (id) do update set \
           app_run_id = excluded.app_run_id, \
           dag_type = excluded.dag_type, \
           state = 'running', \
           input = excluded.input, \
           started_at = coalesce(dag_runs.started_at, excluded.started_at), \
           finished_at = null, \
           updated_at = now()",
    )
    .bind(dag_run_id)
    .bind(app_run_id)
    .bind(dag_type)
    .bind(serde_json::to_value(input)?)
    .execute(&mut *tx)
    .await?;

    sqlx::query("update worker_leases set dag_run_id = $1, updated_at = now() where id = $2")
        .bind(dag_run_id)
        .bind(lease_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "insert into dag_events \
         (app_run_id, dag_run_id, level, event_type, message, payload) \
         values ($1, $2, 'info', 'dag_run.reserved', 'dag run reserved', $3)",
    )
    .bind(app_run_id)
    .bind(dag_run_id)
    .bind(durable_app_event_payload(
        app_run_id,
        json!({
            "dag_run_id": dag_run_id.to_string(),
            "lease_id": lease_id.to_string(),
            "dag_type": dag_type,
            "status": "running",
        }),
    ))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Claim the next queued app run for a worker.
pub async fn claim_next(pool: &PgPool, worker_id: Uuid) -> anyhow::Result<Option<ClaimedAppRun>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "select id, app_id, action_id, input, attempt \
         from app_runs \
         where state = 'queued' \
           and attempt < coalesce((input #>> '{retry,max_attempts}')::int, 2) \
         order by created_at asc \
         for update skip locked \
         limit 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };

    let id: Uuid = row.get("id");
    let attempt = row.get::<i32, _>("attempt") + 1;
    sqlx::query(
        "update app_runs set state = 'running', attempt = attempt + 1, \
         started_at = coalesce(started_at, now()), finished_at = null, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let lease_id = sqlx::query_scalar::<_, Uuid>(
        "insert into worker_leases (worker_id, app_run_id, state, leased_until) \
         values ($1, $2, 'leased', now() + interval '15 minutes') returning id",
    )
    .bind(worker_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let dag_run_id = Uuid::new_v4();
    tx.commit().await?;

    let input_value: serde_json::Value = row.get("input");
    let input: StoredAppRunInput = serde_json::from_value(input_value)?;
    Ok(Some(ClaimedAppRun {
        id,
        worker_id,
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        input,
        dag_run_id,
        lease_id,
        attempt,
    }))
}

/// Claim one specific queued app run for a worker.
pub async fn claim_run(
    pool: &PgPool,
    worker_id: Uuid,
    run_id: Uuid,
) -> anyhow::Result<Option<ClaimedAppRun>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "select id, app_id, action_id, input, attempt \
         from app_runs \
         where id = $1 \
           and state = 'queued' \
           and attempt < coalesce((input #>> '{retry,max_attempts}')::int, 2) \
         for update skip locked",
    )
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };

    let id: Uuid = row.get("id");
    let attempt = row.get::<i32, _>("attempt") + 1;
    sqlx::query(
        "update app_runs set state = 'running', attempt = attempt + 1, \
         started_at = coalesce(started_at, now()), finished_at = null, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let lease_id = sqlx::query_scalar::<_, Uuid>(
        "insert into worker_leases (worker_id, app_run_id, state, leased_until) \
         values ($1, $2, 'leased', now() + interval '15 minutes') returning id",
    )
    .bind(worker_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let dag_run_id = Uuid::new_v4();
    tx.commit().await?;

    let input_value: serde_json::Value = row.get("input");
    let input: StoredAppRunInput = serde_json::from_value(input_value)?;
    Ok(Some(ClaimedAppRun {
        id,
        worker_id,
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        input,
        dag_run_id,
        lease_id,
        attempt,
    }))
}

/// Recover app runs whose worker lease expired while still marked running.
///
/// Runs are requeued until the stored action retry budget is exhausted; after
/// that they become `system_failed` and remain retryable by explicit operator
/// action.
pub async fn recover_expired_leases(pool: &PgPool) -> anyhow::Result<LeaseRecoverySummary> {
    let requeued_rows = sqlx::query(
        "with expired as ( \
           update worker_leases wl set state = 'expired', updated_at = now() \
           from app_runs ar \
           where wl.app_run_id = ar.id \
             and wl.state = 'leased' \
             and wl.leased_until < now() \
             and ar.state = 'running' \
             and ar.attempt < coalesce((ar.input #>> '{retry,max_attempts}')::int, 2) \
           returning wl.app_run_id \
         ) \
         update app_runs ar set state = 'queued', recovered_at = now(), \
           last_lease_expired_at = now(), updated_at = now(), \
           error_code = 'lease_expired', \
           error_message = 'worker lease expired; run requeued', \
           error_retryable = true \
         from expired \
         where ar.id = expired.app_run_id \
         returning ar.id",
    )
    .fetch_all(pool)
    .await?;

    let failed_rows = sqlx::query(
        "with expired as ( \
           update worker_leases wl set state = 'expired', updated_at = now() \
           from app_runs ar \
           where wl.app_run_id = ar.id \
             and wl.state = 'leased' \
             and wl.leased_until < now() \
             and ar.state = 'running' \
             and ar.attempt >= coalesce((ar.input #>> '{retry,max_attempts}')::int, 2) \
           returning wl.app_run_id \
         ) \
         update app_runs ar set state = 'system_failed', finished_at = coalesce(finished_at, now()), \
           last_lease_expired_at = now(), updated_at = now(), \
           error_code = 'lease_expired', \
           error_message = 'worker lease expired after retry', \
           error_retryable = true \
         from expired \
         where ar.id = expired.app_run_id \
         returning ar.id",
    )
    .fetch_all(pool)
    .await?;

    for row in &requeued_rows {
        let run_id: Uuid = row.get("id");
        insert_event(
            pool,
            run_id,
            "warn",
            "app_run.lease_expired_requeued",
            Some("worker lease expired; app run requeued"),
            json!({ "decision": "requeue" }),
        )
        .await?;
    }
    for row in &failed_rows {
        let run_id: Uuid = row.get("id");
        insert_event(
            pool,
            run_id,
            "error",
            "app_run.lease_expired_failed",
            Some("worker lease expired after retry"),
            json!({ "decision": "system_failed" }),
        )
        .await?;
    }

    Ok(LeaseRecoverySummary {
        requeued: requeued_rows.len(),
        system_failed: failed_rows.len(),
    })
}

/// Persist a successful adapter response.
pub async fn complete_success(
    pool: &PgPool,
    run_id: Uuid,
    output: serde_json::Value,
    report: Option<&DagExecutionReport>,
) -> anyhow::Result<bool> {
    complete_success_with_runtime(pool, run_id, None, output, report).await
}

/// Persist a successful adapter response with scheduler runtime identity.
pub async fn complete_success_with_runtime(
    pool: &PgPool,
    run_id: Uuid,
    runtime: Option<AppRunRuntimeIdentity>,
    output: serde_json::Value,
    report: Option<&DagExecutionReport>,
) -> anyhow::Result<bool> {
    let state = report.map(report_state).unwrap_or("done");
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "update app_runs set state = $2, output = $3, \
         finished_at = case when $2 = 'awaiting_approval' then null else now() end, \
         updated_at = now(), \
         error_code = null, error_message = null, error_retryable = null \
         where id = $1 and state = 'running'",
    )
    .bind(run_id)
    .bind(state)
    .bind(output)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(false);
    }
    if let Some(report) = report {
        persist_dag_report(&mut tx, run_id, report).await?;
    } else if let Some(runtime) = runtime {
        let dag_state = successful_app_terminal_dag_state(state);
        sqlx::query(
            "update dag_runs \
             set state = $3, \
                 finished_at = case when $3 = 'awaiting_approval' then null else coalesce(finished_at, now()) end, \
                 updated_at = now(), output = coalesce(output, '{}'::jsonb) \
             where app_run_id = $1 and id = $2 and state in ('queued', 'running', 'awaiting_approval')",
        )
        .bind(run_id)
        .bind(runtime.dag_run_id)
        .bind(dag_state)
        .execute(&mut *tx)
        .await?;
        let (dag_event_type, dag_message) = if dag_state == "awaiting_approval" {
            ("dag.awaiting_approval", "dag awaiting approval")
        } else {
            ("dag.completed", "dag completed")
        };
        insert_dag_event_tx(
            &mut tx,
            run_id,
            runtime.dag_run_id,
            "info",
            dag_event_type,
            Some(dag_message),
            durable_dag_event_payload(
                run_id,
                runtime.dag_run_id,
                runtime_event_payload(
                    Some(runtime),
                    None,
                    json!({
                        "state": dag_state,
                        "status": dag_state,
                    }),
                ),
            ),
        )
        .await?;
    }
    let (event_type, message) = if state == "awaiting_approval" {
        ("app_run.awaiting_approval", "app run awaiting approval")
    } else {
        ("app_run.finished", "app run finished")
    };
    insert_event_tx(
        &mut tx,
        run_id,
        "info",
        event_type,
        Some(message),
        runtime_event_payload(runtime, report, json!({ "state": state })),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Persist a failed adapter response or system failure.
pub async fn complete_failure(
    pool: &PgPool,
    run_id: Uuid,
    state: &str,
    code: &str,
    message: &str,
    retryable: bool,
) -> anyhow::Result<bool> {
    complete_failure_with_observability(pool, run_id, state, code, message, retryable, None, None)
        .await
}

/// Persist a failed adapter response with optional output/report diagnostics.
pub async fn complete_failure_with_observability(
    pool: &PgPool,
    run_id: Uuid,
    state: &str,
    code: &str,
    message: &str,
    retryable: bool,
    output: Option<serde_json::Value>,
    report: Option<&DagExecutionReport>,
) -> anyhow::Result<bool> {
    complete_failure_with_runtime_observability(
        pool, run_id, None, state, code, message, retryable, output, report,
    )
    .await
}

/// Persist a failed adapter response with scheduler runtime identity.
pub async fn complete_failure_with_runtime_observability(
    pool: &PgPool,
    run_id: Uuid,
    runtime: Option<AppRunRuntimeIdentity>,
    state: &str,
    code: &str,
    message: &str,
    retryable: bool,
    output: Option<serde_json::Value>,
    report: Option<&DagExecutionReport>,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "update app_runs set state = $2, error_code = $3, error_message = $4, \
         error_retryable = $5, output = coalesce($6::jsonb, output), finished_at = now(), updated_at = now() \
         where id = $1 and state = 'running'",
    )
    .bind(run_id)
    .bind(state)
    .bind(code)
    .bind(message)
    .bind(retryable)
    .bind(output)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(false);
    }
    if let Some(report) = report {
        persist_dag_report(&mut tx, run_id, report).await?;
    } else if let Some(runtime) = runtime {
        let dag_state = failed_app_terminal_dag_state(state);
        sqlx::query(
            "update dag_runs \
             set state = $3, finished_at = coalesce(finished_at, now()), updated_at = now(), \
                 error_code = $4, error_message = $5, error_retryable = $6 \
             where app_run_id = $1 and id = $2 and state in ('queued', 'running', 'awaiting_approval')",
        )
        .bind(run_id)
        .bind(runtime.dag_run_id)
        .bind(dag_state)
        .bind(code)
        .bind(message)
        .bind(retryable)
        .execute(&mut *tx)
        .await?;
        insert_dag_event_tx(
            &mut tx,
            run_id,
            runtime.dag_run_id,
            "error",
            "dag.failed",
            Some(message),
            failed_adapter_dag_event_payload(run_id, runtime, state, code, message, retryable),
        )
        .await?;
    }
    insert_event_tx(
        &mut tx,
        run_id,
        "error",
        "app_run.failed",
        Some(message),
        runtime_event_payload(
            runtime,
            report,
            json!({ "state": state, "code": code, "retryable": retryable }),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Record an approval decision and requeue a paused app run.
pub async fn approve_awaiting_run(
    pool: &PgPool,
    run_id: Uuid,
    approved_key: &str,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "select input, output, attempt from app_runs \
         where id = $1 and state = 'awaiting_approval' \
         for update",
    )
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(false);
    };

    let replay_unsafe_count: i64 = sqlx::query_scalar(
        "select count(*) from dag_run_nodes drn \
         join dag_runs dr on dr.id = drn.dag_run_id \
         where dr.app_run_id = $1 \
           and dr.id = (select id from dag_runs where app_run_id = $1 order by created_at desc, id desc limit 1) \
           and drn.state in ('ok', 'degraded', 'failed')",
    )
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    let output: serde_json::Value = row.get("output");
    let checkpoint = checkpoint_from_adapter_output(&output)?;
    if let Some(checkpoint) = checkpoint.as_ref() {
        if !checkpoint_accepts_approval_key(checkpoint, approved_key) {
            anyhow::bail!(
                "approval key `{approved_key}` does not match the awaiting approval checkpoint for app run {run_id}"
            );
        }
    }
    if !approval_resume_replay_is_safe(replay_unsafe_count, checkpoint.is_some()) {
        anyhow::bail!(
            "app run {run_id} has completed DAG node attempts; approval resume requires checkpoint replay support"
        );
    }

    let input: StoredAppRunInput = serde_json::from_value(row.get("input"))?;
    let attempt: i32 = row.get("attempt");
    let input = approval_requeue_input(input, approved_key, attempt, checkpoint);
    sqlx::query(
        "update app_runs set state = 'queued', input = $2, finished_at = null, \
         error_code = null, error_message = null, error_retryable = null, updated_at = now() \
         where id = $1",
    )
    .bind(run_id)
    .bind(serde_json::to_value(&input)?)
    .execute(&mut *tx)
    .await?;
    insert_event_tx(
        &mut tx,
        run_id,
        "info",
        "app_run.approved_requeued",
        Some(&format!("approved `{approved_key}` and requeued app run")),
        approval_requeued_event_payload(run_id, approved_key),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Mark a worker lease released.
pub async fn release_lease(pool: &PgPool, lease_id: Uuid, state: &str) -> anyhow::Result<()> {
    sqlx::query(
        "update worker_leases set state = $2, updated_at = now() \
         where id = $1 and state = 'leased'",
    )
    .bind(lease_id)
    .bind(state)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a reserved DAG run cancelled when its owning app run is cancelled.
pub async fn cancel_dag_run(
    pool: &PgPool,
    run_id: Uuid,
    dag_run_id: Uuid,
    message: &str,
) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "update dag_runs \
         set state = 'cancelled', finished_at = coalesce(finished_at, now()), updated_at = now(), \
             error_code = 'operator_cancelled', error_message = $3, error_retryable = false \
         where app_run_id = $1 and id = $2 and state in ('queued', 'running', 'awaiting_approval')",
    )
    .bind(run_id)
    .bind(dag_run_id)
    .bind(message)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Emit terminal cancellation events for nodes that were still live when a DAG
/// was cancelled by the scheduler.
pub async fn cancel_live_nodes(
    pool: &PgPool,
    run_id: Uuid,
    dag_run_id: Uuid,
    message: &str,
) -> anyhow::Result<usize> {
    let rows = sqlx::query(
        "select event_type, payload \
         from ( \
           select distinct on (payload->>'node_id') event_type, payload, created_at, id \
           from dag_events \
           where app_run_id = $1 \
             and dag_run_id = $2 \
             and payload->>'node_id' is not null \
             and event_type in ( \
               'node.queued', 'node.started', 'node.retry_scheduled', 'node.awaiting_approval', \
               'node.completed', 'node.failed', 'node.skipped', 'node.cancelled' \
             ) \
           order by payload->>'node_id', created_at desc, id desc \
         ) latest \
         where event_type in ('node.queued', 'node.started', 'node.retry_scheduled', 'node.awaiting_approval') \
         order by payload->>'node_id'",
    )
    .bind(run_id)
    .bind(dag_run_id)
    .fetch_all(pool)
    .await?;

    let mut cancelled = 0usize;
    for row in rows {
        let payload: serde_json::Value = row.get("payload");
        let node_id = payload
            .get("node_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        insert_dag_event(
            pool,
            run_id,
            dag_run_id,
            "warn",
            "node.cancelled",
            Some(&format!("{node_id} cancelled")),
            cancelled_node_event_payload(run_id, dag_run_id, payload, message),
        )
        .await?;
        cancelled += 1;
    }
    Ok(cancelled)
}

fn cancelled_node_event_payload(
    run_id: Uuid,
    dag_run_id: Uuid,
    mut payload: serde_json::Value,
    message: &str,
) -> serde_json::Value {
    if !payload.is_object() {
        payload = json!({});
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("status".to_string(), json!("cancelled"));
        object.insert("cancelled".to_string(), json!(true));
        object.insert("cancel_reason".to_string(), json!(message));
    }
    durable_dag_event_payload(run_id, dag_run_id, payload)
}

/// Return the current durable state for one app run.
pub async fn run_state(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("select state from app_runs where id = $1")
            .bind(run_id)
            .fetch_optional(pool)
            .await?,
    )
}

/// List app runs.
pub async fn list_runs(
    pool: &PgPool,
    app: Option<&str>,
    state: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<AppRunRecord>> {
    let rows = sqlx::query(
        "select id, app_id, action_id, state, input, output, error_code, error_message, \
                error_retryable, attempt, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
           and ($2::text is null or state = $2) \
         order by created_at desc \
         limit $3",
    )
    .bind(app)
    .bind(state)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(record_from_row).collect())
}

/// List app runs with shallow monitor data for dashboards and web clients.
pub async fn list_run_items(
    pool: &PgPool,
    app: Option<&str>,
    state: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<AppRunListItem>> {
    let rows = sqlx::query(
        "select ar.id, ar.app_id, ar.action_id, ar.state, ar.input, ar.output, \
                ar.error_code, ar.error_message, ar.error_retryable, ar.attempt, \
                ar.created_at, ar.started_at, ar.finished_at, \
                (select count(*) from dag_events de where de.app_run_id = ar.id) as event_count \
         from app_runs ar \
         where ($1::text is null or ar.app_id = $1) \
           and ($2::text is null or ar.state = $2) \
         order by ar.created_at desc \
         limit $3",
    )
    .bind(app)
    .bind(state)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let event_count = row.get::<i64, _>("event_count");
            let run = record_from_row(row);
            let (log_exists, log_bytes) = app_run_log_metadata(run.id);
            AppRunListItem {
                observability: AppRunListObservabilitySummary {
                    event_count: usize::try_from(event_count).unwrap_or(0),
                    log_exists,
                    log_bytes,
                    links: AppRunObservabilityLinks::for_run_context(
                        run.id,
                        Some(&run.app_id),
                        Some(&run.action_id),
                        None,
                    ),
                },
                run,
            }
        })
        .collect())
}

/// Fetch one app run.
pub async fn get_run(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Option<AppRunRecord>> {
    let row = sqlx::query(
        "select id, app_id, action_id, state, input, output, error_code, error_message, \
                error_retryable, attempt, created_at, started_at, finished_at \
         from app_runs where id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(record_from_row))
}

/// Fetch one app run with latest DAG/node/artifact observability.
pub async fn get_run_detail(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Option<AppRunDetail>> {
    let Some(run) = get_run(pool, run_id).await? else {
        return Ok(None);
    };
    let observability = load_observability(pool, run_id).await?;
    let events = list_events(pool, run_id).await?;
    let event_count = count_events(pool, run_id).await?;
    let (log_exists, log_bytes) = app_run_log_metadata(run_id);
    let determinism = app_run_determinism_summary(
        &run.output,
        observability.latest_dag_run.as_ref(),
        &observability.nodes,
        &observability.artifacts,
    );
    let policies = app_run_policy_summary(&observability.nodes);
    let observability_links = AppRunObservabilityLinks::for_run_context(
        run_id,
        Some(&run.app_id),
        Some(&run.action_id),
        observability
            .latest_dag_run
            .as_ref()
            .map(|dag_run| dag_run.dag_type.as_str()),
    );
    Ok(Some(AppRunDetail {
        run,
        latest_dag_run: observability.latest_dag_run,
        live_nodes: observability.live_nodes,
        nodes: observability.nodes,
        artifacts: observability.artifacts,
        determinism,
        policies,
        observability_summary: AppRunObservabilitySummary {
            event_count,
            log_exists,
            log_bytes,
        },
        observability: observability_links,
        recent_events: events.clone(),
        events,
    }))
}

/// Build an app-neutral determinism/replay summary from persisted runtime rows.
pub fn app_run_determinism_summary(
    app_output: &serde_json::Value,
    latest_dag_run: Option<&AppRunDagSummary>,
    nodes: &[AppRunNodeSummary],
    artifacts: &[AppRunArtifactSummary],
) -> AppRunDeterminismSummary {
    let manifest_hash = latest_dag_run.and_then(|dag| dag.manifest_hash.clone());
    let frozen_input_hash = latest_dag_run.map(|dag| json_sha256(&dag.input));
    let dag_output_hash = latest_dag_run.map(|dag| json_sha256(&dag.output));
    let checkpoint_available = app_output
        .get("report")
        .is_some_and(|report| report.is_object());
    let node_attempts = nodes.len();
    let node_input_hashes = nodes
        .iter()
        .filter(|node| json_string_present(&node.input, "input_hash"))
        .count();
    let node_output_hashes = nodes
        .iter()
        .filter(|node| json_string_present(&node.output, "output_hash"))
        .count();
    let artifacts_with_sha256 = artifacts
        .iter()
        .filter(|artifact| {
            artifact
                .sha256
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        })
        .count();
    let artifacts_missing_sha256 = artifacts.len().saturating_sub(artifacts_with_sha256);
    let node_hashes_complete =
        node_attempts == node_input_hashes && node_attempts == node_output_hashes;
    let replay_ready = latest_dag_run.is_some()
        && manifest_hash.is_some()
        && frozen_input_hash.is_some()
        && checkpoint_available
        && node_hashes_complete;
    let compare_ready = frozen_input_hash.is_some()
        && dag_output_hash.is_some()
        && node_output_hashes == node_attempts
        && artifacts_missing_sha256 == 0;

    AppRunDeterminismSummary {
        manifest_hash,
        frozen_input_hash,
        dag_output_hash,
        checkpoint_available,
        node_attempts,
        node_input_hashes,
        node_output_hashes,
        artifacts: artifacts.len(),
        artifacts_with_sha256,
        artifacts_missing_sha256,
        replay_ready,
        compare_ready,
    }
}

/// Compare two persisted app runs using only app-neutral runtime data.
pub fn compare_app_run_details(left: &AppRunDetail, right: &AppRunDetail) -> AppRunComparison {
    let left_side = app_run_comparison_side(left);
    let right_side = app_run_comparison_side(right);
    let left_dag_type = left
        .latest_dag_run
        .as_ref()
        .map(|dag| dag.dag_type.as_str());
    let right_dag_type = right
        .latest_dag_run
        .as_ref()
        .map(|dag| dag.dag_type.as_str());
    let left_node_outputs = node_output_signatures(&left.nodes);
    let right_node_outputs = node_output_signatures(&right.nodes);
    let left_artifacts = artifact_signatures(&left.artifacts);
    let right_artifacts = artifact_signatures(&right.artifacts);
    let left_normalized_frozen_input_hash = normalized_frozen_input_hash(left);
    let right_normalized_frozen_input_hash = normalized_frozen_input_hash(right);
    let left_normalized_dag_output_hash = normalized_dag_output_hash(left);
    let right_normalized_dag_output_hash = normalized_dag_output_hash(right);
    let left_normalized_node_outputs = normalized_node_output_signatures(&left.nodes);
    let right_normalized_node_outputs = normalized_node_output_signatures(&right.nodes);
    let checks = AppRunComparisonChecks {
        same_app: left.run.app_id == right.run.app_id,
        same_action: left.run.action_id == right.run.action_id,
        same_dag_type: left_dag_type == right_dag_type,
        same_manifest_hash: left.determinism.manifest_hash == right.determinism.manifest_hash,
        same_frozen_input_hash: left.determinism.frozen_input_hash
            == right.determinism.frozen_input_hash,
        same_dag_output_hash: left.determinism.dag_output_hash == right.determinism.dag_output_hash,
        same_node_outputs: left_node_outputs == right_node_outputs,
        same_artifacts: left_artifacts == right_artifacts,
        same_normalized_frozen_input_hash: left_normalized_frozen_input_hash
            == right_normalized_frozen_input_hash,
        same_normalized_dag_output_hash: left_normalized_dag_output_hash
            == right_normalized_dag_output_hash,
        same_normalized_node_outputs: left_normalized_node_outputs == right_normalized_node_outputs,
    };
    let compare_ready = left.determinism.compare_ready && right.determinism.compare_ready;
    let mut differences = Vec::new();
    let mut work_product_differences = Vec::new();
    push_comparison_difference(
        &mut differences,
        "app_id",
        json!(&left.run.app_id),
        json!(&right.run.app_id),
        checks.same_app,
    );
    push_comparison_difference(
        &mut differences,
        "action_id",
        json!(&left.run.action_id),
        json!(&right.run.action_id),
        checks.same_action,
    );
    push_comparison_difference(
        &mut differences,
        "dag_type",
        json!(left_dag_type),
        json!(right_dag_type),
        checks.same_dag_type,
    );
    push_comparison_difference(
        &mut differences,
        "determinism.manifest_hash",
        json!(&left.determinism.manifest_hash),
        json!(&right.determinism.manifest_hash),
        checks.same_manifest_hash,
    );
    push_comparison_difference(
        &mut differences,
        "determinism.frozen_input_hash",
        json!(&left.determinism.frozen_input_hash),
        json!(&right.determinism.frozen_input_hash),
        checks.same_frozen_input_hash,
    );
    push_comparison_difference(
        &mut differences,
        "determinism.dag_output_hash",
        json!(&left.determinism.dag_output_hash),
        json!(&right.determinism.dag_output_hash),
        checks.same_dag_output_hash,
    );
    push_map_differences(
        &mut differences,
        "node_outputs",
        &left_node_outputs,
        &right_node_outputs,
    );
    push_map_differences(
        &mut differences,
        "artifacts",
        &left_artifacts,
        &right_artifacts,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "app_id",
        json!(&left.run.app_id),
        json!(&right.run.app_id),
        checks.same_app,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "action_id",
        json!(&left.run.action_id),
        json!(&right.run.action_id),
        checks.same_action,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "dag_type",
        json!(left_dag_type),
        json!(right_dag_type),
        checks.same_dag_type,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "determinism.manifest_hash",
        json!(&left.determinism.manifest_hash),
        json!(&right.determinism.manifest_hash),
        checks.same_manifest_hash,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "determinism.normalized_frozen_input_hash",
        json!(&left_normalized_frozen_input_hash),
        json!(&right_normalized_frozen_input_hash),
        checks.same_normalized_frozen_input_hash,
    );
    push_comparison_difference(
        &mut work_product_differences,
        "determinism.normalized_dag_output_hash",
        json!(&left_normalized_dag_output_hash),
        json!(&right_normalized_dag_output_hash),
        checks.same_normalized_dag_output_hash,
    );
    push_map_differences(
        &mut work_product_differences,
        "normalized_node_outputs",
        &left_normalized_node_outputs,
        &right_normalized_node_outputs,
    );
    push_map_differences(
        &mut work_product_differences,
        "artifacts",
        &left_artifacts,
        &right_artifacts,
    );
    let matches = compare_ready && checks.all();
    let work_product_matches = compare_ready && checks.work_product_all();
    AppRunComparison {
        left: left_side,
        right: right_side,
        compare_ready,
        matches,
        work_product_matches,
        checks,
        differences,
        work_product_differences,
    }
}

fn app_run_comparison_side(detail: &AppRunDetail) -> AppRunComparisonSide {
    AppRunComparisonSide {
        run_id: detail.run.id,
        app_id: detail.run.app_id.clone(),
        action_id: detail.run.action_id.clone(),
        state: detail.run.state.clone(),
        dag_run_id: detail.latest_dag_run.as_ref().map(|dag| dag.id),
        dag_type: detail
            .latest_dag_run
            .as_ref()
            .map(|dag| dag.dag_type.clone()),
        determinism: detail.determinism.clone(),
        normalized_frozen_input_hash: normalized_frozen_input_hash(detail),
        normalized_dag_output_hash: normalized_dag_output_hash(detail),
    }
}

fn node_output_signatures(nodes: &[AppRunNodeSummary]) -> BTreeMap<String, Option<String>> {
    nodes
        .iter()
        .map(|node| {
            (
                format!("{}#{}", node.node_id, node.attempt),
                node.output
                    .get("output_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
            )
        })
        .collect()
}

fn artifact_signatures(artifacts: &[AppRunArtifactSummary]) -> BTreeMap<String, Option<String>> {
    artifacts
        .iter()
        .map(|artifact| {
            (
                format!(
                    "{}:{}#{}",
                    artifact.node_id.as_deref().unwrap_or("-"),
                    artifact.name,
                    artifact.attempt.unwrap_or(0)
                ),
                artifact.sha256.clone(),
            )
        })
        .collect()
}

fn normalized_frozen_input_hash(detail: &AppRunDetail) -> Option<String> {
    detail
        .latest_dag_run
        .as_ref()
        .map(|dag| json_sha256(&normalize_dag_io_for_compare(&dag.input)))
}

fn normalized_dag_output_hash(detail: &AppRunDetail) -> Option<String> {
    detail
        .latest_dag_run
        .as_ref()
        .map(|dag| json_sha256(&normalize_dag_io_for_compare(&dag.output)))
}

fn normalized_node_output_signatures(
    nodes: &[AppRunNodeSummary],
) -> BTreeMap<String, Option<String>> {
    nodes
        .iter()
        .map(|node| {
            let hash_payload = json!({
                "state": node.state,
                "exit_status": node.exit_status,
                "outputs": node.output.get("outputs").cloned().unwrap_or(serde_json::Value::Null),
                "output_artifact_integrity": node
                    .output
                    .get("output_artifact_integrity")
                    .map(normalize_artifacts_for_compare)
                    .unwrap_or(serde_json::Value::Null),
                "warning": node.output.get("warning").cloned().unwrap_or(serde_json::Value::Null),
            });
            (
                format!("{}#{}", node.node_id, node.attempt),
                Some(json_sha256(&hash_payload)),
            )
        })
        .collect()
}

fn normalize_dag_io_for_compare(value: &serde_json::Value) -> serde_json::Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut normalized = serde_json::Map::new();
    for (key, child) in object {
        match key.as_str() {
            "values" => {
                normalized.insert(key.clone(), normalize_dag_values_for_compare(child));
            }
            "artifacts" => {
                normalized.insert(key.clone(), normalize_artifacts_for_compare(child));
            }
            _ => {
                normalized.insert(key.clone(), child.clone());
            }
        }
    }
    serde_json::Value::Object(normalized)
}

fn normalize_dag_values_for_compare(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .filter(|(key, _)| !is_runtime_identity_value_key(key))
                .map(|(key, value)| (key.clone(), normalize_dag_values_for_compare(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(normalize_dag_values_for_compare)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn normalize_artifacts_for_compare(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), normalize_artifact_ref_for_compare(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn normalize_artifact_ref_for_compare(value: &serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(object) = value else {
        return value.clone();
    };
    serde_json::Value::Object(
        object
            .iter()
            .filter(|(key, _)| key.as_str() != "uri")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn is_runtime_identity_value_key(key: &str) -> bool {
    matches!(
        key,
        "adapter_idempotency_key"
            | "app_run_id"
            | "app_run_log_path"
            | "dag_run_id"
            | "lease_id"
            | "loop_continue"
    )
}

fn push_comparison_difference(
    differences: &mut Vec<AppRunComparisonDifference>,
    field: &str,
    left: serde_json::Value,
    right: serde_json::Value,
    same: bool,
) {
    if !same {
        differences.push(AppRunComparisonDifference {
            field: field.to_string(),
            left,
            right,
        });
    }
}

fn push_map_differences(
    differences: &mut Vec<AppRunComparisonDifference>,
    prefix: &str,
    left: &BTreeMap<String, Option<String>>,
    right: &BTreeMap<String, Option<String>>,
) {
    let keys = left
        .keys()
        .chain(right.keys())
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for key in keys {
        let left_value = left.get(key).cloned().flatten();
        let right_value = right.get(key).cloned().flatten();
        if left_value != right_value {
            differences.push(AppRunComparisonDifference {
                field: format!("{prefix}.{key}"),
                left: json!(left_value),
                right: json!(right_value),
            });
        }
    }
}

fn json_string_present(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

/// Build an app-neutral policy/isolation summary from persisted node rows.
pub fn app_run_policy_summary(nodes: &[AppRunNodeSummary]) -> AppRunPolicySummary {
    let mut summary = AppRunPolicySummary {
        node_attempts: nodes.len(),
        ..AppRunPolicySummary::default()
    };
    for node in nodes {
        let input_policy = node.input.get("policy").unwrap_or(&serde_json::Value::Null);
        if value_has_policy(&node.policy) || value_has_policy(input_policy) {
            summary.nodes_with_policy += 1;
        }
        if node_has_timeout_policy(node) {
            summary.timeout_limited_nodes += 1;
        }
        let tool_policy = node.policy.get("tool").unwrap_or(&serde_json::Value::Null);
        if let Some(budget_units) = json_u64(tool_policy.get("budget_units")) {
            summary.budget_limited_nodes += 1;
            summary.budget_units_requested =
                summary.budget_units_requested.saturating_add(budget_units);
        }
        if node
            .policy
            .get("approval")
            .is_some_and(|value| value.is_object())
        {
            summary.approval_gates += 1;
        }
        if tool_policy
            .get("approval_required")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            summary.approval_required_tools += 1;
        }
        let network_denied = tool_policy
            .get("network")
            .and_then(|network| network.get("allow"))
            .and_then(serde_json::Value::as_bool)
            == Some(false);
        if network_denied {
            summary.network_denied_nodes += 1;
        }
        let filesystem_restricted = filesystem_policy_restricted(tool_policy.get("filesystem"));
        if filesystem_restricted {
            summary.filesystem_restricted_nodes += 1;
        }
        if network_denied || filesystem_restricted {
            summary.isolation_required_nodes += 1;
        }
        if node
            .policy
            .get("retry")
            .is_some_and(|value| value.is_object())
        {
            summary.retry_policies += 1;
        }
        if node_policy_denied(node) {
            summary.policy_denied_nodes += 1;
        }
    }
    summary
}

fn value_has_policy(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
}

fn node_has_timeout_policy(node: &AppRunNodeSummary) -> bool {
    json_u64(node.policy.get("timeout_secs")).is_some()
        || json_u64(
            node.policy
                .get("tool")
                .and_then(|tool| tool.get("timeout_secs")),
        )
        .is_some()
        || json_u64(
            node.input
                .get("policy")
                .and_then(|policy| policy.get("timeout_secs")),
        )
        .is_some()
}

fn json_u64(value: Option<&serde_json::Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
}

fn filesystem_policy_restricted(value: Option<&serde_json::Value>) -> bool {
    let Some(filesystem) = value else {
        return false;
    };
    ["read", "write"].iter().any(|key| {
        filesystem
            .get(*key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| !entries.is_empty())
    })
}

fn node_policy_denied(node: &AppRunNodeSummary) -> bool {
    node.exit_status.is_none()
        && node.error_message.as_deref().is_some_and(|message| {
            let lower = message.to_ascii_lowercase();
            lower.contains("policy") || lower.contains("requires isolated runner")
        })
}

/// Fetch latest DAG/node/artifact observability for one app run.
pub async fn load_observability(
    pool: &PgPool,
    run_id: Uuid,
) -> anyhow::Result<AppRunObservability> {
    let live_nodes = load_live_node_summaries(pool, run_id).await?;
    let latest_dag_run = latest_dag_run(pool, run_id).await?;
    let Some(dag_run) = latest_dag_run.clone() else {
        return Ok(AppRunObservability {
            live_nodes,
            ..AppRunObservability::default()
        });
    };
    let nodes = list_dag_run_nodes(pool, dag_run.id).await?;
    let artifacts = list_dag_run_artifacts(pool, dag_run.id).await?;
    Ok(AppRunObservability {
        latest_dag_run: Some(dag_run),
        live_nodes,
        nodes,
        artifacts,
    })
}

/// List app-run events.
pub async fn list_events(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Vec<AppRunEvent>> {
    list_events_limited(pool, run_id, APP_RUN_EVENT_DETAIL_LIMIT).await
}

/// Count all durable events for one app run.
pub async fn count_events(pool: &PgPool, run_id: Uuid) -> anyhow::Result<usize> {
    let count =
        sqlx::query_scalar::<_, i64>("select count(*) from dag_events where app_run_id = $1")
            .bind(run_id)
            .fetch_one(pool)
            .await?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// List a bounded recent tail of app-run events in chronological order.
pub async fn list_events_limited(
    pool: &PgPool,
    run_id: Uuid,
    limit: usize,
) -> anyhow::Result<Vec<AppRunEvent>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from ( \
           select id, level, event_type, message, payload, created_at \
           from dag_events \
           where app_run_id = $1 \
           order by created_at desc, id desc \
           limit $2 \
         ) recent \
         order by created_at asc, id asc",
    )
    .bind(run_id)
    .bind(
        i64::try_from(limit)
            .unwrap_or(APP_RUN_EVENT_DETAIL_LIMIT as i64)
            .clamp(1, 5_000),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunEvent {
            id: row.get("id"),
            level: row.get("level"),
            event_type: row.get("event_type"),
            message: row.get("message"),
            payload: row.get("payload"),
            created_at: row.get("created_at"),
        })
        .collect())
}

/// List app-run events after a durable event id in chronological order.
pub async fn list_events_after(
    pool: &PgPool,
    run_id: Uuid,
    after_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<AppRunEvent>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from dag_events \
         where app_run_id = $1 and id > $2 \
         order by id asc \
         limit $3",
    )
    .bind(run_id)
    .bind(after_id)
    .bind(
        i64::try_from(limit)
            .unwrap_or(APP_RUN_EVENT_DETAIL_LIMIT as i64)
            .clamp(1, 5_000),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunEvent {
            id: row.get("id"),
            level: row.get("level"),
            event_type: row.get("event_type"),
            message: row.get("message"),
            payload: row.get("payload"),
            created_at: row.get("created_at"),
        })
        .collect())
}

async fn latest_dag_run(pool: &PgPool, run_id: Uuid) -> anyhow::Result<Option<AppRunDagSummary>> {
    let row = sqlx::query(
        "select id, dag_type, manifest_version, manifest_hash, state, input, output, \
                started_at, finished_at, created_at \
         from dag_runs \
         where app_run_id = $1 \
         order by created_at desc, id desc \
         limit 1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| AppRunDagSummary {
        id: row.get("id"),
        dag_type: row.get("dag_type"),
        manifest_version: row.get("manifest_version"),
        manifest_hash: row.get("manifest_hash"),
        state: row.get("state"),
        input: row.get("input"),
        output: row.get("output"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        created_at: row.get("created_at"),
    }))
}

async fn list_dag_run_nodes(
    pool: &PgPool,
    dag_run_id: Uuid,
) -> anyhow::Result<Vec<AppRunNodeSummary>> {
    let rows = sqlx::query(
        "select id, node_id, node_kind, state, attempt, runner, model, prompt_hash, \
                command, exit_status, role, tool, child_dag_type, required, \
                input_refs, output_refs, diagnostic_refs, policy, input, output, error_message, \
                latency_ms, started_at, finished_at, created_at \
         from dag_run_nodes \
         where dag_run_id = $1 \
         order by created_at asc, node_id asc, attempt asc",
    )
    .bind(dag_run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunNodeSummary {
            id: row.get("id"),
            node_id: row.get("node_id"),
            node_kind: row.get("node_kind"),
            state: row.get("state"),
            attempt: row.get("attempt"),
            runner: row.get("runner"),
            model: row.get("model"),
            prompt_hash: row.get("prompt_hash"),
            command: row.get("command"),
            exit_status: row.get("exit_status"),
            role: row.get("role"),
            tool: row.get("tool"),
            child_dag_type: row.get("child_dag_type"),
            required: row.get("required"),
            input_refs: row.get("input_refs"),
            output_refs: row.get("output_refs"),
            diagnostic_refs: row.get("diagnostic_refs"),
            policy: row.get("policy"),
            input: row.get("input"),
            output: row.get("output"),
            error_message: row.get("error_message"),
            latency_ms: row.get("latency_ms"),
            started_at: row.get("started_at"),
            finished_at: row.get("finished_at"),
            created_at: row.get("created_at"),
        })
        .collect())
}

async fn list_dag_run_artifacts(
    pool: &PgPool,
    dag_run_id: Uuid,
) -> anyhow::Result<Vec<AppRunArtifactSummary>> {
    let rows = sqlx::query(
        "select da.id, da.name, da.uri, da.media_type, da.sha256, da.size_bytes, \
                da.schema_ref, da.metadata, drn.node_id, drn.attempt, da.created_at \
         from dag_artifacts da \
         left join dag_run_nodes drn on drn.id = da.node_run_id \
         where da.dag_run_id = $1 \
         order by da.created_at asc, da.name asc",
    )
    .bind(dag_run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunArtifactSummary {
            id: row.get("id"),
            name: row.get("name"),
            uri: row.get("uri"),
            media_type: row.get("media_type"),
            sha256: row.get("sha256"),
            size_bytes: row.get("size_bytes"),
            schema_ref: row.get("schema_ref"),
            metadata: row.get("metadata"),
            node_id: row.get("node_id"),
            attempt: row.get("attempt"),
            created_at: row.get("created_at"),
        })
        .collect())
}

async fn load_live_node_summaries(
    pool: &PgPool,
    run_id: Uuid,
) -> anyhow::Result<Vec<AppRunLiveNodeSummary>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from ( \
           select distinct on (coalesce(payload->>'node_id', 'app_action:' || coalesce(payload->>'action', 'unknown'))) \
                  id, level, event_type, message, payload, created_at \
           from dag_events \
           where app_run_id = $1 \
             and ( \
               ( \
                 event_type in ( \
                   'node.queued', 'node.started', 'node.retry_scheduled', \
                   'node.awaiting_approval', 'node.completed', 'node.failed', 'node.skipped', \
                   'node.cancelled' \
                 ) \
                 and payload->>'node_id' is not null \
               ) \
               or event_type in ( \
                 'app_action.started', 'app_action.awaiting_approval', \
                 'app_action.completed', 'app_action.failed', 'app_action.cancelled' \
               ) \
             ) \
           order by coalesce(payload->>'node_id', 'app_action:' || coalesce(payload->>'action', 'unknown')), \
                    created_at desc, id desc \
         ) latest \
         order by created_at asc, id asc \
         limit $2",
    )
    .bind(run_id)
    .bind(i64::try_from(LIVE_NODE_SUMMARY_LIMIT).unwrap_or(500))
    .fetch_all(pool)
    .await?;
    let events = rows
        .into_iter()
        .map(|row| AppRunEvent {
            id: row.get("id"),
            level: row.get("level"),
            event_type: row.get("event_type"),
            message: row.get("message"),
            payload: row.get("payload"),
            created_at: row.get("created_at"),
        })
        .collect::<Vec<_>>();
    Ok(summarize_live_node_events(&events))
}

fn summarize_live_node_events(events: &[AppRunEvent]) -> Vec<AppRunLiveNodeSummary> {
    let mut by_node: HashMap<String, AppRunLiveNodeSummary> = HashMap::new();
    for event in events {
        let Some(summary) = live_node_summary_from_event(event) else {
            continue;
        };
        let replace = by_node
            .get(&summary.node_id)
            .map(|existing| {
                summary.updated_at > existing.updated_at
                    || (summary.updated_at == existing.updated_at
                        && summary.event_id > existing.event_id)
            })
            .unwrap_or(true);
        if replace {
            by_node.insert(summary.node_id.clone(), summary);
        }
    }
    let mut summaries = by_node.into_values().collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.event_id.cmp(&right.event_id))
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
    summaries
}

fn live_node_summary_from_event(event: &AppRunEvent) -> Option<AppRunLiveNodeSummary> {
    let is_node_event = LIVE_NODE_EVENT_TYPES.contains(&event.event_type.as_str());
    let is_action_event = LIVE_ACTION_EVENT_TYPES.contains(&event.event_type.as_str());
    if !is_node_event && !is_action_event {
        return None;
    }
    let node_id = if is_action_event {
        format!(
            "app_action:{}",
            payload_string(&event.payload, "action").unwrap_or_else(|| "unknown".to_string())
        )
    } else {
        payload_string(&event.payload, "node_id")?
    };
    let state = live_node_state(&event.event_type, &event.payload);
    let attempt = payload_i32(&event.payload, "attempt").unwrap_or(1);
    Some(AppRunLiveNodeSummary {
        node_id,
        state,
        event_type: event.event_type.clone(),
        level: event.level.clone(),
        attempt,
        node_kind: if is_action_event {
            Some("app_action".to_string())
        } else {
            payload_string(&event.payload, "node_kind")
                .or_else(|| payload_string(&event.payload, "kind"))
        },
        status: payload_string(&event.payload, "status"),
        message: event.message.clone(),
        payload: event.payload.clone(),
        event_id: event.id,
        updated_at: event.created_at,
    })
}

fn live_node_state(event_type: &str, payload: &serde_json::Value) -> String {
    match event_type {
        "node.queued" => "queued".to_string(),
        "node.started" => "running".to_string(),
        "node.retry_scheduled" => "retry_scheduled".to_string(),
        "node.awaiting_approval" => "awaiting_approval".to_string(),
        "node.completed" => {
            payload_string(payload, "status").unwrap_or_else(|| "completed".to_string())
        }
        "node.failed" => "failed".to_string(),
        "node.skipped" => "skipped".to_string(),
        "node.cancelled" => "cancelled".to_string(),
        "app_action.started" => "running".to_string(),
        "app_action.awaiting_approval" => "awaiting_approval".to_string(),
        "app_action.completed" => {
            payload_string(payload, "status").unwrap_or_else(|| "completed".to_string())
        }
        "app_action.failed" => "failed".to_string(),
        "app_action.cancelled" => "cancelled".to_string(),
        _ => "unknown".to_string(),
    }
}

fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

fn payload_i32(payload: &serde_json::Value, key: &str) -> Option<i32> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

#[derive(Debug)]
struct AdapterNodeEventRecord {
    node_id: String,
    node_kind: String,
    state: String,
    attempt: i32,
    runner: Option<String>,
    model: Option<String>,
    prompt_hash: Option<String>,
    command: serde_json::Value,
    exit_status: Option<i32>,
    role: Option<String>,
    tool: Option<String>,
    input_refs: serde_json::Value,
    output_refs: serde_json::Value,
    diagnostic_refs: serde_json::Value,
    input: serde_json::Value,
    output: serde_json::Value,
    error_message: Option<String>,
    latency_ms: Option<i32>,
}

fn adapter_node_event_record(
    event_type: &str,
    message: Option<&str>,
    payload: &serde_json::Value,
) -> Option<AdapterNodeEventRecord> {
    if !LIVE_NODE_EVENT_TYPES.contains(&event_type) {
        return None;
    }
    let node_id = payload_string(payload, "node_id")?;
    let node_kind = payload_string(payload, "node_kind")
        .or_else(|| payload_string(payload, "kind"))
        .unwrap_or_else(|| "unknown".to_string());
    let tool = payload_string(payload, "tool_id").or_else(|| payload_string(payload, "tool"));
    let role = payload_string(payload, "role")
        .or_else(|| (node_kind == "llm").then(|| tool.clone()).flatten());
    let input_refs = payload
        .get("input_refs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let output_refs = payload
        .get("output_refs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let diagnostic_refs = payload
        .get("diagnostic_refs")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let error_message = payload_string(payload, "error").or_else(|| {
        adapter_node_event_state(event_type, payload)
            .starts_with("failed")
            .then(|| message.map(ToOwned::to_owned))
            .flatten()
    });
    Some(AdapterNodeEventRecord {
        node_id,
        node_kind,
        state: adapter_node_event_state(event_type, payload),
        attempt: payload_i32(payload, "attempt").unwrap_or(1),
        runner: payload_string(payload, "runner").or_else(|| payload_string(payload, "provider")),
        model: payload_string(payload, "model"),
        prompt_hash: payload_string(payload, "prompt_hash"),
        command: payload.get("command").cloned().unwrap_or_else(|| json!([])),
        exit_status: payload_i32(payload, "exit_status"),
        role,
        tool,
        input_refs: input_refs.clone(),
        output_refs: output_refs.clone(),
        diagnostic_refs,
        input: json!({ "input_refs": input_refs }),
        output: payload.clone(),
        error_message,
        latency_ms: payload_i32(payload, "duration_ms")
            .or_else(|| payload_i32(payload, "latency_ms")),
    })
}

fn adapter_node_event_state(event_type: &str, payload: &serde_json::Value) -> String {
    match event_type {
        "node.queued" | "node.retry_scheduled" => "queued".to_string(),
        "node.started" => "running".to_string(),
        "node.awaiting_approval" => "awaiting_approval".to_string(),
        "node.skipped" => "skipped".to_string(),
        "node.cancelled" => "cancelled".to_string(),
        "node.failed" => "failed".to_string(),
        "node.completed" => match payload_string(payload, "status")
            .unwrap_or_else(|| "ok".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "ok" | "pass" | "passed" | "success" | "completed" | "done" => "ok".to_string(),
            "partial" | "degraded" | "fallback_ok" => "degraded".to_string(),
            "skipped" => "skipped".to_string(),
            "cancelled" => "cancelled".to_string(),
            "fail" | "failed" | "error" | "system_failed" => "failed".to_string(),
            _ => "ok".to_string(),
        },
        _ => "queued".to_string(),
    }
}

fn adapter_node_event_is_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "ok" | "degraded" | "skipped" | "failed" | "cancelled" | "system_failed"
    )
}

async fn upsert_dag_run_node_from_event(
    pool: &PgPool,
    dag_run_id: Uuid,
    event_type: &str,
    message: Option<&str>,
    payload: &serde_json::Value,
) -> anyhow::Result<Option<Uuid>> {
    let Some(record) = adapter_node_event_record(event_type, message, payload) else {
        return Ok(None);
    };
    let terminal = adapter_node_event_is_terminal_state(&record.state);
    let node_run_id = sqlx::query_scalar::<_, Uuid>(
        "insert into dag_run_nodes \
         (dag_run_id, node_id, node_kind, state, attempt, runner, model, prompt_hash, command, \
          exit_status, input_refs, output_refs, diagnostic_refs, role, tool, required, input, \
          output, error_message, latency_ms, started_at, finished_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, true, \
                 $16, $17, $18, $19, \
                 case when $4 in ('running','awaiting_approval','ok','degraded','skipped','failed','cancelled','system_failed') then now() else null end, \
                 case when $20 then now() else null end) \
         on conflict (dag_run_id, node_id, attempt) do update set \
           node_kind = excluded.node_kind, \
           state = excluded.state, \
           runner = coalesce(excluded.runner, dag_run_nodes.runner), \
           model = coalesce(excluded.model, dag_run_nodes.model), \
           prompt_hash = coalesce(excluded.prompt_hash, dag_run_nodes.prompt_hash), \
           command = case when excluded.command <> '[]'::jsonb then excluded.command else dag_run_nodes.command end, \
           exit_status = coalesce(excluded.exit_status, dag_run_nodes.exit_status), \
           input_refs = case when excluded.input_refs <> '{}'::jsonb then excluded.input_refs else dag_run_nodes.input_refs end, \
           output_refs = case when excluded.output_refs <> '{}'::jsonb then excluded.output_refs else dag_run_nodes.output_refs end, \
           diagnostic_refs = case when excluded.diagnostic_refs <> '{}'::jsonb then excluded.diagnostic_refs else dag_run_nodes.diagnostic_refs end, \
           role = coalesce(excluded.role, dag_run_nodes.role), \
           tool = coalesce(excluded.tool, dag_run_nodes.tool), \
           input = case when excluded.input <> '{\"input_refs\":{}}'::jsonb then excluded.input else dag_run_nodes.input end, \
           output = excluded.output, \
           error_message = coalesce(excluded.error_message, dag_run_nodes.error_message), \
           latency_ms = coalesce(excluded.latency_ms, dag_run_nodes.latency_ms), \
           started_at = coalesce(dag_run_nodes.started_at, excluded.started_at), \
           finished_at = coalesce(excluded.finished_at, dag_run_nodes.finished_at), \
           updated_at = now() \
         returning id",
    )
    .bind(dag_run_id)
    .bind(&record.node_id)
    .bind(&record.node_kind)
    .bind(&record.state)
    .bind(record.attempt)
    .bind(record.runner.as_deref())
    .bind(record.model.as_deref())
    .bind(record.prompt_hash.as_deref())
    .bind(&record.command)
    .bind(record.exit_status)
    .bind(&record.input_refs)
    .bind(&record.output_refs)
    .bind(&record.diagnostic_refs)
    .bind(record.role.as_deref())
    .bind(record.tool.as_deref())
    .bind(&record.input)
    .bind(&record.output)
    .bind(record.error_message.as_deref())
    .bind(record.latency_ms)
    .bind(terminal)
    .fetch_one(pool)
    .await?;
    Ok(Some(node_run_id))
}

/// Insert an app-run event.
pub async fn insert_event(
    pool: &PgPool,
    run_id: Uuid,
    level: &str,
    event_type: &str,
    message: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into dag_events (app_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(durable_app_event_payload(run_id, payload))
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert an app-run event already associated with a live DAG run.
pub async fn insert_dag_event(
    pool: &PgPool,
    run_id: Uuid,
    dag_run_id: Uuid,
    level: &str,
    event_type: &str,
    message: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    let payload = durable_dag_event_payload(run_id, dag_run_id, payload);
    let node_run_id =
        upsert_dag_run_node_from_event(pool, dag_run_id, event_type, message, &payload).await?;
    sqlx::query(
        "insert into dag_events (app_run_id, dag_run_id, node_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(run_id)
    .bind(dag_run_id)
    .bind(node_run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    level: &str,
    event_type: &str,
    message: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into dag_events (app_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(durable_app_event_payload(run_id, payload))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_dag_event_tx(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    dag_run_id: Uuid,
    level: &str,
    event_type: &str,
    message: Option<&str>,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into dag_events (app_run_id, dag_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(run_id)
    .bind(dag_run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(durable_dag_event_payload(run_id, dag_run_id, payload))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn durable_app_event_payload(run_id: Uuid, payload: serde_json::Value) -> serde_json::Value {
    agenthero_agent_runtime::agenthero_trace_payload(run_id, None, payload)
}

fn durable_dag_event_payload(
    run_id: Uuid,
    dag_run_id: Uuid,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut payload = durable_app_event_payload(run_id, payload);
    if let Some(object) = payload.as_object_mut() {
        object.insert("dag_run_id".to_string(), json!(dag_run_id.to_string()));
    }
    payload
}

fn approval_requeued_event_payload(run_id: Uuid, approved_key: &str) -> serde_json::Value {
    durable_app_event_payload(
        run_id,
        json!({
            "approved_key": approved_key,
            "state": "queued",
        }),
    )
}

async fn persist_dag_report(
    tx: &mut Transaction<'_, Postgres>,
    app_run_id: Uuid,
    report: &DagExecutionReport,
) -> anyhow::Result<()> {
    let dag_state = report_state(report);
    let preassigned_dag_run_id = preassigned_dag_run_id(report);
    let dag_run_id = sqlx::query_scalar::<_, Uuid>(
        "insert into dag_runs \
         (id, app_run_id, dag_type, manifest_version, manifest_hash, state, input, output, started_at, finished_at) \
         values (coalesce($1::uuid, gen_random_uuid()), $2, $3, $4, $5, $6, $7, $8, now(), \
                 case when $6 = 'awaiting_approval' then null else now() end) \
         on conflict (id) do update set \
           app_run_id = excluded.app_run_id, \
           dag_type = excluded.dag_type, \
           manifest_version = excluded.manifest_version, \
           manifest_hash = excluded.manifest_hash, \
           state = excluded.state, \
           input = excluded.input, \
           output = excluded.output, \
           started_at = coalesce(dag_runs.started_at, excluded.started_at), \
           finished_at = excluded.finished_at, \
           updated_at = now() \
         returning id",
    )
    .bind(preassigned_dag_run_id)
    .bind(app_run_id)
    .bind(report.dag_type.as_str())
    .bind(i32::try_from(report.manifest_version).unwrap_or(i32::MAX))
    .bind(&report.manifest_hash)
    .bind(dag_state)
    .bind(serde_json::to_value(&report.input)?)
    .bind(serde_json::to_value(&report.outputs)?)
    .fetch_one(&mut **tx)
    .await?;

    let lease_id = preassigned_lease_id(report);
    let mut node_run_ids = HashMap::new();
    for node in &report.nodes {
        let node_run_id = sqlx::query_scalar::<_, Uuid>(
            "insert into dag_run_nodes \
             (dag_run_id, node_id, node_kind, state, attempt, runner, model, prompt_hash, command, \
              exit_status, policy, input_refs, output_refs, diagnostic_refs, role, tool, child_dag_type, required, \
              input, output, error_message, latency_ms, started_at, finished_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                     $17, $18, $19, $20, $21, $22, now(), \
                     case when $4 = 'awaiting_approval' then null else now() end) \
             on conflict (dag_run_id, node_id, attempt) do update set \
               node_kind = excluded.node_kind, \
               state = excluded.state, \
               runner = coalesce(excluded.runner, dag_run_nodes.runner), \
               model = coalesce(excluded.model, dag_run_nodes.model), \
               prompt_hash = coalesce(excluded.prompt_hash, dag_run_nodes.prompt_hash), \
               command = case when excluded.command <> '[]'::jsonb then excluded.command else dag_run_nodes.command end, \
               exit_status = coalesce(excluded.exit_status, dag_run_nodes.exit_status), \
               policy = case when excluded.policy <> '{}'::jsonb then excluded.policy else dag_run_nodes.policy end, \
               input_refs = case when excluded.input_refs <> '{}'::jsonb then excluded.input_refs else dag_run_nodes.input_refs end, \
               output_refs = case when excluded.output_refs <> '{}'::jsonb then excluded.output_refs else dag_run_nodes.output_refs end, \
               diagnostic_refs = case when excluded.diagnostic_refs <> '{}'::jsonb then excluded.diagnostic_refs else dag_run_nodes.diagnostic_refs end, \
               role = coalesce(excluded.role, dag_run_nodes.role), \
               tool = coalesce(excluded.tool, dag_run_nodes.tool), \
               child_dag_type = coalesce(excluded.child_dag_type, dag_run_nodes.child_dag_type), \
               required = excluded.required, \
               input = excluded.input, \
               output = excluded.output, \
               error_message = coalesce(excluded.error_message, dag_run_nodes.error_message), \
               latency_ms = coalesce(excluded.latency_ms, dag_run_nodes.latency_ms), \
               started_at = coalesce(dag_run_nodes.started_at, excluded.started_at), \
               finished_at = coalesce(excluded.finished_at, dag_run_nodes.finished_at), \
               updated_at = now() \
             returning id",
        )
        .bind(dag_run_id)
        .bind(&node.node_id)
        .bind(&node.kind)
        .bind(node_state(node.status))
        .bind(i32::try_from(node.attempt).unwrap_or(i32::MAX))
        .bind(node.executor.as_deref())
        .bind(node.model.as_deref())
        .bind(node.prompt_hash.as_deref())
        .bind(serde_json::to_value(&node.command)?)
        .bind(node.exit_status)
        .bind(serde_json::to_value(&node.policy)?)
        .bind(serde_json::to_value(&node.input_refs)?)
        .bind(serde_json::to_value(&node.output_refs)?)
        .bind(serde_json::to_value(&node.diagnostic_refs)?)
        .bind(node.role.as_deref())
        .bind(node.tool.as_deref())
        .bind(node.child_dag_type.as_deref())
        .bind(node.required)
        .bind(node_input_json(
            &node.inputs,
            &node.input_refs,
            &node.policy,
            &node.trace,
        ))
        .bind(node_output_json(
            &serde_json::to_value(&node.outputs)?,
            &node.output_refs,
            &node.diagnostic_refs,
            &node.warning,
            &node.trace,
        ))
        .bind(node.error.as_deref())
        .bind(
            node.latency_ms
                .map(|value| i32::try_from(value).unwrap_or(i32::MAX)),
        )
        .fetch_one(&mut **tx)
        .await?;
        node_run_ids.insert(
            (
                node.node_id.clone(),
                i32::try_from(node.attempt).unwrap_or(i32::MAX),
            ),
            node_run_id,
        );
    }

    for (name, artifact) in &report.outputs.artifacts {
        let mut metadata = artifact.metadata.clone();
        metadata.insert("final_state".to_string(), json!(true));
        let integrity = artifact_integrity_for_uri(&artifact.uri);
        sqlx::query(
            "insert into dag_artifacts \
             (app_run_id, dag_run_id, name, uri, media_type, sha256, size_bytes, metadata) \
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(app_run_id)
        .bind(dag_run_id)
        .bind(name)
        .bind(&artifact.uri)
        .bind(artifact.media_type.as_deref())
        .bind(integrity.sha256.as_deref())
        .bind(integrity.size_bytes)
        .bind(serde_json::to_value(metadata)?)
        .execute(&mut **tx)
        .await?;
    }

    for node in &report.nodes {
        let node_run_id = node_run_ids
            .get(&(
                node.node_id.clone(),
                i32::try_from(node.attempt).unwrap_or(i32::MAX),
            ))
            .copied();
        for (name, uri) in &node.output_refs {
            let integrity = artifact_integrity_for_uri(uri);
            sqlx::query(
                "insert into dag_artifacts \
                 (app_run_id, dag_run_id, node_run_id, name, uri, sha256, size_bytes, metadata) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(app_run_id)
            .bind(dag_run_id)
            .bind(node_run_id)
            .bind(name)
            .bind(uri)
            .bind(integrity.sha256.as_deref())
            .bind(integrity.size_bytes)
            .bind(json!({
                "artifact_kind": "node_output",
                "node_id": node.node_id.clone(),
                "attempt": node.attempt,
            }))
            .execute(&mut **tx)
            .await?;
        }
        for (name, uri) in &node.diagnostic_refs {
            let integrity = artifact_integrity_for_uri(uri);
            sqlx::query(
                "insert into dag_artifacts \
                 (app_run_id, dag_run_id, node_run_id, name, uri, sha256, size_bytes, metadata) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(app_run_id)
            .bind(dag_run_id)
            .bind(node_run_id)
            .bind(name)
            .bind(uri)
            .bind(integrity.sha256.as_deref())
            .bind(integrity.size_bytes)
            .bind(json!({
                "artifact_kind": "node_diagnostic",
                "node_id": node.node_id.clone(),
                "attempt": node.attempt,
            }))
            .execute(&mut **tx)
            .await?;
        }
    }

    for event in &report.events {
        let node_run_id = event
            .node_id
            .as_ref()
            .and_then(|node_id| {
                let attempt = event
                    .payload
                    .get("attempt")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .unwrap_or(1);
                node_run_ids.get(&(node_id.clone(), attempt))
            })
            .copied();
        let event_payload = dag_execution_event_payload(app_run_id, dag_run_id, lease_id, event)?;
        if attach_existing_live_event(
            tx,
            app_run_id,
            dag_run_id,
            node_run_id,
            event,
            &event_payload,
        )
        .await?
        {
            continue;
        }
        sqlx::query(
            "insert into dag_events \
             (app_run_id, dag_run_id, node_run_id, level, event_type, message, payload) \
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(app_run_id)
        .bind(dag_run_id)
        .bind(node_run_id)
        .bind(&event.level)
        .bind(&event.event_type)
        .bind(event.message.as_deref())
        .bind(event_payload)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn preassigned_dag_run_id(report: &DagExecutionReport) -> Option<Uuid> {
    report
        .input
        .values
        .get("dag_run_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn preassigned_lease_id(report: &DagExecutionReport) -> Option<Uuid> {
    report
        .input
        .values
        .get("lease_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn runtime_event_payload(
    runtime: Option<AppRunRuntimeIdentity>,
    report: Option<&DagExecutionReport>,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut payload = report_runtime_payload(report, payload);
    let Some(runtime) = runtime else {
        return payload;
    };
    if !payload.is_object() {
        payload = json!({});
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "dag_run_id".to_string(),
            json!(runtime.dag_run_id.to_string()),
        );
        object.insert("lease_id".to_string(), json!(runtime.lease_id.to_string()));
    }
    payload
}

fn failed_adapter_dag_event_payload(
    run_id: Uuid,
    runtime: AppRunRuntimeIdentity,
    state: &str,
    code: &str,
    message: &str,
    retryable: bool,
) -> serde_json::Value {
    durable_dag_event_payload(
        run_id,
        runtime.dag_run_id,
        runtime_event_payload(
            Some(runtime),
            None,
            json!({
                "state": failed_app_terminal_dag_state(state),
                "status": failed_app_terminal_dag_state(state),
                "code": code,
                "message": message,
                "retryable": retryable,
            }),
        ),
    )
}

fn report_runtime_payload(
    report: Option<&DagExecutionReport>,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    let Some(report) = report else {
        return payload;
    };
    if !payload.is_object() {
        payload = json!({});
    }
    if let Some(object) = payload.as_object_mut() {
        if let Some(dag_run_id) = preassigned_dag_run_id(report) {
            object.insert("dag_run_id".to_string(), json!(dag_run_id.to_string()));
        }
        if let Some(lease_id) = preassigned_lease_id(report) {
            object.insert("lease_id".to_string(), json!(lease_id.to_string()));
        }
    }
    payload
}

fn dag_execution_event_payload(
    app_run_id: Uuid,
    dag_run_id: Uuid,
    lease_id: Option<Uuid>,
    event: &agenthero_dag_executor::DagExecutionEvent,
) -> anyhow::Result<serde_json::Value> {
    let mut payload = agenthero_agent_runtime::agenthero_event_payload(app_run_id, event);
    if let Some(object) = payload.as_object_mut() {
        object.insert("dag_run_id".to_string(), json!(dag_run_id.to_string()));
        if let Some(lease_id) = lease_id {
            object.insert("lease_id".to_string(), json!(lease_id.to_string()));
        }
    }
    Ok(payload)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveEventMatchKey {
    Node {
        node_id: String,
        attempt: i32,
    },
    Dag {
        dag_type: String,
        manifest_hash: String,
    },
}

fn live_event_match_key(
    event: &agenthero_dag_executor::DagExecutionEvent,
    payload: &serde_json::Value,
) -> Option<LiveEventMatchKey> {
    if let Some(node_id) = event
        .node_id
        .as_deref()
        .or_else(|| payload.get("node_id").and_then(serde_json::Value::as_str))
    {
        let attempt = payload
            .get("attempt")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(1);
        return Some(LiveEventMatchKey::Node {
            node_id: node_id.to_string(),
            attempt,
        });
    }

    if event.event_type.starts_with("dag.") {
        let dag_type = payload_string(payload, "dag_type")?;
        let manifest_hash = payload_string(payload, "manifest_hash")?;
        return Some(LiveEventMatchKey::Dag {
            dag_type,
            manifest_hash,
        });
    }

    None
}

fn node_input_json(
    inputs: &[String],
    input_refs: &std::collections::BTreeMap<String, String>,
    policy: &std::collections::BTreeMap<String, serde_json::Value>,
    trace: &std::collections::BTreeMap<String, serde_json::Value>,
) -> serde_json::Value {
    let hash_payload = json!({
        "inputs": inputs,
        "input_refs": input_refs,
        "policy": policy,
        "trace": trace,
    });
    json!({
        "inputs": inputs,
        "input_refs": input_refs,
        "input_hash": json_sha256(&hash_payload),
        "input_artifact_integrity": artifact_integrity_map(input_refs),
        "policy": policy,
        "trace": trace,
    })
}

fn node_output_json(
    outputs: &serde_json::Value,
    output_refs: &std::collections::BTreeMap<String, String>,
    diagnostic_refs: &std::collections::BTreeMap<String, String>,
    warning: &Option<String>,
    trace: &std::collections::BTreeMap<String, serde_json::Value>,
) -> serde_json::Value {
    let hash_payload = json!({
        "outputs": outputs,
        "output_refs": output_refs,
        "diagnostic_refs": diagnostic_refs,
        "warning": warning,
        "trace": trace,
    });
    json!({
        "outputs": outputs,
        "output_refs": output_refs,
        "output_hash": json_sha256(&hash_payload),
        "output_artifact_integrity": artifact_integrity_map(output_refs),
        "diagnostic_refs": diagnostic_refs,
        "diagnostic_artifact_integrity": artifact_integrity_map(diagnostic_refs),
        "warning": warning,
        "trace": trace,
    })
}

fn artifact_integrity_map(refs: &std::collections::BTreeMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        refs.iter()
            .map(|(name, uri)| {
                let integrity = artifact_integrity_for_uri(uri);
                (
                    name.clone(),
                    json!({
                        "uri": uri,
                        "sha256": integrity.sha256,
                        "size_bytes": integrity.size_bytes,
                    }),
                )
            })
            .collect(),
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ArtifactIntegrity {
    sha256: Option<String>,
    size_bytes: Option<i64>,
}

fn artifact_integrity_for_uri(uri: &str) -> ArtifactIntegrity {
    let Some(path) = local_artifact_path(uri) else {
        return ArtifactIntegrity::default();
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return ArtifactIntegrity::default();
    };
    if !metadata.is_file() {
        return ArtifactIntegrity::default();
    }
    let size_bytes = i64::try_from(metadata.len()).ok();
    let Ok(mut file) = std::fs::File::open(&path) else {
        return ArtifactIntegrity {
            sha256: None,
            size_bytes,
        };
    };
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buf[..read]),
            Err(_) => {
                return ArtifactIntegrity {
                    sha256: None,
                    size_bytes,
                };
            }
        }
    }
    ArtifactIntegrity {
        sha256: Some(hex_digest(hasher.finalize().as_slice())),
        size_bytes,
    }
}

fn local_artifact_path(uri: &str) -> Option<PathBuf> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    if let Some(path) = uri.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }
    if uri.contains("://") {
        return None;
    }
    Some(PathBuf::from(uri))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn json_sha256(value: &serde_json::Value) -> String {
    let mut canonical = String::new();
    write_canonical_json(value, &mut canonical);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    hex_digest(&digest)
}

fn write_canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(value) => out.push_str(&value.to_string()),
        serde_json::Value::String(value) => {
            let encoded = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
            out.push_str(&encoded);
        }
        serde_json::Value::Array(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical_json(value, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            out.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                let encoded_key = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string());
                out.push_str(&encoded_key);
                out.push(':');
                write_canonical_json(value, out);
            }
            out.push('}');
        }
    }
}

async fn attach_existing_live_event(
    tx: &mut Transaction<'_, Postgres>,
    app_run_id: Uuid,
    dag_run_id: Uuid,
    node_run_id: Option<Uuid>,
    event: &agenthero_dag_executor::DagExecutionEvent,
    payload: &serde_json::Value,
) -> anyhow::Result<bool> {
    let Some(match_key) = live_event_match_key(event, payload) else {
        return Ok(false);
    };

    let result = match match_key {
        LiveEventMatchKey::Node { node_id, attempt } => sqlx::query(
            "update dag_events \
             set dag_run_id = $1, node_run_id = $2, message = coalesce(message, $3), payload = $4 \
             where id = ( \
               select id from dag_events \
               where app_run_id = $5 \
                 and (dag_run_id is null or dag_run_id = $1) \
                 and event_type = $6 \
                 and payload->>'node_id' = $7 \
                 and coalesce(nullif(payload->>'attempt', '')::int, 1) = $8 \
               order by created_at asc, id asc \
               limit 1 \
             )",
        )
        .bind(dag_run_id)
        .bind(node_run_id)
        .bind(event.message.as_deref())
        .bind(payload)
        .bind(app_run_id)
        .bind(&event.event_type)
        .bind(node_id)
        .bind(attempt)
        .execute(&mut **tx)
        .await?,
        LiveEventMatchKey::Dag {
            dag_type,
            manifest_hash,
        } => sqlx::query(
            "update dag_events \
             set dag_run_id = $1, node_run_id = null, message = coalesce(message, $2), payload = $3 \
             where id = ( \
               select id from dag_events \
               where app_run_id = $4 \
                 and (dag_run_id is null or dag_run_id = $1) \
                 and event_type = $5 \
                 and payload->>'dag_type' = $6 \
                 and payload->>'manifest_hash' = $7 \
               order by created_at asc, id asc \
               limit 1 \
             )",
        )
        .bind(dag_run_id)
        .bind(event.message.as_deref())
        .bind(payload)
        .bind(app_run_id)
        .bind(&event.event_type)
        .bind(dag_type)
        .bind(manifest_hash)
        .execute(&mut **tx)
        .await?,
    };
    Ok(result.rows_affected() > 0)
}

fn record_from_row(row: sqlx::postgres::PgRow) -> AppRunRecord {
    AppRunRecord {
        id: row.get("id"),
        app_id: row.get("app_id"),
        action_id: row.get("action_id"),
        state: row.get("state"),
        input: row.get("input"),
        output: row.get("output"),
        error_code: row.get("error_code"),
        error_message: row.get("error_message"),
        error_retryable: row.get("error_retryable"),
        attempt: row.get("attempt"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn app_run_log_metadata(run_id: Uuid) -> (bool, Option<u64>) {
    let path = crate::dag_apps::app_run_log_path(run_id);
    let metadata = std::fs::metadata(path).ok();
    let log_exists = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
    let log_bytes = metadata.map(|metadata| metadata.len());
    (log_exists, log_bytes)
}

fn report_state(report: &DagExecutionReport) -> &'static str {
    match report.status {
        DagNodeStatus::Ok => "done",
        DagNodeStatus::Degraded | DagNodeStatus::Skipped => "partial",
        DagNodeStatus::Pending | DagNodeStatus::Running => "running",
        DagNodeStatus::AwaitingApproval => "awaiting_approval",
        DagNodeStatus::Failed => "failed",
    }
}

fn failed_app_terminal_dag_state(app_state: &str) -> &'static str {
    match app_state {
        "system_failed" => "system_failed",
        _ => "failed",
    }
}

fn successful_app_terminal_dag_state(app_state: &str) -> &'static str {
    match app_state {
        "awaiting_approval" => "awaiting_approval",
        _ => "done",
    }
}

fn node_state(status: DagNodeStatus) -> &'static str {
    match status {
        DagNodeStatus::Pending => "queued",
        DagNodeStatus::Running => "running",
        DagNodeStatus::AwaitingApproval => "awaiting_approval",
        DagNodeStatus::Ok => "ok",
        DagNodeStatus::Degraded => "degraded",
        DagNodeStatus::Failed => "failed",
        DagNodeStatus::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_lease_recovery_uses_action_retry_budget() {
        assert_eq!(expired_lease_decision(1, 3), ExpiredLeaseDecision::Requeue);
        assert_eq!(expired_lease_decision(2, 3), ExpiredLeaseDecision::Requeue);
        assert_eq!(
            expired_lease_decision(3, 3),
            ExpiredLeaseDecision::SystemFailed
        );
        assert_eq!(
            expired_lease_decision(4, 3),
            ExpiredLeaseDecision::SystemFailed
        );
    }

    #[test]
    fn failed_app_state_maps_to_terminal_dag_state() {
        assert_eq!(failed_app_terminal_dag_state("failed"), "failed");
        assert_eq!(
            failed_app_terminal_dag_state("system_failed"),
            "system_failed"
        );
        assert_eq!(failed_app_terminal_dag_state("unexpected"), "failed");
    }

    #[test]
    fn successful_app_state_maps_to_terminal_dag_state() {
        assert_eq!(successful_app_terminal_dag_state("done"), "done");
        assert_eq!(successful_app_terminal_dag_state("partial"), "done");
        assert_eq!(
            successful_app_terminal_dag_state("awaiting_approval"),
            "awaiting_approval"
        );
    }

    #[test]
    fn worker_completion_preserves_operator_cancelled_terminal_state() {
        assert!(worker_completion_should_update_run("running"));
        assert!(!worker_completion_should_update_run("cancelled"));
        assert!(!worker_completion_should_update_run("done"));
        assert!(!worker_completion_should_update_run("failed"));
        assert!(!worker_completion_should_update_run("system_failed"));
        assert!(!worker_completion_should_update_run("awaiting_approval"));
    }

    #[test]
    fn live_node_summary_marks_cancelled_nodes_terminal() {
        let started_at = chrono::DateTime::parse_from_rfc3339("2026-06-25T05:05:24Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let cancelled_at = chrono::DateTime::parse_from_rfc3339("2026-06-25T05:05:34Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let events = vec![
            AppRunEvent {
                id: 1,
                level: "info".to_string(),
                event_type: "node.started".to_string(),
                message: Some("delayed_marker started".to_string()),
                payload: json!({
                    "node_id": "delayed_marker",
                    "node_kind": "tool",
                    "tool_id": "delayed_marker",
                    "attempt": 1
                }),
                created_at: started_at,
            },
            AppRunEvent {
                id: 2,
                level: "warn".to_string(),
                event_type: "node.cancelled".to_string(),
                message: Some("delayed_marker cancelled".to_string()),
                payload: json!({
                    "node_id": "delayed_marker",
                    "node_kind": "tool",
                    "tool_id": "delayed_marker",
                    "attempt": 1,
                    "status": "cancelled"
                }),
                created_at: cancelled_at,
            },
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "delayed_marker");
        assert_eq!(summaries[0].event_type, "node.cancelled");
        assert_eq!(summaries[0].state, "cancelled");
        assert_eq!(summaries[0].status.as_deref(), Some("cancelled"));
    }

    #[test]
    fn cancelled_node_event_payload_preserves_node_identity() {
        let run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let dag_run_id = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

        let payload = cancelled_node_event_payload(
            run_id,
            dag_run_id,
            json!({
                "node_id": "delayed_marker",
                "node_kind": "tool",
                "tool_id": "delayed_marker",
                "attempt": 1,
                "manifest_hash": "fnv1a64:test",
                "lease_id": "33333333-3333-3333-3333-333333333333"
            }),
            "operator cancelled",
        );

        assert_eq!(payload["app_run_id"], json!(run_id.to_string()));
        assert_eq!(payload["dag_run_id"], json!(dag_run_id.to_string()));
        assert_eq!(payload["node_id"], "delayed_marker");
        assert_eq!(payload["node_kind"], "tool");
        assert_eq!(payload["tool_id"], "delayed_marker");
        assert_eq!(payload["attempt"], json!(1));
        assert_eq!(payload["manifest_hash"], "fnv1a64:test");
        assert_eq!(payload["lease_id"], "33333333-3333-3333-3333-333333333333");
        assert_eq!(payload["status"], "cancelled");
        assert_eq!(payload["cancelled"], json!(true));
        assert_eq!(payload["cancel_reason"], "operator cancelled");
    }

    #[test]
    fn durable_app_event_payload_includes_agenthero_trace_field_contract() {
        let run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        let payload = durable_app_event_payload(
            run_id,
            json!({
                "decision": "requeue",
                "latency_ms": 17
            }),
        );

        assert_eq!(
            payload["app_run_id"],
            json!("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(payload["decision"], json!("requeue"));
        assert_eq!(payload["duration_ms"], json!(17));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                payload.get(*field).is_some(),
                "durable app event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
        assert_eq!(payload["node_id"], serde_json::Value::Null);
        assert_eq!(payload["attempt"], serde_json::Value::Null);
    }

    #[test]
    fn approval_requeued_payload_includes_agenthero_trace_field_contract() {
        let run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        let payload = approval_requeued_event_payload(run_id, "approval/human_release");

        assert_eq!(
            payload["app_run_id"],
            json!("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(payload["approved_key"], json!("approval/human_release"));
        assert_eq!(payload["state"], json!("queued"));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                payload.get(*field).is_some(),
                "approval requeue payload should include mandatory AgentHero trace field `{field}`"
            );
        }
        assert_eq!(payload["node_id"], serde_json::Value::Null);
        assert_eq!(payload["attempt"], serde_json::Value::Null);
    }

    #[test]
    fn dag_run_id_from_report_input_uses_preassigned_runtime_identity() {
        let dag_run_id = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let report: DagExecutionReport = serde_json::from_value(json!({
            "dag_type": "review-loop",
            "manifest_version": 1,
            "manifest_hash": "fnv1a64:test",
            "status": "ok",
            "input": {
                "values": {
                    "dag_run_id": "22222222-2222-2222-2222-222222222222"
                },
                "artifacts": {}
            },
            "nodes": [],
            "outputs": {"values": {}, "artifacts": {}},
            "events": []
        }))
        .expect("report deserializes");

        assert_eq!(preassigned_dag_run_id(&report), Some(dag_run_id));
    }

    #[test]
    fn dag_type_for_claimed_run_uses_queued_adapter_input_contract() {
        let mut input = StoredAppRunInput::default();
        input
            .input
            .values
            .insert("dag_type".to_string(), json!("review-loop"));

        assert_eq!(dag_type_for_claimed_run(&input), Some("review-loop"));
    }

    #[test]
    fn app_run_detail_serializes_node_artifact_and_provenance_observability() {
        let created_at = Utc::now();
        let run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let dag_run_id = uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let node_run_id = uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let artifact_id = uuid::Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let event_payload = durable_dag_event_payload(
            run_id,
            dag_run_id,
            json!({
                "node_id": "lean_verifier",
                "node_kind": "verify",
                "tool_id": "lean",
                "attempt": 1,
                "manifest_hash": "fnv1a64:test",
                "lease_id": "55555555-5555-5555-5555-555555555555",
                "status": "ok",
                "duration_ms": 12
            }),
        );
        let detail = AppRunDetail {
            run: AppRunRecord {
                id: run_id,
                app_id: "sample-app".to_string(),
                action_id: "run".to_string(),
                state: "done".to_string(),
                input: json!({}),
                output: json!({
                    "report": {
                        "dag_type": "sample-dag",
                        "manifest_hash": "fnv1a64:test"
                    }
                }),
                error_code: None,
                error_message: None,
                error_retryable: None,
                attempt: 1,
                created_at,
                started_at: Some(created_at),
                finished_at: Some(created_at),
            },
            latest_dag_run: Some(AppRunDagSummary {
                id: dag_run_id,
                dag_type: "sample-dag".to_string(),
                manifest_version: Some(1),
                manifest_hash: Some("fnv1a64:test".to_string()),
                state: "done".to_string(),
                input: json!({
                    "values": {
                        "lean_policy": {
                            "auto_detect": true
                        }
                    },
                    "artifacts": {}
                }),
                output: json!({
                    "values": {},
                    "artifacts": {}
                }),
                started_at: Some(created_at),
                finished_at: Some(created_at),
                created_at,
            }),
            live_nodes: vec![AppRunLiveNodeSummary {
                node_id: "lean_verifier".to_string(),
                state: "ok".to_string(),
                event_type: "node.completed".to_string(),
                level: "info".to_string(),
                attempt: 1,
                node_kind: Some("verify".to_string()),
                status: Some("ok".to_string()),
                message: Some("lean_verifier Ok".to_string()),
                payload: event_payload.clone(),
                event_id: 1,
                updated_at: created_at,
            }],
            nodes: vec![AppRunNodeSummary {
                id: node_run_id,
                node_id: "lean_verifier".to_string(),
                node_kind: "verify".to_string(),
                state: "ok".to_string(),
                attempt: 1,
                runner: Some("lean".to_string()),
                model: Some("gpt-5".to_string()),
                prompt_hash: Some("fnv1a64:prompt".to_string()),
                command: json!(["lake", "env", "lean", "Proof.lean"]),
                exit_status: Some(0),
                role: None,
                tool: Some("lean".to_string()),
                child_dag_type: None,
                required: true,
                input_refs: json!({ "source": "artifact://source" }),
                output_refs: json!({ "proof": "artifact://proof" }),
                diagnostic_refs: json!({ "stderr": "artifact://stderr" }),
                policy: json!({
                    "tool": {
                        "budget_units": 7,
                        "approval_required": true,
                        "network": { "allow": false },
                        "filesystem": {
                            "read": ["."],
                            "write": [".agenthero"]
                        }
                    },
                    "retry": {
                        "max_attempts": 2,
                        "backoff_ms": 100
                    }
                }),
                input: json!({
                    "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "policy": {
                        "timeout_secs": 30
                    },
                    "input_artifact_integrity": {
                        "source": {
                            "uri": "artifact://source",
                            "sha256": null,
                            "size_bytes": null
                        }
                    }
                }),
                output: json!({
                    "output_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "output_artifact_integrity": {
                        "proof": {
                            "uri": "artifact://proof",
                            "sha256": null,
                            "size_bytes": null
                        }
                    }
                }),
                error_message: None,
                latency_ms: Some(12),
                started_at: Some(created_at),
                finished_at: Some(created_at),
                created_at,
            }],
            artifacts: vec![AppRunArtifactSummary {
                id: artifact_id,
                name: "proof".to_string(),
                uri: "artifact://proof".to_string(),
                media_type: Some("text/plain".to_string()),
                sha256: Some("abc123".to_string()),
                size_bytes: Some(42),
                schema_ref: None,
                metadata: json!({ "artifact_kind": "node_output" }),
                node_id: Some("lean_verifier".to_string()),
                attempt: Some(1),
                created_at,
            }],
            determinism: AppRunDeterminismSummary {
                manifest_hash: Some("fnv1a64:test".to_string()),
                frozen_input_hash: Some(
                    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
                ),
                dag_output_hash: Some(
                    "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".to_string(),
                ),
                checkpoint_available: true,
                node_attempts: 1,
                node_input_hashes: 1,
                node_output_hashes: 1,
                artifacts: 1,
                artifacts_with_sha256: 1,
                artifacts_missing_sha256: 0,
                replay_ready: true,
                compare_ready: true,
            },
            policies: AppRunPolicySummary {
                node_attempts: 1,
                nodes_with_policy: 1,
                timeout_limited_nodes: 1,
                budget_limited_nodes: 1,
                budget_units_requested: 7,
                approval_gates: 0,
                approval_required_tools: 1,
                network_denied_nodes: 1,
                filesystem_restricted_nodes: 1,
                isolation_required_nodes: 1,
                retry_policies: 1,
                policy_denied_nodes: 0,
            },
            observability_summary: AppRunObservabilitySummary {
                event_count: 1,
                log_exists: true,
                log_bytes: Some(2048),
            },
            observability: AppRunObservabilityLinks::for_run_context(
                run_id,
                Some("sample-app"),
                Some("run"),
                Some("sample-dag"),
            ),
            recent_events: vec![AppRunEvent {
                id: 1,
                level: "info".to_string(),
                event_type: "node.completed".to_string(),
                message: Some("lean_verifier Ok".to_string()),
                payload: event_payload.clone(),
                created_at,
            }],
            events: vec![AppRunEvent {
                id: 1,
                level: "info".to_string(),
                event_type: "node.completed".to_string(),
                message: Some("lean_verifier Ok".to_string()),
                payload: event_payload,
                created_at,
            }],
        };

        let value = serde_json::to_value(detail).expect("detail serializes");

        assert_eq!(value["latest_dag_run"]["manifest_hash"], "fnv1a64:test");
        assert_eq!(value["determinism"]["manifest_hash"], "fnv1a64:test");
        assert_eq!(
            value["determinism"]["frozen_input_hash"]
                .as_str()
                .map(str::len),
            Some(64)
        );
        assert_eq!(
            value["determinism"]["dag_output_hash"]
                .as_str()
                .map(str::len),
            Some(64)
        );
        assert_eq!(value["determinism"]["node_attempts"], 1);
        assert_eq!(value["determinism"]["node_input_hashes"], 1);
        assert_eq!(value["determinism"]["node_output_hashes"], 1);
        assert_eq!(value["determinism"]["artifacts"], 1);
        assert_eq!(value["determinism"]["artifacts_with_sha256"], 1);
        assert_eq!(value["determinism"]["artifacts_missing_sha256"], 0);
        assert_eq!(value["determinism"]["checkpoint_available"], true);
        assert_eq!(value["determinism"]["replay_ready"], true);
        assert_eq!(value["determinism"]["compare_ready"], true);
        assert_eq!(value["policies"]["node_attempts"], 1);
        assert_eq!(value["policies"]["nodes_with_policy"], 1);
        assert_eq!(value["policies"]["timeout_limited_nodes"], 1);
        assert_eq!(value["policies"]["budget_limited_nodes"], 1);
        assert_eq!(value["policies"]["budget_units_requested"], 7);
        assert_eq!(value["policies"]["approval_gates"], 0);
        assert_eq!(value["policies"]["approval_required_tools"], 1);
        assert_eq!(value["policies"]["network_denied_nodes"], 1);
        assert_eq!(value["policies"]["filesystem_restricted_nodes"], 1);
        assert_eq!(value["policies"]["isolation_required_nodes"], 1);
        assert_eq!(value["policies"]["retry_policies"], 1);
        assert_eq!(value["policies"]["policy_denied_nodes"], 0);
        assert_eq!(value["observability_summary"]["event_count"], 1);
        assert_eq!(value["observability_summary"]["log_exists"], true);
        assert_eq!(value["observability_summary"]["log_bytes"], 2048);
        assert_eq!(
            value["observability"]["status_command"],
            "agh app status 11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(
            value["observability"]["event_stream_path"],
            "/app-runs/11111111-1111-1111-1111-111111111111/events/stream"
        );
        assert_eq!(
            value["observability"]["logs_path"],
            "/app-runs/11111111-1111-1111-1111-111111111111/logs"
        );
        assert_eq!(value["observability"]["metrics_path"], "/metrics");
        assert_eq!(
            value["observability"]["metrics_labels"]["app"],
            "sample-app"
        );
        assert_eq!(value["observability"]["metrics_labels"]["action"], "run");
        assert_eq!(
            value["observability"]["metrics_labels"]["dag_type"],
            "sample-dag"
        );
        assert_eq!(value["observability"]["trace_fields"][0], "app_run_id");
        assert_eq!(
            value["observability"]["event_contract"]["trace_fields"][0],
            "app_run_id"
        );
        assert_eq!(
            value["observability"]["log_contract"]["format"],
            "durable_text_log_with_agenthero_event_jsonl"
        );
        assert_eq!(
            value["observability"]["stream_contract"]["format"],
            "server_sent_events"
        );
        assert_eq!(
            value["observability"]["stream_contract"]["cursor_parameter"],
            "after_id"
        );
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                value["events"][0]["payload"].get(*field).is_some(),
                "app-run event payload should serialize mandatory AgentHero trace field `{field}`"
            );
            assert!(
                value["recent_events"][0]["payload"].get(*field).is_some(),
                "recent app-run event payload should serialize mandatory AgentHero trace field `{field}`"
            );
            assert!(
                value["live_nodes"][0]["payload"].get(*field).is_some(),
                "live node payload should serialize mandatory AgentHero trace field `{field}`"
            );
        }
        assert_eq!(
            value["events"][0]["payload"]["app_run_id"],
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(
            value["events"][0]["payload"]["dag_run_id"],
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(value["events"][0]["payload"]["node_id"], "lean_verifier");
        assert_eq!(value["events"][0]["payload"]["node_kind"], "verify");
        assert_eq!(value["events"][0]["payload"]["tool_id"], "lean");
        assert_eq!(
            value["events"][0]["payload"]["lease_id"],
            "55555555-5555-5555-5555-555555555555"
        );
        assert_eq!(value["events"][0]["payload"]["status"], "ok");
        assert_eq!(value["events"][0]["payload"]["duration_ms"], 12);
        assert_eq!(
            value["latest_dag_run"]["input"]["values"]["lean_policy"]["auto_detect"],
            true
        );
        assert_eq!(value["live_nodes"][0]["node_id"], "lean_verifier");
        assert_eq!(value["live_nodes"][0]["state"], "ok");
        assert_eq!(value["live_nodes"][0]["event_type"], "node.completed");
        assert_eq!(value["live_nodes"][0]["payload"]["tool_id"], "lean");
        assert_eq!(value["nodes"][0]["state"], "ok");
        assert_eq!(value["nodes"][0]["node_kind"], "verify");
        assert_eq!(value["nodes"][0]["required"], true);
        assert_eq!(value["nodes"][0]["runner"], "lean");
        assert_eq!(value["nodes"][0]["model"], "gpt-5");
        assert_eq!(value["nodes"][0]["prompt_hash"], "fnv1a64:prompt");
        assert_eq!(value["nodes"][0]["command"][0], "lake");
        assert_eq!(value["nodes"][0]["exit_status"], 0);
        assert_eq!(value["nodes"][0]["latency_ms"], 12);
        assert_eq!(
            value["nodes"][0]["input_refs"]["source"],
            "artifact://source"
        );
        assert_eq!(
            value["nodes"][0]["output_refs"]["proof"],
            "artifact://proof"
        );
        assert_eq!(
            value["nodes"][0]["diagnostic_refs"]["stderr"],
            "artifact://stderr"
        );
        assert_eq!(
            value["nodes"][0]["policy"]["tool"]["approval_required"],
            true
        );
        assert_eq!(value["nodes"][0]["policy"]["tool"]["budget_units"], 7);
        assert_eq!(
            value["nodes"][0]["policy"]["tool"]["network"]["allow"],
            false
        );
        assert_eq!(
            value["nodes"][0]["input"]["input_artifact_integrity"]["source"]["uri"],
            "artifact://source"
        );
        assert_eq!(
            value["nodes"][0]["output"]["output_artifact_integrity"]["proof"]["uri"],
            "artifact://proof"
        );
        assert_eq!(value["artifacts"][0]["node_id"], "lean_verifier");
        assert_eq!(value["artifacts"][0]["uri"], "artifact://proof");
        assert_eq!(value["artifacts"][0]["sha256"], "abc123");
        assert_eq!(value["artifacts"][0]["size_bytes"], 42);
        assert_eq!(
            value["artifacts"][0]["metadata"]["artifact_kind"],
            "node_output"
        );
        assert_eq!(value["events"][0]["event_type"], "node.completed");
        assert_eq!(value["events"][0]["node_id"], "lean_verifier");
        assert_eq!(value["events"][0]["attempt"], 1);
        assert_eq!(value["recent_events"][0]["event_type"], "node.completed");
        assert_eq!(value["recent_events"][0]["node_id"], "lean_verifier");
        assert_eq!(value["recent_events"][0]["attempt"], 1);
    }

    #[test]
    fn adapter_node_event_record_materializes_lean_compile_node() {
        let payload = durable_dag_event_payload(
            uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            json!({
                "node_id": "lean_target_3_compile",
                "node_kind": "lean",
                "tool_id": "lake_env_lean",
                "attempt": 1,
                "status": "fail",
                "command": ["lake", "env", "lean", "Project/Proofs.lean"],
                "exit_status": 1,
                "duration_ms": 200,
                "artifact_id": "review_loop/lean/targets/02_add_zero/Project/Proofs.lean",
                "output_refs": ["review_loop/lean/targets/02_add_zero/Project/Proofs.lean"],
                "error": "type mismatch"
            }),
        );

        let record =
            adapter_node_event_record("node.failed", Some("Lean target compile failed"), &payload)
                .expect("node record");

        assert_eq!(record.node_id, "lean_target_3_compile");
        assert_eq!(record.node_kind, "lean");
        assert_eq!(record.state, "failed");
        assert_eq!(record.tool.as_deref(), Some("lake_env_lean"));
        assert_eq!(record.command[0], "lake");
        assert_eq!(record.exit_status, Some(1));
        assert_eq!(record.latency_ms, Some(200));
        assert_eq!(
            record.output_refs[0],
            "review_loop/lean/targets/02_add_zero/Project/Proofs.lean"
        );
        assert_eq!(record.error_message.as_deref(), Some("type mismatch"));
    }

    #[test]
    fn adapter_node_event_record_maps_completed_statuses_to_durable_states() {
        for (status, expected_state) in [
            ("ok", "ok"),
            ("pass", "ok"),
            ("partial", "degraded"),
            ("fallback_ok", "degraded"),
            ("skipped", "skipped"),
            ("fail", "failed"),
        ] {
            let payload = json!({
                "node_id": "formalize_typed_ir",
                "node_kind": "llm",
                "tool_id": "formalize_source_inventory_typed_transcriber",
                "attempt": 1,
                "status": status,
            });
            let record =
                adapter_node_event_record("node.completed", None, &payload).expect("node record");
            assert_eq!(record.state, expected_state, "status={status}");
        }
    }

    #[test]
    fn app_run_list_item_serializes_shallow_observability_without_wrapping_run() {
        let created_at = Utc::now();
        let run_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let item = AppRunListItem {
            run: AppRunRecord {
                id: run_id,
                app_id: "sample-app".to_string(),
                action_id: "run".to_string(),
                state: "done".to_string(),
                input: json!({}),
                output: json!({}),
                error_code: None,
                error_message: None,
                error_retryable: None,
                attempt: 1,
                created_at,
                started_at: Some(created_at),
                finished_at: Some(created_at),
            },
            observability: AppRunListObservabilitySummary {
                event_count: 12,
                log_exists: true,
                log_bytes: Some(4096),
                links: AppRunObservabilityLinks::for_run_context(
                    run_id,
                    Some("sample-app"),
                    Some("run"),
                    None,
                ),
            },
        };

        let value = serde_json::to_value(item).expect("list item serializes");

        assert_eq!(value["id"], run_id.to_string());
        assert!(
            value.get("run").is_none(),
            "run fields should stay top-level"
        );
        assert_eq!(value["observability"]["event_count"], 12);
        assert_eq!(value["observability"]["log_exists"], true);
        assert_eq!(value["observability"]["log_bytes"], 4096);
        assert_eq!(
            value["observability"]["links"]["events_path"],
            "/app-runs/11111111-1111-1111-1111-111111111111/events"
        );
        assert_eq!(
            value["observability"]["links"]["metrics_labels"]["app"],
            "sample-app"
        );
        assert_eq!(
            value["observability"]["links"]["metrics_labels"]["action"],
            "run"
        );
    }

    #[test]
    fn determinism_summary_marks_replay_and_compare_ready_from_persisted_hashes() {
        let created_at = Utc::now();
        let dag = AppRunDagSummary {
            id: uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            dag_type: "sample-dag".to_string(),
            manifest_version: Some(1),
            manifest_hash: Some("fnv1a64:test".to_string()),
            state: "done".to_string(),
            input: json!({"values": {"seed": 1}, "artifacts": {}}),
            output: json!({"values": {"ok": true}, "artifacts": {}}),
            started_at: Some(created_at),
            finished_at: Some(created_at),
            created_at,
        };
        let nodes = vec![AppRunNodeSummary {
            id: uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            node_id: "compile".to_string(),
            node_kind: "shell".to_string(),
            state: "ok".to_string(),
            attempt: 1,
            runner: Some("shell".to_string()),
            model: None,
            prompt_hash: None,
            command: json!(["cargo", "check"]),
            exit_status: Some(0),
            role: None,
            tool: Some("cargo".to_string()),
            child_dag_type: None,
            required: true,
            input_refs: json!({}),
            output_refs: json!({}),
            diagnostic_refs: json!({}),
            policy: json!({}),
            input: json!({
                "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
            output: json!({
                "output_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            }),
            error_message: None,
            latency_ms: Some(10),
            started_at: Some(created_at),
            finished_at: Some(created_at),
            created_at,
        }];
        let artifacts = vec![AppRunArtifactSummary {
            id: uuid::Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            name: "report".to_string(),
            uri: "file:///tmp/report.json".to_string(),
            media_type: Some("application/json".to_string()),
            sha256: Some("abc123".to_string()),
            size_bytes: Some(2),
            schema_ref: Some("schemas/report.schema.json".to_string()),
            metadata: json!({}),
            node_id: Some("compile".to_string()),
            attempt: Some(1),
            created_at,
        }];

        let summary = app_run_determinism_summary(
            &json!({"report": {"dag_type": "sample-dag"}}),
            Some(&dag),
            &nodes,
            &artifacts,
        );

        assert_eq!(summary.manifest_hash.as_deref(), Some("fnv1a64:test"));
        assert_eq!(summary.frozen_input_hash.as_deref().map(str::len), Some(64));
        assert_eq!(summary.dag_output_hash.as_deref().map(str::len), Some(64));
        assert!(summary.checkpoint_available);
        assert_eq!(summary.node_attempts, 1);
        assert_eq!(summary.node_input_hashes, 1);
        assert_eq!(summary.node_output_hashes, 1);
        assert_eq!(summary.artifacts_with_sha256, 1);
        assert_eq!(summary.artifacts_missing_sha256, 0);
        assert!(summary.replay_ready);
        assert!(summary.compare_ready);
    }

    #[test]
    fn app_run_comparison_detects_output_and_artifact_drift() {
        let left =
            sample_comparable_app_run_detail("11111111-1111-1111-1111-111111111111", "node-left");
        let right =
            sample_comparable_app_run_detail("22222222-2222-2222-2222-222222222222", "node-right");

        let comparison = compare_app_run_details(&left, &right);

        assert!(comparison.compare_ready);
        assert!(!comparison.matches);
        assert!(comparison.checks.same_manifest_hash);
        assert!(comparison.checks.same_frozen_input_hash);
        assert!(!comparison.checks.same_dag_output_hash);
        assert!(!comparison.checks.same_node_outputs);
        assert!(!comparison.checks.same_artifacts);
        assert!(comparison
            .differences
            .iter()
            .any(|difference| difference.field == "determinism.dag_output_hash"));
        assert!(comparison
            .differences
            .iter()
            .any(|difference| difference.field == "node_outputs.compile#1"));
        assert!(comparison
            .differences
            .iter()
            .any(|difference| difference.field == "artifacts.compile:report#1"));
    }

    #[test]
    fn replay_input_from_completed_run_attaches_checkpoint_and_preserves_args() {
        let source = AppRunRecord {
            id: uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            app_id: "platform-smoke".to_string(),
            action_id: "verification-routing-smoke".to_string(),
            state: "done".to_string(),
            input: serde_json::to_value(StoredAppRunInput {
                args: vec!["verification-routing-smoke".to_string()],
                input: DagIo {
                    values: std::collections::BTreeMap::from([(
                        "dag_type".to_string(),
                        json!("verification-routing-smoke"),
                    )]),
                    artifacts: Default::default(),
                },
                dry_run: false,
                json: true,
                checkpoint: None,
                retry: StoredAppRunRetry { max_attempts: 1 },
            })
            .unwrap(),
            output: json!({
                "protocol": "agenthero.app.v1",
                "app": "platform-smoke",
                "action": "verification-routing-smoke",
                "dag_type": "verification-routing-smoke",
                "ok": true,
                "report": {
                    "dag_type": "verification-routing-smoke",
                    "manifest_version": 1,
                    "manifest_hash": "fnv1a64:test",
                    "status": "ok",
                    "input": {"values": {}, "artifacts": {}},
                    "nodes": [],
                    "outputs": {"values": {}, "artifacts": {}},
                    "events": []
                },
                "output": null,
                "error": null
            }),
            error_code: None,
            error_message: None,
            error_retryable: None,
            attempt: 1,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
        };

        let replay_input =
            replay_input_from_completed_run(&source, StoredAppRunRetry { max_attempts: 3 })
                .expect("replay input");

        assert_eq!(replay_input.args, vec!["verification-routing-smoke"]);
        assert_eq!(
            replay_input.input.values["dag_type"],
            json!("verification-routing-smoke")
        );
        assert!(replay_input.checkpoint.is_some());
        assert_eq!(replay_input.retry.max_attempts, 3);
        assert_eq!(
            replay_input
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.manifest_hash.as_str()),
            Some("fnv1a64:test")
        );
    }

    #[test]
    fn replay_plan_from_input_reports_checkpoint_without_queue_identity() {
        let source = AppRunRecord {
            id: uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            app_id: "platform-smoke".to_string(),
            action_id: "verification-routing-smoke".to_string(),
            state: "done".to_string(),
            input: serde_json::to_value(StoredAppRunInput {
                args: vec!["verification-routing-smoke".to_string()],
                input: DagIo::default(),
                dry_run: false,
                json: true,
                checkpoint: None,
                retry: StoredAppRunRetry { max_attempts: 1 },
            })
            .unwrap(),
            output: json!({
                "protocol": "agenthero.app.v1",
                "app": "platform-smoke",
                "action": "verification-routing-smoke",
                "dag_type": "verification-routing-smoke",
                "ok": true,
                "report": {
                    "dag_type": "verification-routing-smoke",
                    "manifest_version": 1,
                    "manifest_hash": "fnv1a64:test",
                    "status": "ok",
                    "input": {"values": {}, "artifacts": {}},
                    "nodes": [],
                    "outputs": {"values": {}, "artifacts": {}},
                    "events": []
                },
                "output": null,
                "error": null
            }),
            error_code: None,
            error_message: None,
            error_retryable: None,
            attempt: 1,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
        };
        let replay_input =
            replay_input_from_completed_run(&source, StoredAppRunRetry { max_attempts: 3 })
                .expect("replay input");

        let plan = replay_plan_from_input(&source, &replay_input);

        assert_eq!(plan.source_run_id, source.id);
        assert_eq!(plan.app_id, "platform-smoke");
        assert_eq!(plan.action_id, "verification-routing-smoke");
        assert_eq!(plan.dag_type, "verification-routing-smoke");
        assert_eq!(plan.manifest_hash, "fnv1a64:test");
    }

    #[test]
    fn app_run_comparison_separates_runtime_identity_from_work_product_match() {
        let left = sample_runtime_identity_rerun_detail(
            "11111111-1111-1111-1111-111111111111",
            "left-runtime",
            "same-result",
        );
        let right = sample_runtime_identity_rerun_detail(
            "22222222-2222-2222-2222-222222222222",
            "right-runtime",
            "same-result",
        );

        let comparison = compare_app_run_details(&left, &right);

        assert!(comparison.compare_ready);
        assert!(!comparison.matches);
        assert!(comparison.work_product_matches);
        assert!(!comparison.checks.same_frozen_input_hash);
        assert!(!comparison.checks.same_dag_output_hash);
        assert!(!comparison.checks.same_node_outputs);
        assert!(comparison.checks.same_normalized_frozen_input_hash);
        assert!(comparison.checks.same_normalized_dag_output_hash);
        assert!(comparison.checks.same_normalized_node_outputs);
        assert!(comparison.work_product_differences.is_empty());
    }

    #[test]
    fn app_run_comparison_ignores_loop_control_sentinel_in_work_product() {
        let mut left = sample_runtime_identity_rerun_detail(
            "11111111-1111-1111-1111-111111111111",
            "left-runtime",
            "same-result",
        );
        let right = sample_runtime_identity_rerun_detail(
            "22222222-2222-2222-2222-222222222222",
            "right-runtime",
            "same-result",
        );
        let output = left
            .latest_dag_run
            .as_mut()
            .expect("sample has latest DAG")
            .output
            .as_object_mut()
            .expect("dag output is an object");
        output
            .get_mut("values")
            .expect("dag output has values")
            .as_object_mut()
            .expect("dag values are an object")
            .insert("loop_continue".to_string(), json!(false));
        left.determinism = app_run_determinism_summary(
            &left.run.output,
            left.latest_dag_run.as_ref(),
            &left.nodes,
            &left.artifacts,
        );

        let comparison = compare_app_run_details(&left, &right);

        assert!(comparison.compare_ready);
        assert!(!comparison.matches);
        assert!(comparison.work_product_matches);
        assert!(comparison.checks.same_normalized_dag_output_hash);
        assert!(comparison.work_product_differences.is_empty());
    }

    fn sample_comparable_app_run_detail(run_id: &str, output_seed: &str) -> AppRunDetail {
        let created_at = Utc::now();
        let run_id = uuid::Uuid::parse_str(run_id).unwrap();
        let dag_run_id = uuid::Uuid::new_v4();
        let dag = AppRunDagSummary {
            id: dag_run_id,
            dag_type: "sample-dag".to_string(),
            manifest_version: Some(1),
            manifest_hash: Some("fnv1a64:test".to_string()),
            state: "done".to_string(),
            input: json!({"values": {"seed": 1}, "artifacts": {}}),
            output: json!({"values": {"seed": output_seed}, "artifacts": {}}),
            started_at: Some(created_at),
            finished_at: Some(created_at),
            created_at,
        };
        let nodes = vec![AppRunNodeSummary {
            id: uuid::Uuid::new_v4(),
            node_id: "compile".to_string(),
            node_kind: "shell".to_string(),
            state: "ok".to_string(),
            attempt: 1,
            runner: Some("shell".to_string()),
            model: None,
            prompt_hash: None,
            command: json!(["cargo", "check"]),
            exit_status: Some(0),
            role: None,
            tool: Some("cargo".to_string()),
            child_dag_type: None,
            required: true,
            input_refs: json!({}),
            output_refs: json!({}),
            diagnostic_refs: json!({}),
            policy: json!({}),
            input: json!({
                "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            }),
            output: json!({
                "output_hash": output_seed
            }),
            error_message: None,
            latency_ms: Some(10),
            started_at: Some(created_at),
            finished_at: Some(created_at),
            created_at,
        }];
        let artifacts = vec![AppRunArtifactSummary {
            id: uuid::Uuid::new_v4(),
            name: "report".to_string(),
            uri: format!("file:///tmp/{output_seed}/report.json"),
            media_type: Some("application/json".to_string()),
            sha256: Some(output_seed.to_string()),
            size_bytes: Some(2),
            schema_ref: Some("schemas/report.schema.json".to_string()),
            metadata: json!({}),
            node_id: Some("compile".to_string()),
            attempt: Some(1),
            created_at,
        }];
        let determinism = app_run_determinism_summary(
            &json!({"report": {"dag_type": "sample-dag"}}),
            Some(&dag),
            &nodes,
            &artifacts,
        );
        AppRunDetail {
            run: AppRunRecord {
                id: run_id,
                app_id: "sample-app".to_string(),
                action_id: "run".to_string(),
                state: "done".to_string(),
                input: json!({}),
                output: json!({"report": {"dag_type": "sample-dag"}}),
                error_code: None,
                error_message: None,
                error_retryable: None,
                attempt: 1,
                created_at,
                started_at: Some(created_at),
                finished_at: Some(created_at),
            },
            latest_dag_run: Some(dag),
            live_nodes: Vec::new(),
            nodes,
            artifacts,
            determinism,
            policies: AppRunPolicySummary::default(),
            observability_summary: AppRunObservabilitySummary {
                event_count: 0,
                log_exists: false,
                log_bytes: None,
            },
            observability: AppRunObservabilityLinks::for_run(run_id),
            recent_events: Vec::new(),
            events: Vec::new(),
        }
    }

    fn sample_runtime_identity_rerun_detail(
        run_id: &str,
        runtime_seed: &str,
        output_seed: &str,
    ) -> AppRunDetail {
        let mut detail = sample_comparable_app_run_detail(run_id, output_seed);
        let dag_run_id = uuid::Uuid::new_v4();
        let lease_id = uuid::Uuid::new_v4();
        if let Some(dag) = detail.latest_dag_run.as_mut() {
            dag.id = dag_run_id;
            dag.input = json!({
                "values": {
                    "seed": 1,
                    "adapter_idempotency_key": format!("app-run:{runtime_seed}"),
                    "app_run_id": detail.run.id.to_string(),
                    "app_run_log_path": format!(".agenthero/app_runs/{runtime_seed}.log"),
                    "dag_run_id": dag_run_id.to_string(),
                    "lease_id": lease_id.to_string()
                },
                "artifacts": {}
            });
            dag.output = json!({
                "values": {
                    "seed": output_seed,
                    "app_run_id": detail.run.id.to_string(),
                    "app_run_log_path": format!(".agenthero/app_runs/{runtime_seed}.log"),
                    "dag_run_id": dag_run_id.to_string(),
                    "lease_id": lease_id.to_string()
                },
                "artifacts": {
                    "report": {
                        "uri": format!(".agenthero/{runtime_seed}/report.json"),
                        "media_type": "application/json",
                        "metadata": {
                            "sha256": output_seed,
                            "size_bytes": 2
                        }
                    }
                }
            });
        }
        detail.nodes[0].output = json!({
            "outputs": ["report"],
            "output_refs": {
                "report": format!(".agenthero/{runtime_seed}/report.json")
            },
            "output_hash": json_sha256(&json!({
                "outputs": ["report"],
                "output_refs": {
                    "report": format!(".agenthero/{runtime_seed}/report.json")
                },
                "diagnostic_refs": {
                    "logs/status.json": format!(".agenthero/{runtime_seed}/status.json")
                },
                "warning": null,
                "trace": {
                    "scheduler": "tokio_concurrent_layer"
                }
            })),
            "output_artifact_integrity": {
                "report": {
                    "sha256": output_seed,
                    "size_bytes": 2,
                    "uri": format!(".agenthero/{runtime_seed}/report.json")
                }
            },
            "diagnostic_refs": {
                "logs/status.json": format!(".agenthero/{runtime_seed}/status.json")
            },
            "warning": null,
            "trace": {
                "scheduler": "tokio_concurrent_layer"
            }
        });
        detail.artifacts[0].uri = format!("file:///tmp/{runtime_seed}/report.json");
        detail.artifacts[0].sha256 = Some(output_seed.to_string());
        detail.determinism = app_run_determinism_summary(
            &detail.run.output,
            detail.latest_dag_run.as_ref(),
            &detail.nodes,
            &detail.artifacts,
        );
        detail
    }

    #[test]
    fn policy_summary_counts_limits_gates_permissions_retries_and_denials() {
        let created_at = Utc::now();
        let nodes = vec![
            AppRunNodeSummary {
                id: uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                node_id: "network_blocked".to_string(),
                node_kind: "tool".to_string(),
                state: "failed".to_string(),
                attempt: 1,
                runner: Some("shell".to_string()),
                model: None,
                prompt_hash: None,
                command: json!(["sh", "-c", "curl example.invalid"]),
                exit_status: None,
                role: None,
                tool: Some("curl".to_string()),
                child_dag_type: None,
                required: true,
                input_refs: json!({}),
                output_refs: json!({}),
                diagnostic_refs: json!({}),
                policy: json!({
                    "tool": {
                        "budget_units": 5,
                        "approval_required": true,
                        "network": { "allow": false },
                        "filesystem": {
                            "read": ["."],
                            "write": [".agenthero"]
                        }
                    },
                    "retry": {
                        "max_attempts": 3,
                        "backoff_ms": 50
                    }
                }),
                input: json!({
                    "policy": {
                        "timeout_secs": 10
                    }
                }),
                output: json!({}),
                error_message: Some("tool `curl` policy requires isolated runner".to_string()),
                latency_ms: None,
                started_at: Some(created_at),
                finished_at: Some(created_at),
                created_at,
            },
            AppRunNodeSummary {
                id: uuid::Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
                node_id: "human_release".to_string(),
                node_kind: "approval".to_string(),
                state: "awaiting_approval".to_string(),
                attempt: 1,
                runner: None,
                model: None,
                prompt_hash: None,
                command: json!(null),
                exit_status: None,
                role: None,
                tool: None,
                child_dag_type: None,
                required: true,
                input_refs: json!({}),
                output_refs: json!({}),
                diagnostic_refs: json!({}),
                policy: json!({
                    "approval": {
                        "approved_key": "approval/human_release"
                    }
                }),
                input: json!({}),
                output: json!({}),
                error_message: None,
                latency_ms: None,
                started_at: Some(created_at),
                finished_at: None,
                created_at,
            },
        ];

        let summary = app_run_policy_summary(&nodes);

        assert_eq!(summary.node_attempts, 2);
        assert_eq!(summary.nodes_with_policy, 2);
        assert_eq!(summary.timeout_limited_nodes, 1);
        assert_eq!(summary.budget_limited_nodes, 1);
        assert_eq!(summary.budget_units_requested, 5);
        assert_eq!(summary.approval_gates, 1);
        assert_eq!(summary.approval_required_tools, 1);
        assert_eq!(summary.network_denied_nodes, 1);
        assert_eq!(summary.filesystem_restricted_nodes, 1);
        assert_eq!(summary.isolation_required_nodes, 1);
        assert_eq!(summary.retry_policies, 1);
        assert_eq!(summary.policy_denied_nodes, 1);
    }

    #[test]
    fn local_file_artifact_integrity_records_size_and_sha256() {
        let run_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-artifact-integrity-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("artifact.txt");
        std::fs::write(&path, b"abc").unwrap();

        let integrity = artifact_integrity_for_uri(&path.to_string_lossy());

        assert_eq!(integrity.size_bytes, Some(3));
        assert_eq!(
            integrity.sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtual_artifact_integrity_leaves_non_local_uris_unhashed() {
        for uri in [
            "artifact://proof",
            "https://example.invalid/artifact.json",
            "s3://bucket/artifact.json",
        ] {
            assert_eq!(
                artifact_integrity_for_uri(uri),
                ArtifactIntegrity::default()
            );
        }
    }

    #[test]
    fn node_output_json_includes_artifact_integrity_snapshot() {
        let run_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-node-integrity-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let output_path = dir.join("output.txt");
        let diagnostic_path = dir.join("stderr.log");
        std::fs::write(&output_path, b"abc").unwrap();
        std::fs::write(&diagnostic_path, b"diagnostic").unwrap();

        let output = node_output_json(
            &json!({"result": "ok"}),
            &std::collections::BTreeMap::from([(
                "output.txt".to_string(),
                output_path.to_string_lossy().to_string(),
            )]),
            &std::collections::BTreeMap::from([(
                "stderr.log".to_string(),
                diagnostic_path.to_string_lossy().to_string(),
            )]),
            &None,
            &std::collections::BTreeMap::new(),
        );

        assert_eq!(output["outputs"]["result"], "ok");
        assert_eq!(
            output["output_artifact_integrity"]["output.txt"]["sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            output["output_artifact_integrity"]["output.txt"]["size_bytes"],
            3
        );
        assert_eq!(
            output["diagnostic_artifact_integrity"]["stderr.log"]["size_bytes"],
            10
        );
        assert_eq!(output["output_hash"].as_str().map(str::len), Some(64));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_input_json_includes_artifact_integrity_snapshot() {
        let run_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-node-input-integrity-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let input_path = dir.join("input.json");
        std::fs::write(&input_path, b"{}").unwrap();

        let input = node_input_json(
            &["input.json".to_string()],
            &std::collections::BTreeMap::from([(
                "input.json".to_string(),
                input_path.to_string_lossy().to_string(),
            )]),
            &std::collections::BTreeMap::from([("timeout_secs".to_string(), json!(30))]),
            &std::collections::BTreeMap::from([("phase".to_string(), json!("verify"))]),
        );

        assert_eq!(input["inputs"][0], "input.json");
        assert_eq!(input["policy"]["timeout_secs"], 30);
        assert_eq!(input["trace"]["phase"], "verify");
        assert_eq!(
            input["input_artifact_integrity"]["input.json"]["size_bytes"],
            2
        );
        assert_eq!(
            input["input_artifact_integrity"]["input.json"]["sha256"],
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_sha256_is_stable_across_object_key_order() {
        let left = json!({
            "b": [2, 1],
            "a": { "z": true, "m": null }
        });
        let right = json!({
            "a": { "m": null, "z": true },
            "b": [2, 1]
        });

        assert_eq!(json_sha256(&left), json_sha256(&right));
    }

    #[test]
    fn live_node_summaries_track_latest_lifecycle_event_per_node() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![
            test_app_event(
                1,
                "info",
                "node.started",
                base,
                json!({ "node_id": "extract", "kind": "shell", "attempt": 1 }),
            ),
            test_app_event(
                2,
                "info",
                "node.started",
                base + chrono::Duration::seconds(1),
                json!({ "node_id": "lean_check", "kind": "lean", "attempt": 1 }),
            ),
            test_app_event(
                3,
                "error",
                "node.failed",
                base + chrono::Duration::seconds(2),
                json!({
                    "node_id": "extract",
                    "kind": "shell",
                    "attempt": 1,
                    "status": "failed"
                }),
            ),
            test_app_event(
                4,
                "warn",
                "node.retry_scheduled",
                base + chrono::Duration::seconds(3),
                json!({
                    "node_id": "extract",
                    "kind": "shell",
                    "attempt": 1,
                    "next_attempt": 2,
                    "max_attempts": 3,
                    "backoff_ms": 250
                }),
            ),
            test_app_event(
                5,
                "info",
                "node.started",
                base + chrono::Duration::seconds(4),
                json!({ "node_id": "extract", "kind": "shell", "attempt": 2 }),
            ),
            test_app_event(
                6,
                "info",
                "node.completed",
                base + chrono::Duration::seconds(5),
                json!({
                    "node_id": "extract",
                    "kind": "shell",
                    "attempt": 2,
                    "status": "ok"
                }),
            ),
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 2);
        let extract = summaries
            .iter()
            .find(|node| node.node_id == "extract")
            .expect("extract summary");
        assert_eq!(extract.state, "ok");
        assert_eq!(extract.event_type, "node.completed");
        assert_eq!(extract.attempt, 2);
        assert_eq!(extract.node_kind.as_deref(), Some("shell"));
        assert_eq!(extract.status.as_deref(), Some("ok"));
        assert_eq!(extract.event_id, 6);

        let lean_check = summaries
            .iter()
            .find(|node| node.node_id == "lean_check")
            .expect("lean_check summary");
        assert_eq!(lean_check.state, "running");
        assert_eq!(lean_check.event_type, "node.started");
        assert_eq!(lean_check.attempt, 1);
        assert_eq!(lean_check.node_kind.as_deref(), Some("lean"));
    }

    #[test]
    fn live_node_summaries_surface_retry_schedule_until_next_attempt_starts() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![
            test_app_event(
                1,
                "error",
                "node.failed",
                base,
                json!({
                    "node_id": "formalize",
                    "kind": "llm",
                    "attempt": 1,
                    "status": "failed"
                }),
            ),
            test_app_event(
                2,
                "warn",
                "node.retry_scheduled",
                base + chrono::Duration::seconds(1),
                json!({
                    "node_id": "formalize",
                    "kind": "llm",
                    "attempt": 1,
                    "next_attempt": 2,
                    "max_attempts": 3,
                    "backoff_ms": 500
                }),
            ),
            test_app_event(
                3,
                "info",
                "app_log.stderr",
                base + chrono::Duration::seconds(2),
                json!({ "line": "adapter noise" }),
            ),
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "formalize");
        assert_eq!(summaries[0].state, "retry_scheduled");
        assert_eq!(summaries[0].event_type, "node.retry_scheduled");
        assert_eq!(summaries[0].attempt, 1);
        assert_eq!(summaries[0].payload["next_attempt"], 2);
        assert_eq!(summaries[0].payload["backoff_ms"], 500);
    }

    #[test]
    fn live_node_summaries_surface_app_action_events_without_node_id() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![
            test_app_event(
                1,
                "info",
                "app_action.started",
                base,
                json!({
                    "app": "research-review",
                    "action": "review",
                    "dag_type": "review-loop",
                    "status": "started"
                }),
            ),
            test_app_event(
                2,
                "info",
                "app_action.completed",
                base + chrono::Duration::seconds(1),
                json!({
                    "app": "research-review",
                    "action": "review",
                    "dag_type": "review-loop",
                    "status": "completed",
                    "exit_status": 0
                }),
            ),
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "app_action:review");
        assert_eq!(summaries[0].state, "completed");
        assert_eq!(summaries[0].event_type, "app_action.completed");
        assert_eq!(summaries[0].attempt, 1);
        assert_eq!(summaries[0].node_kind.as_deref(), Some("app_action"));
        assert_eq!(summaries[0].status.as_deref(), Some("completed"));
        assert_eq!(summaries[0].event_id, 2);
        assert_eq!(summaries[0].payload["exit_status"], 0);
    }

    #[test]
    fn live_node_summaries_mark_app_action_cancelled_terminal() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![
            test_app_event(
                1,
                "info",
                "app_action.started",
                base,
                json!({
                    "app": "platform-smoke",
                    "action": "cancellation-smoke",
                    "dag_type": "cancellation-smoke",
                    "status": "running"
                }),
            ),
            test_app_event(
                2,
                "warn",
                "app_action.cancelled",
                base + chrono::Duration::seconds(1),
                json!({
                    "app": "platform-smoke",
                    "action": "cancellation-smoke",
                    "dag_type": "cancellation-smoke",
                    "status": "cancelled"
                }),
            ),
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "app_action:cancellation-smoke");
        assert_eq!(summaries[0].state, "cancelled");
        assert_eq!(summaries[0].event_type, "app_action.cancelled");
        assert_eq!(summaries[0].node_kind.as_deref(), Some("app_action"));
        assert_eq!(summaries[0].status.as_deref(), Some("cancelled"));
        assert_eq!(summaries[0].event_id, 2);
    }

    #[test]
    fn live_node_summaries_mark_app_action_awaiting_approval_terminal() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![
            test_app_event(
                1,
                "info",
                "app_action.started",
                base,
                json!({
                    "app": "platform-smoke",
                    "action": "approval-pause-smoke",
                    "dag_type": "approval-pause-smoke",
                    "status": "running"
                }),
            ),
            test_app_event(
                2,
                "info",
                "app_action.awaiting_approval",
                base + chrono::Duration::seconds(1),
                json!({
                    "app": "platform-smoke",
                    "action": "approval-pause-smoke",
                    "dag_type": "approval-pause-smoke",
                    "status": "awaiting_approval",
                    "approved_key": "approval/human_release"
                }),
            ),
        ];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "app_action:approval-pause-smoke");
        assert_eq!(summaries[0].state, "awaiting_approval");
        assert_eq!(summaries[0].event_type, "app_action.awaiting_approval");
        assert_eq!(summaries[0].node_kind.as_deref(), Some("app_action"));
        assert_eq!(summaries[0].status.as_deref(), Some("awaiting_approval"));
        assert_eq!(
            summaries[0].payload["approved_key"],
            "approval/human_release"
        );
        assert_eq!(summaries[0].event_id, 2);
    }

    #[test]
    fn live_node_summaries_prefer_canonical_node_kind_payload() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let events = vec![test_app_event(
            1,
            "info",
            "node.completed",
            base,
            json!({
                "node_id": "lean_check",
                "node_kind": "verify",
                "attempt": 1,
                "status": "ok"
            }),
        )];

        let summaries = summarize_live_node_events(&events);

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].node_id, "lean_check");
        assert_eq!(summaries[0].node_kind.as_deref(), Some("verify"));
        assert_eq!(summaries[0].state, "ok");
    }

    #[test]
    fn dag_execution_event_payload_preserves_envelope_node_id() {
        let run_id = uuid::Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = uuid::Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = uuid::Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("load_pipeline_and_queue".to_string()),
            message: Some("load_pipeline_and_queue ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("attempt".to_string(), json!(1)),
                ("kind".to_string(), json!("prepare_inputs")),
                ("status".to_string(), json!("ok")),
            ]),
        };

        let payload = dag_execution_event_payload(run_id, dag_run_id, Some(lease_id), &event)
            .expect("payload");

        assert_eq!(
            payload["app_run_id"],
            "2d0a1d88-b9f9-4e8f-848e-605b86717330"
        );
        assert_eq!(payload["dag_run_id"], dag_run_id.to_string());
        assert_eq!(payload["lease_id"], lease_id.to_string());
        assert_eq!(payload["node_id"], "load_pipeline_and_queue");
        assert_eq!(payload["attempt"], 1);
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["node_kind"], "prepare_inputs");
        for field in [
            "dag_run_id",
            "tool_id",
            "manifest_hash",
            "artifact_id",
            "lease_id",
            "exit_status",
            "duration_ms",
        ] {
            assert!(
                payload.get(field).is_some(),
                "dag execution event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn live_event_match_key_supports_node_attempt_and_dag_lifecycle_events() {
        let run_id = uuid::Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = uuid::Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = uuid::Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let node_event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("compile_check".to_string()),
            message: Some("compile_check ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("attempt".to_string(), json!(2)),
                ("kind".to_string(), json!("verify")),
                ("status".to_string(), json!("ok")),
            ]),
        };
        let node_payload =
            dag_execution_event_payload(run_id, dag_run_id, Some(lease_id), &node_event)
                .expect("node payload");

        assert_eq!(
            live_event_match_key(&node_event, &node_payload),
            Some(LiveEventMatchKey::Node {
                node_id: "compile_check".to_string(),
                attempt: 2
            })
        );

        let dag_event = agenthero_dag_executor::DagExecutionEvent {
            level: "info".to_string(),
            event_type: "dag.completed".to_string(),
            node_id: None,
            message: Some("c2rust Ok".to_string()),
            payload: std::collections::BTreeMap::from([
                ("dag_type".to_string(), json!("c2rust")),
                ("manifest_hash".to_string(), json!("fnv1a64:test")),
                ("manifest_version".to_string(), json!(1)),
                ("status".to_string(), json!("ok")),
            ]),
        };
        let dag_payload =
            dag_execution_event_payload(run_id, dag_run_id, Some(lease_id), &dag_event)
                .expect("dag payload");

        assert_eq!(
            live_event_match_key(&dag_event, &dag_payload),
            Some(LiveEventMatchKey::Dag {
                dag_type: "c2rust".to_string(),
                manifest_hash: "fnv1a64:test".to_string(),
            })
        );
    }

    #[test]
    fn report_runtime_payload_adds_dag_and_lease_identity() {
        let dag_run_id = uuid::Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = uuid::Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let report = DagExecutionReport {
            dag_type: agenthero_dag_runtime::DagTypeId::new("review-loop"),
            manifest_version: 1,
            manifest_hash: "fnv1a64:test".to_string(),
            status: DagNodeStatus::Ok,
            input: agenthero_dag_executor::DagIo {
                values: std::collections::BTreeMap::from([
                    ("dag_run_id".to_string(), json!(dag_run_id.to_string())),
                    ("lease_id".to_string(), json!(lease_id.to_string())),
                ]),
                artifacts: std::collections::BTreeMap::new(),
            },
            nodes: Vec::new(),
            outputs: agenthero_dag_executor::DagIo::default(),
            events: Vec::new(),
        };

        let payload = report_runtime_payload(Some(&report), json!({ "state": "done" }));

        assert_eq!(payload["state"], "done");
        assert_eq!(payload["dag_run_id"], dag_run_id.to_string());
        assert_eq!(payload["lease_id"], lease_id.to_string());
    }

    #[test]
    fn runtime_event_payload_adds_scheduler_identity_without_report() {
        let dag_run_id = uuid::Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = uuid::Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let payload = runtime_event_payload(
            Some(AppRunRuntimeIdentity {
                dag_run_id,
                lease_id,
            }),
            None,
            json!({ "state": "failed" }),
        );

        assert_eq!(payload["state"], "failed");
        assert_eq!(payload["dag_run_id"], dag_run_id.to_string());
        assert_eq!(payload["lease_id"], lease_id.to_string());
    }

    #[test]
    fn failed_adapter_dag_event_payload_is_terminal_and_traceable() {
        let run_id = uuid::Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let dag_run_id = uuid::Uuid::parse_str("f78c57db-89e3-4b63-8c1a-2c07e3331f0c").unwrap();
        let lease_id = uuid::Uuid::parse_str("a9353847-48b3-472e-b88e-89770fcdbf7a").unwrap();
        let payload = failed_adapter_dag_event_payload(
            run_id,
            AppRunRuntimeIdentity {
                dag_run_id,
                lease_id,
            },
            "failed",
            "adapter_failed",
            "bad adapter args",
            true,
        );

        assert_eq!(payload["app_run_id"], run_id.to_string());
        assert_eq!(payload["dag_run_id"], dag_run_id.to_string());
        assert_eq!(payload["lease_id"], lease_id.to_string());
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["state"], "failed");
        assert_eq!(payload["code"], "adapter_failed");
        assert_eq!(payload["message"], "bad adapter args");
        assert_eq!(payload["retryable"], true);
        for field in [
            "node_id",
            "attempt",
            "node_kind",
            "tool_id",
            "manifest_hash",
            "artifact_id",
            "exit_status",
            "duration_ms",
        ] {
            assert!(
                payload.get(field).is_some(),
                "failed DAG event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    fn test_app_event(
        id: i64,
        level: &str,
        event_type: &str,
        created_at: DateTime<Utc>,
        payload: serde_json::Value,
    ) -> AppRunEvent {
        AppRunEvent {
            id,
            level: level.to_string(),
            event_type: event_type.to_string(),
            message: Some(format!("{event_type} message")),
            payload,
            created_at,
        }
    }

    #[test]
    fn approval_requeue_input_records_key_and_preserves_claim_budget() {
        let mut input = StoredAppRunInput::default();
        input.retry.max_attempts = 1;

        let input = approval_requeue_input(input, "approval/human_release", 1, None);

        assert_eq!(
            input.input.values.get("approval/human_release"),
            Some(&json!(true))
        );
        assert_eq!(input.retry.max_attempts, 2);
        assert!(input.checkpoint.is_none());
    }

    #[test]
    fn approval_resume_requires_checkpoint_when_prior_nodes_completed() {
        assert!(approval_resume_replay_is_safe(0, false));
        assert!(approval_resume_replay_is_safe(1, true));
        assert!(!approval_resume_replay_is_safe(1, false));
    }

    #[test]
    fn approval_checkpoint_accepts_only_paused_declared_approval_key() {
        let checkpoint: DagExecutionReport = serde_json::from_value(json!({
            "dag_type": "review-loop",
            "manifest_version": 1,
            "manifest_hash": "fnv1a64:test",
            "status": "awaiting_approval",
            "input": {"values": {}, "artifacts": {}},
            "nodes": [
                {
                    "node_id": "human_review",
                    "kind": "approval",
                    "status": "awaiting_approval",
                    "policy": {
                        "approval": {
                            "approved_key": "approval/human_release"
                        }
                    }
                }
            ],
            "outputs": {"values": {}, "artifacts": {}},
            "events": []
        }))
        .expect("checkpoint report parses");

        assert!(checkpoint_accepts_approval_key(
            &checkpoint,
            "approval/human_release"
        ));
        assert!(!checkpoint_accepts_approval_key(
            &checkpoint,
            "approval/admin_override"
        ));
    }

    #[test]
    fn approval_checkpoint_accepts_tool_policy_approval_key() {
        let checkpoint: DagExecutionReport = serde_json::from_value(json!({
            "dag_type": "review-loop",
            "manifest_version": 1,
            "manifest_hash": "fnv1a64:test",
            "status": "awaiting_approval",
            "input": {"values": {}, "artifacts": {}},
            "nodes": [
                {
                    "node_id": "publish",
                    "kind": "tool",
                    "status": "awaiting_approval",
                    "tool": "release_gate",
                    "policy": {
                        "tool": {
                            "approval_required": true
                        }
                    }
                }
            ],
            "outputs": {"values": {}, "artifacts": {}},
            "events": []
        }))
        .expect("checkpoint report parses");

        assert!(checkpoint_accepts_approval_key(
            &checkpoint,
            "approval/release_gate"
        ));
        assert!(!checkpoint_accepts_approval_key(
            &checkpoint,
            "release_gate"
        ));
    }

    #[test]
    fn approval_checkpoint_accepts_generic_approval_gate_tool_key() {
        let checkpoint: DagExecutionReport = serde_json::from_value(json!({
            "dag_type": "approval-pause-smoke",
            "manifest_version": 1,
            "manifest_hash": "fnv1a64:test",
            "status": "awaiting_approval",
            "input": {"values": {}, "artifacts": {}},
            "nodes": [
                {
                    "node_id": "wait_for_release",
                    "kind": "tool",
                    "status": "awaiting_approval",
                    "tool": "human_release",
                    "executor": "approval_gate"
                }
            ],
            "outputs": {"values": {}, "artifacts": {}},
            "events": []
        }))
        .expect("checkpoint report parses");

        assert!(checkpoint_accepts_approval_key(
            &checkpoint,
            "approval/human_release"
        ));
        assert!(!checkpoint_accepts_approval_key(
            &checkpoint,
            "approval/admin_override"
        ));
    }
}
