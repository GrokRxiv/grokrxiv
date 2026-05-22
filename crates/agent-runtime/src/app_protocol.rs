//! Process adapter protocol between AgentHero orchestration and DAG apps.

use agenthero_dag_executor::{DagExecutionReport, DagIo};
use serde::{Deserialize, Serialize};

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
    ) -> Self {
        Self {
            protocol: APP_ADAPTER_PROTOCOL.to_string(),
            app: app.into(),
            action: action.into(),
            dag_type: dag_type.into(),
            args,
            input,
            json,
        }
    }
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
