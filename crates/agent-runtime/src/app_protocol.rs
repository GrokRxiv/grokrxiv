//! Process adapter protocol between AgentHero orchestration and DAG apps.

use std::collections::BTreeMap;

use agenthero_dag_executor::{DagExecutionEvent, DagExecutionReport, DagIo};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Current stdin/stdout protocol version for process-backed DAG app adapters.
pub const APP_ADAPTER_PROTOCOL: &str = "agenthero.app.v1";

/// Prefix for structured adapter events written to stderr as JSON lines.
pub const APP_ADAPTER_EVENT_PREFIX: &str = "@@AGENTHERO_EVENT ";

/// Mandatory structured fields every AgentHero event payload exposes for audit and monitoring.
pub const AGENTHERO_EVENT_TRACE_FIELDS: &[&str] = &[
    "app_run_id",
    "dag_run_id",
    "node_id",
    "attempt",
    "node_kind",
    "tool_id",
    "manifest_hash",
    "artifact_id",
    "lease_id",
    "status",
    "exit_status",
    "duration_ms",
];

/// Describe the durable app-run log contract exposed by AgentHero monitor surfaces.
pub fn agenthero_log_contract() -> serde_json::Value {
    json!({
        "format": "durable_text_log_with_agenthero_event_jsonl",
        "structured_event_prefix": APP_ADAPTER_EVENT_PREFIX,
        "tail_parameter": "tail",
        "max_bytes_parameter": "max_bytes",
        "trace_fields": AGENTHERO_EVENT_TRACE_FIELDS,
    })
}

/// Write one structured adapter event to stderr-compatible output.
pub fn write_adapter_event(
    mut writer: impl std::io::Write,
    event: &DagExecutionEvent,
) -> anyhow::Result<()> {
    let event = normalized_adapter_event(event);
    writeln!(
        writer,
        "{}{}",
        APP_ADAPTER_EVENT_PREFIX,
        serde_json::to_string(&event)?
    )?;
    Ok(())
}

/// Build one normalized app-adapter lifecycle event from an adapter request.
pub fn app_adapter_lifecycle_event(
    request: &AppAdapterRequest,
    level: impl Into<String>,
    event_type: impl Into<String>,
    message: impl Into<String>,
    status: impl Into<String>,
    exit_status: Option<i32>,
    mut extra: BTreeMap<String, Value>,
) -> DagExecutionEvent {
    let app_run_id = request_input_string(request, "app_run_id").unwrap_or_default();
    let status = status.into();
    let mut payload = BTreeMap::from([
        ("app".to_string(), json!(request.app)),
        ("action".to_string(), json!(request.action)),
        ("dag_type".to_string(), json!(request.dag_type)),
        ("adapter_protocol".to_string(), json!(request.protocol)),
        ("args_count".to_string(), json!(request.args.len())),
        ("dry_run".to_string(), json!(request.dry_run)),
        ("json".to_string(), json!(request.json)),
        (
            "idempotency_key".to_string(),
            json!(request.idempotency_key),
        ),
        ("status".to_string(), json!(status)),
        ("exit_status".to_string(), json!(exit_status)),
    ]);
    for key in ["app_run_id", "dag_run_id", "lease_id"] {
        if let Some(value) = request_input_string(request, key) {
            payload.insert(key.to_string(), json!(value));
        }
    }
    payload.append(&mut extra);
    let payload = agenthero_trace_payload(
        &app_run_id,
        None,
        serde_json::to_value(payload).unwrap_or_else(|_| json!({})),
    );
    let payload = serde_json::from_value(payload).unwrap_or_default();

    DagExecutionEvent {
        level: level.into(),
        event_type: event_type.into(),
        node_id: None,
        message: Some(message.into()),
        payload,
    }
}

fn request_input_string(request: &AppAdapterRequest, key: &str) -> Option<String> {
    request
        .input
        .values
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Normalize one adapter event before it is emitted on the process protocol.
pub fn normalized_adapter_event(event: &DagExecutionEvent) -> DagExecutionEvent {
    let app_run_id = event
        .payload
        .get("app_run_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let payload = agenthero_event_payload(app_run_id, event);
    let payload = serde_json::from_value(payload).unwrap_or_default();
    DagExecutionEvent {
        level: event.level.clone(),
        event_type: event.event_type.clone(),
        node_id: event.node_id.clone(),
        message: event.message.clone(),
        payload,
    }
}

/// Normalize one execution event payload to the AgentHero audit/monitoring field contract.
pub fn agenthero_event_payload(
    app_run_id: impl ToString,
    event: &DagExecutionEvent,
) -> serde_json::Value {
    let payload = serde_json::to_value(&event.payload).unwrap_or_else(|_| json!({}));
    agenthero_trace_payload(app_run_id, event.node_id.as_deref(), payload)
}

/// Normalize an arbitrary event payload to the AgentHero audit/monitoring field contract.
pub fn agenthero_trace_payload(
    app_run_id: impl ToString,
    envelope_node_id: Option<&str>,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    normalize_agenthero_event_payload(&mut payload, app_run_id.to_string(), envelope_node_id);
    payload
}

fn normalize_agenthero_event_payload(
    payload: &mut Value,
    app_run_id: String,
    envelope_node_id: Option<&str>,
) {
    if !payload.is_object() {
        *payload = json!({});
    }
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("app_run_id".to_string(), json!(app_run_id));
    if let Some(node_id) = envelope_node_id {
        object.insert("node_id".to_string(), json!(node_id));
    }
    insert_alias_or_null(object, "node_kind", &["kind"]);
    insert_alias_or_null(object, "tool_id", &["tool"]);
    insert_alias_or_null(object, "duration_ms", &["latency_ms"]);
    for field in AGENTHERO_EVENT_TRACE_FIELDS {
        object.entry((*field).to_string()).or_insert(Value::Null);
    }
}

fn insert_alias_or_null(object: &mut Map<String, Value>, canonical: &str, aliases: &[&str]) {
    if object.contains_key(canonical) {
        return;
    }
    if let Some(value) = aliases
        .iter()
        .find_map(|alias| object.get(*alias).filter(|value| !value.is_null()).cloned())
    {
        object.insert(canonical.to_string(), value);
    } else {
        object.insert(canonical.to_string(), Value::Null);
    }
}

/// Request sent by the AgentHero orchestrator to a process-backed DAG app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppAdapterRequest {
    /// Protocol marker.
    pub protocol: String,
    /// Product app slug, e.g. `c2rust`.
    pub app: String,
    /// App action id resolved from `app.yaml`.
    pub action: String,
    /// DAG type bound to the action.
    pub dag_type: String,
    /// Action-specific argv after the command path.
    #[serde(default)]
    pub args: Vec<String>,
    /// Initial DAG input values and artifact refs.
    #[serde(default)]
    pub input: DagIo,
    /// Whether the caller requested JSON output.
    #[serde(default)]
    pub json: bool,
    /// Whether the caller requested a plan-only dry run.
    #[serde(default)]
    pub dry_run: bool,
    /// Stable key adapters use to deduplicate retried external side effects.
    #[serde(default)]
    pub idempotency_key: String,
    /// Prior execution report used to replay completed nodes during resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<DagExecutionReport>,
}

impl AppAdapterRequest {
    /// Build a request using the current protocol marker.
    pub fn new(
        app: impl Into<String>,
        action: impl Into<String>,
        dag_type: impl Into<String>,
        args: Vec<String>,
        input: DagIo,
        json: bool,
        dry_run: bool,
    ) -> Self {
        let app = app.into();
        let action = action.into();
        let dag_type = dag_type.into();
        let idempotency_key =
            default_idempotency_key(&app, &action, &dag_type, &args, &input, json, dry_run);
        Self {
            protocol: APP_ADAPTER_PROTOCOL.to_string(),
            app,
            action,
            dag_type,
            args,
            input,
            json,
            dry_run,
            idempotency_key,
            checkpoint: None,
        }
    }

    /// Override the deterministic payload-derived key with a durable scheduler key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = key.into();
        self
    }

    /// Attach a durable DAG execution checkpoint for replay-aware resume.
    pub fn with_checkpoint(mut self, checkpoint: DagExecutionReport) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }
}

fn default_idempotency_key(
    app: &str,
    action: &str,
    dag_type: &str,
    args: &[String],
    input: &DagIo,
    json: bool,
    dry_run: bool,
) -> String {
    let payload = serde_json::json!({
        "protocol": APP_ADAPTER_PROTOCOL,
        "app": app,
        "action": action,
        "dag_type": dag_type,
        "args": args,
        "input": input,
        "json": json,
        "dry_run": dry_run,
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("{APP_ADAPTER_PROTOCOL}:{hex}")
}

/// Response returned by a process-backed DAG app adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppAdapterResponse {
    /// Protocol marker.
    pub protocol: String,
    /// Product app slug.
    pub app: String,
    /// App action id.
    pub action: String,
    /// DAG type executed by the app.
    pub dag_type: String,
    /// True when the action completed successfully.
    pub ok: bool,
    /// Optional machine-readable DAG execution report.
    #[serde(default)]
    pub report: Option<DagExecutionReport>,
    /// Optional app-specific output for actions that are not pure DAG smokes.
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    /// Optional error message when `ok` is false.
    #[serde(default)]
    pub error: Option<String>,
}

impl AppAdapterResponse {
    /// Build a successful response carrying a DAG execution report.
    pub fn ok_report(request: &AppAdapterRequest, report: DagExecutionReport) -> Self {
        Self {
            protocol: APP_ADAPTER_PROTOCOL.to_string(),
            app: request.app.clone(),
            action: request.action.clone(),
            dag_type: request.dag_type.clone(),
            ok: true,
            report: Some(report),
            output: None,
            error: None,
        }
    }

    /// Build a failed response.
    pub fn failed(request: &AppAdapterRequest, error: impl Into<String>) -> Self {
        Self {
            protocol: APP_ADAPTER_PROTOCOL.to_string(),
            app: request.app.clone(),
            action: request.action.clone(),
            dag_type: request.dag_type.clone(),
            ok: false,
            report: None,
            output: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_request_builds_stable_idempotency_key() {
        let request = AppAdapterRequest::new(
            "demo",
            "run",
            "demo-dag",
            vec!["input.c".to_string()],
            DagIo::default(),
            true,
            false,
        );
        let same_request = AppAdapterRequest::new(
            "demo",
            "run",
            "demo-dag",
            vec!["input.c".to_string()],
            DagIo::default(),
            true,
            false,
        );
        let different_request = AppAdapterRequest::new(
            "demo",
            "run",
            "demo-dag",
            vec!["other.c".to_string()],
            DagIo::default(),
            true,
            false,
        );

        assert!(!request.idempotency_key.is_empty());
        assert_eq!(request.idempotency_key, same_request.idempotency_key);
        assert_ne!(request.idempotency_key, different_request.idempotency_key);
    }

    #[test]
    fn adapter_request_can_use_scheduler_idempotency_key() {
        let request = AppAdapterRequest::new(
            "demo",
            "run",
            "demo-dag",
            Vec::new(),
            DagIo::default(),
            true,
            false,
        )
        .with_idempotency_key("app-run:11111111-1111-1111-1111-111111111111");

        assert_eq!(
            request.idempotency_key,
            "app-run:11111111-1111-1111-1111-111111111111"
        );
    }

    #[test]
    fn adapter_request_can_carry_checkpoint() {
        let checkpoint: DagExecutionReport = serde_json::from_value(serde_json::json!({
            "dag_type": "demo-dag",
            "manifest_version": 1,
            "manifest_hash": "fnv1a64:test",
            "status": "awaiting_approval",
            "input": {"values": {}, "artifacts": {}},
            "nodes": [],
            "outputs": {"values": {}, "artifacts": {}},
            "events": []
        }))
        .expect("checkpoint report JSON deserializes");

        let request = AppAdapterRequest::new(
            "demo",
            "run",
            "demo-dag",
            Vec::new(),
            DagIo::default(),
            true,
            false,
        )
        .with_checkpoint(checkpoint.clone());

        assert_eq!(request.checkpoint, Some(checkpoint));
    }

    #[test]
    fn write_adapter_event_normalizes_agenthero_trace_fields() {
        let event = DagExecutionEvent {
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            node_id: Some("verify".to_string()),
            message: Some("verify ok".to_string()),
            payload: std::collections::BTreeMap::from([
                (
                    "app_run_id".to_string(),
                    serde_json::json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
                ),
                (
                    "dag_run_id".to_string(),
                    serde_json::json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
                ),
                ("kind".to_string(), serde_json::json!("verify")),
                ("tool".to_string(), serde_json::json!("lean")),
                ("latency_ms".to_string(), serde_json::json!(42)),
                ("status".to_string(), serde_json::json!("ok")),
            ]),
        };
        let mut bytes = Vec::new();

        write_adapter_event(&mut bytes, &event).expect("adapter event writes");

        let line = String::from_utf8(bytes).expect("adapter event is utf8");
        let payload = line
            .trim_end()
            .strip_prefix(APP_ADAPTER_EVENT_PREFIX)
            .expect("adapter event prefix");
        let emitted: DagExecutionEvent = serde_json::from_str(payload).expect("adapter event JSON");
        assert_eq!(emitted.payload["app_run_id"], event.payload["app_run_id"]);
        assert_eq!(emitted.payload["dag_run_id"], event.payload["dag_run_id"]);
        assert_eq!(emitted.payload["node_id"], serde_json::json!("verify"));
        assert_eq!(emitted.payload["node_kind"], serde_json::json!("verify"));
        assert_eq!(emitted.payload["tool_id"], serde_json::json!("lean"));
        assert_eq!(emitted.payload["duration_ms"], serde_json::json!(42));
        for field in AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                emitted.payload.contains_key(*field),
                "adapter event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn app_adapter_lifecycle_event_carries_request_identity() {
        let mut input = DagIo::default();
        input.values.insert(
            "app_run_id".to_string(),
            serde_json::json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
        );
        input.values.insert(
            "dag_run_id".to_string(),
            serde_json::json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
        );
        input.values.insert(
            "lease_id".to_string(),
            serde_json::json!("a9353847-48b3-472e-b88e-89770fcdbf7a"),
        );
        let request = AppAdapterRequest::new(
            "sample-app",
            "run",
            "sample-dag",
            vec!["--flag".to_string()],
            input,
            true,
            false,
        )
        .with_idempotency_key("app-run:2d0a1d88-b9f9-4e8f-848e-605b86717330");

        let event = app_adapter_lifecycle_event(
            &request,
            "info",
            "app_action.completed",
            "sample-app run completed",
            "completed",
            Some(0),
            std::collections::BTreeMap::from([(
                "stdout_bytes".to_string(),
                serde_json::json!(128),
            )]),
        );
        let mut bytes = Vec::new();
        write_adapter_event(&mut bytes, &event).expect("adapter event writes");
        let line = String::from_utf8(bytes).expect("adapter event is utf8");
        let payload = line
            .trim_end()
            .strip_prefix(APP_ADAPTER_EVENT_PREFIX)
            .expect("adapter event prefix");
        let emitted: DagExecutionEvent = serde_json::from_str(payload).expect("adapter event JSON");

        assert_eq!(emitted.event_type, "app_action.completed");
        assert_eq!(
            emitted.payload["app_run_id"],
            serde_json::json!("2d0a1d88-b9f9-4e8f-848e-605b86717330")
        );
        assert_eq!(
            emitted.payload["dag_run_id"],
            serde_json::json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c")
        );
        assert_eq!(
            emitted.payload["lease_id"],
            serde_json::json!("a9353847-48b3-472e-b88e-89770fcdbf7a")
        );
        assert_eq!(emitted.payload["app"], "sample-app");
        assert_eq!(emitted.payload["action"], "run");
        assert_eq!(emitted.payload["dag_type"], "sample-dag");
        assert_eq!(emitted.payload["status"], "completed");
        assert_eq!(emitted.payload["exit_status"], serde_json::json!(0));
        assert_eq!(emitted.payload["args_count"], serde_json::json!(1));
        assert_eq!(emitted.payload["stdout_bytes"], serde_json::json!(128));
        for field in AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                emitted.payload.contains_key(*field),
                "adapter lifecycle payload should include mandatory AgentHero trace field `{field}`"
            );
        }
    }

    #[test]
    fn agenthero_log_contract_advertises_trace_fields_and_adapter_prefix() {
        let contract = agenthero_log_contract();

        assert_eq!(
            contract["format"],
            serde_json::json!("durable_text_log_with_agenthero_event_jsonl")
        );
        assert_eq!(
            contract["structured_event_prefix"],
            serde_json::json!(APP_ADAPTER_EVENT_PREFIX)
        );
        assert_eq!(contract["tail_parameter"], serde_json::json!("tail"));
        assert_eq!(
            contract["max_bytes_parameter"],
            serde_json::json!("max_bytes")
        );
        for field in AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                contract["trace_fields"]
                    .as_array()
                    .expect("trace fields")
                    .iter()
                    .any(|value| value == field),
                "log contract should advertise mandatory AgentHero trace field `{field}`"
            );
        }
    }
}
