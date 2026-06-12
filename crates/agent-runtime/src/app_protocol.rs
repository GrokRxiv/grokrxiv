//! Process adapter protocol between AgentHero orchestration and DAG apps.

use agenthero_dag_executor::{DagExecutionReport, DagIo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current stdin/stdout protocol version for process-backed DAG app adapters.
pub const APP_ADAPTER_PROTOCOL: &str = "agenthero.app.v1";

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
        }
    }

    /// Override the deterministic payload-derived key with a durable scheduler key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = key.into();
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
}
