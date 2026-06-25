//! AgentHero platform orchestrator.
//!
//! This crate is intentionally app-neutral. Product code lives under
//! `agenthero/apps/<app>/` and is invoked through app manifests plus the
//! adapter protocol.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app_runs;
pub mod cli;
pub mod config;
pub mod dag_apps;
pub mod doctor;
pub mod entrypoint;
pub mod scheduler;
pub mod serve;
pub mod telemetry;

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// Build the generic AgentHero router.
pub fn router() -> axum::Router {
    router_with_state(PlatformState::default())
}

/// Shared state for the generic AgentHero HTTP API.
#[derive(Clone, Default)]
pub struct PlatformState {
    /// Optional platform database pool.
    pub pool: Option<sqlx::PgPool>,
    /// Optional bearer token for private write routes.
    pub service_token: Option<String>,
}

/// Build the generic AgentHero router with explicit state.
pub fn router_with_state(state: PlatformState) -> axum::Router {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .route("/apps", get(apps_index))
        .route("/apps/:app", get(apps_show))
        .route("/apps/:app/actions/:action/runs", post(enqueue_app_run))
        .route("/app-runs", get(app_runs_index))
        .route("/app-runs/:id", get(app_runs_show))
        .route("/app-runs/:id/logs", get(app_run_logs))
        .route("/app-runs/:id/events", get(app_run_events))
        .route("/app-runs/:id/events/stream", get(app_run_events_stream))
        .with_state(state)
}

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

const APP_RUN_METRIC_STATES: &[&str] = &[
    "queued",
    "running",
    "awaiting_approval",
    "partial",
    "done",
    "failed",
    "cancelled",
    "system_failed",
];
const DAG_RUN_METRIC_STATES: &[&str] = APP_RUN_METRIC_STATES;
const WORKER_LEASE_METRIC_STATES: &[&str] = &["leased", "released", "expired", "failed"];
const DAG_RUN_NODE_METRIC_STATES: &[&str] = &[
    "queued",
    "running",
    "awaiting_approval",
    "ok",
    "degraded",
    "skipped",
    "failed",
    "cancelled",
    "system_failed",
];
const DAG_EVENT_METRIC_TYPES: &[&str] = &[
    "node.retry_scheduled",
    "node.failed",
    "node.completed",
    "app_run.lease_expired_requeued",
    "app_run.lease_expired_failed",
];
const DEFAULT_APP_RUN_LOG_TAIL_LINES: usize = 100;
const MAX_APP_RUN_LOG_TAIL_LINES: usize = 5_000;
const DEFAULT_APP_RUN_LOG_TAIL_BYTES: usize = 256 * 1024;
const MAX_APP_RUN_LOG_TAIL_BYTES: usize = 1024 * 1024;

type AppActionStateLabels = (String, String, String);
type AppActionEventLabels = (String, String, String);
type AppDagStateLabels = (String, String, String);
type AppDagEventLabels = (String, String, String);
type AppDagArtifactLabels = (String, String, String);
type NodeIdentityLabels = (String, String, String, String, String);
type NodeStateLabels = (String, String, String, String, String, String);

#[derive(Debug, Default)]
struct MetricSum {
    count: i64,
    sum: i64,
}

async fn apps_index() -> impl IntoResponse {
    match dag_apps::registered_apps() {
        Ok(apps) => Json(json!({ "apps": apps })).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "apps_load_failed", err),
    }
}

async fn apps_show(AxumPath(app): AxumPath<String>) -> impl IntoResponse {
    match dag_apps::registered_app(&app) {
        Ok(Some(app)) => Json(app).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "app_not_found", "app not found"),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "apps_load_failed", err),
    }
}

async fn metrics(State(state): State<PlatformState>) -> impl IntoResponse {
    let text = render_platform_metrics(state.pool.as_ref()).await;
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
}

async fn render_platform_metrics(pool: Option<&sqlx::PgPool>) -> String {
    let mut metrics = PlatformMetrics::default();
    metrics.database_configured = pool.is_some();
    if let Some(pool) = pool {
        metrics.database_query_ok = load_platform_metrics(pool, &mut metrics).await.is_ok();
    }
    format_platform_metrics(&metrics)
}

#[derive(Debug, Default)]
struct PlatformMetrics {
    database_configured: bool,
    database_query_ok: bool,
    app_runs: std::collections::BTreeMap<String, i64>,
    app_runs_by_action: std::collections::BTreeMap<AppActionStateLabels, i64>,
    dag_runs: std::collections::BTreeMap<String, i64>,
    dag_runs_by_app_dag: std::collections::BTreeMap<AppDagStateLabels, i64>,
    worker_leases: std::collections::BTreeMap<String, i64>,
    dag_run_nodes: std::collections::BTreeMap<String, i64>,
    dag_run_nodes_by_node: std::collections::BTreeMap<NodeStateLabels, i64>,
    dag_run_node_latency_ms_by_node: std::collections::BTreeMap<NodeIdentityLabels, MetricSum>,
    dag_node_retries_by_node: std::collections::BTreeMap<NodeIdentityLabels, i64>,
    dag_events_by_type: std::collections::BTreeMap<String, i64>,
    dag_events_by_app_action: std::collections::BTreeMap<AppActionEventLabels, i64>,
    dag_events_by_app_dag: std::collections::BTreeMap<AppDagEventLabels, i64>,
    dag_run_node_latency_ms_count: i64,
    dag_run_node_latency_ms_sum: i64,
    dag_events_total: i64,
    dag_artifact_bytes_total: i64,
    dag_artifact_bytes_by_app_dag: std::collections::BTreeMap<AppDagArtifactLabels, i64>,
}

async fn load_platform_metrics(
    pool: &sqlx::PgPool,
    metrics: &mut PlatformMetrics,
) -> anyhow::Result<()> {
    for row in sqlx::query("select state, count(*)::bigint as count from app_runs group by state")
        .fetch_all(pool)
        .await?
    {
        metrics
            .app_runs
            .insert(row.get::<String, _>("state"), row.get("count"));
    }
    for row in sqlx::query(
        "select app_id, action_id, state, count(*)::bigint as count \
         from app_runs group by app_id, action_id, state",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.app_runs_by_action.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("action_id"),
                row.get::<String, _>("state"),
            ),
            row.get("count"),
        );
    }
    for row in sqlx::query("select state, count(*)::bigint as count from dag_runs group by state")
        .fetch_all(pool)
        .await?
    {
        metrics
            .dag_runs
            .insert(row.get::<String, _>("state"), row.get("count"));
    }
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, dr.dag_type, dr.state, count(*)::bigint as count \
         from dag_runs dr \
         left join app_runs ar on ar.id = dr.app_run_id \
         group by coalesce(ar.app_id, ''), dr.dag_type, dr.state",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_runs_by_app_dag.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("state"),
            ),
            row.get("count"),
        );
    }
    for row in
        sqlx::query("select state, count(*)::bigint as count from worker_leases group by state")
            .fetch_all(pool)
            .await?
    {
        metrics
            .worker_leases
            .insert(row.get::<String, _>("state"), row.get("count"));
    }
    for row in
        sqlx::query("select state, count(*)::bigint as count from dag_run_nodes group by state")
            .fetch_all(pool)
            .await?
    {
        metrics
            .dag_run_nodes
            .insert(row.get::<String, _>("state"), row.get("count"));
    }
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                dr.dag_type, \
                drn.node_id, \
                drn.node_kind, \
                coalesce(drn.tool, '') as tool_id, \
                drn.state, \
                count(*)::bigint as count \
         from dag_run_nodes drn \
         join dag_runs dr on dr.id = drn.dag_run_id \
         left join app_runs ar on ar.id = dr.app_run_id \
         group by coalesce(ar.app_id, ''), dr.dag_type, drn.node_id, drn.node_kind, coalesce(drn.tool, ''), drn.state",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_run_nodes_by_node.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("node_id"),
                row.get::<String, _>("node_kind"),
                row.get::<String, _>("tool_id"),
                row.get::<String, _>("state"),
            ),
            row.get("count"),
        );
    }
    for row in sqlx::query(
        "select event_type, count(*)::bigint as count from dag_events group by event_type",
    )
    .fetch_all(pool)
    .await?
    {
        metrics
            .dag_events_by_type
            .insert(row.get::<String, _>("event_type"), row.get("count"));
    }
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                coalesce(dr.dag_type, '') as dag_type, \
                de.event_type, \
                count(*)::bigint as count \
         from dag_events de \
         left join dag_runs dr on dr.id = de.dag_run_id \
         left join app_runs ar on ar.id = coalesce(de.app_run_id, dr.app_run_id) \
         group by coalesce(ar.app_id, ''), coalesce(dr.dag_type, ''), de.event_type",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_events_by_app_dag.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("event_type"),
            ),
            row.get("count"),
        );
    }
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                coalesce(ar.action_id, '') as action_id, \
                de.event_type, \
                count(*)::bigint as count \
         from dag_events de \
         left join dag_runs dr on dr.id = de.dag_run_id \
         left join app_runs ar on ar.id = coalesce(de.app_run_id, dr.app_run_id) \
         group by coalesce(ar.app_id, ''), coalesce(ar.action_id, ''), de.event_type",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_events_by_app_action.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("action_id"),
                row.get::<String, _>("event_type"),
            ),
            row.get("count"),
        );
    }
    let latency_row = sqlx::query(
        "select count(latency_ms)::bigint as count, coalesce(sum(latency_ms), 0)::bigint as sum \
         from dag_run_nodes",
    )
    .fetch_one(pool)
    .await?;
    metrics.dag_run_node_latency_ms_count = latency_row.get("count");
    metrics.dag_run_node_latency_ms_sum = latency_row.get("sum");
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                dr.dag_type, \
                drn.node_id, \
                drn.node_kind, \
                coalesce(drn.tool, '') as tool_id, \
                count(drn.latency_ms)::bigint as count, \
                coalesce(sum(drn.latency_ms), 0)::bigint as sum \
         from dag_run_nodes drn \
         join dag_runs dr on dr.id = drn.dag_run_id \
         left join app_runs ar on ar.id = dr.app_run_id \
         where drn.latency_ms is not null \
         group by coalesce(ar.app_id, ''), dr.dag_type, drn.node_id, drn.node_kind, coalesce(drn.tool, '')",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_run_node_latency_ms_by_node.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("node_id"),
                row.get::<String, _>("node_kind"),
                row.get::<String, _>("tool_id"),
            ),
            MetricSum {
                count: row.get("count"),
                sum: row.get("sum"),
            },
        );
    }
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                coalesce(dr.dag_type, '') as dag_type, \
                coalesce(drn.node_id, de.payload->>'node_id', '') as node_id, \
                coalesce(drn.node_kind, de.payload->>'node_kind', '') as node_kind, \
                coalesce(drn.tool, de.payload->>'tool_id', de.payload->>'tool', '') as tool_id, \
                count(*)::bigint as count \
         from dag_events de \
         left join dag_runs dr on dr.id = de.dag_run_id \
         left join app_runs ar on ar.id = coalesce(de.app_run_id, dr.app_run_id) \
         left join dag_run_nodes drn on drn.id = de.node_run_id \
         where de.event_type = 'node.retry_scheduled' \
         group by coalesce(ar.app_id, ''), \
                  coalesce(dr.dag_type, ''), \
                  coalesce(drn.node_id, de.payload->>'node_id', ''), \
                  coalesce(drn.node_kind, de.payload->>'node_kind', ''), \
                  coalesce(drn.tool, de.payload->>'tool_id', de.payload->>'tool', '')",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_node_retries_by_node.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("node_id"),
                row.get::<String, _>("node_kind"),
                row.get::<String, _>("tool_id"),
            ),
            row.get("count"),
        );
    }
    metrics.dag_events_total = sqlx::query_scalar("select count(*)::bigint from dag_events")
        .fetch_one(pool)
        .await?;
    metrics.dag_artifact_bytes_total =
        sqlx::query_scalar("select coalesce(sum(size_bytes), 0)::bigint from dag_artifacts")
            .fetch_one(pool)
            .await?;
    for row in sqlx::query(
        "select coalesce(ar.app_id, '') as app_id, \
                coalesce(dr.dag_type, '') as dag_type, \
                da.name as artifact_name, \
                coalesce(sum(da.size_bytes), 0)::bigint as bytes \
         from dag_artifacts da \
         left join dag_runs dr on dr.id = da.dag_run_id \
         left join app_runs ar on ar.id = coalesce(da.app_run_id, dr.app_run_id) \
         group by coalesce(ar.app_id, ''), coalesce(dr.dag_type, ''), da.name",
    )
    .fetch_all(pool)
    .await?
    {
        metrics.dag_artifact_bytes_by_app_dag.insert(
            (
                row.get::<String, _>("app_id"),
                row.get::<String, _>("dag_type"),
                row.get::<String, _>("artifact_name"),
            ),
            row.get("bytes"),
        );
    }
    Ok(())
}

fn format_platform_metrics(metrics: &PlatformMetrics) -> String {
    let mut out = String::new();
    out.push_str(
        "# HELP agenthero_database_configured Whether AgentHero has a configured database pool.\n",
    );
    out.push_str("# TYPE agenthero_database_configured gauge\n");
    out.push_str(&format!(
        "agenthero_database_configured {}\n",
        bool_metric(metrics.database_configured)
    ));
    out.push_str("# HELP agenthero_database_query_ok Whether the latest metrics database queries succeeded.\n");
    out.push_str("# TYPE agenthero_database_query_ok gauge\n");
    out.push_str(&format!(
        "agenthero_database_query_ok {}\n",
        bool_metric(metrics.database_query_ok)
    ));
    out.push_str("# HELP agenthero_app_runs Current app runs by durable state.\n");
    out.push_str("# TYPE agenthero_app_runs gauge\n");
    for state in APP_RUN_METRIC_STATES {
        out.push_str(&format!(
            "agenthero_app_runs{{state=\"{}\"}} {}\n",
            prometheus_label_value(state),
            metrics.app_runs.get(*state).copied().unwrap_or_default()
        ));
    }
    out.push_str(
        "# HELP agenthero_app_runs_by_action Current app runs by app, action, and durable state.\n",
    );
    out.push_str("# TYPE agenthero_app_runs_by_action gauge\n");
    for ((app, action, state), count) in &metrics.app_runs_by_action {
        out.push_str(&format!(
            "agenthero_app_runs_by_action{{app=\"{}\",action=\"{}\",state=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(action),
            prometheus_label_value(state),
            count
        ));
    }
    out.push_str("# HELP agenthero_dag_runs Current DAG runs by durable state.\n");
    out.push_str("# TYPE agenthero_dag_runs gauge\n");
    for state in DAG_RUN_METRIC_STATES {
        out.push_str(&format!(
            "agenthero_dag_runs{{state=\"{}\"}} {}\n",
            prometheus_label_value(state),
            metrics.dag_runs.get(*state).copied().unwrap_or_default()
        ));
    }
    out.push_str("# HELP agenthero_dag_runs_by_app_dag Current DAG runs by app, DAG type, and durable state.\n");
    out.push_str("# TYPE agenthero_dag_runs_by_app_dag gauge\n");
    for ((app, dag_type, state), count) in &metrics.dag_runs_by_app_dag {
        out.push_str(&format!(
            "agenthero_dag_runs_by_app_dag{{app=\"{}\",dag_type=\"{}\",state=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(state),
            count
        ));
    }
    out.push_str("# HELP agenthero_worker_leases Current worker leases by durable state.\n");
    out.push_str("# TYPE agenthero_worker_leases gauge\n");
    for state in WORKER_LEASE_METRIC_STATES {
        out.push_str(&format!(
            "agenthero_worker_leases{{state=\"{}\"}} {}\n",
            prometheus_label_value(state),
            metrics
                .worker_leases
                .get(*state)
                .copied()
                .unwrap_or_default()
        ));
    }
    out.push_str("# HELP agenthero_dag_run_nodes Current DAG run nodes by durable state.\n");
    out.push_str("# TYPE agenthero_dag_run_nodes gauge\n");
    for state in DAG_RUN_NODE_METRIC_STATES {
        out.push_str(&format!(
            "agenthero_dag_run_nodes{{state=\"{}\"}} {}\n",
            prometheus_label_value(state),
            metrics
                .dag_run_nodes
                .get(*state)
                .copied()
                .unwrap_or_default()
        ));
    }
    out.push_str("# HELP agenthero_dag_run_nodes_by_node Current DAG run nodes by app, DAG type, node, kind, tool, and durable state.\n");
    out.push_str("# TYPE agenthero_dag_run_nodes_by_node gauge\n");
    for ((app, dag_type, node_id, node_kind, tool_id, state), count) in
        &metrics.dag_run_nodes_by_node
    {
        out.push_str(&format!(
            "agenthero_dag_run_nodes_by_node{{app=\"{}\",dag_type=\"{}\",node_id=\"{}\",node_kind=\"{}\",tool_id=\"{}\",state=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(node_id),
            prometheus_label_value(node_kind),
            prometheus_label_value(tool_id),
            prometheus_label_value(state),
            count
        ));
    }
    out.push_str("# HELP agenthero_dag_run_node_latency_ms_count Nodes with recorded latency.\n");
    out.push_str("# TYPE agenthero_dag_run_node_latency_ms_count gauge\n");
    out.push_str(&format!(
        "agenthero_dag_run_node_latency_ms_count {}\n",
        metrics.dag_run_node_latency_ms_count
    ));
    out.push_str("# HELP agenthero_dag_run_node_latency_ms_sum Sum of recorded node latency in milliseconds.\n");
    out.push_str("# TYPE agenthero_dag_run_node_latency_ms_sum gauge\n");
    out.push_str(&format!(
        "agenthero_dag_run_node_latency_ms_sum {}\n",
        metrics.dag_run_node_latency_ms_sum
    ));
    out.push_str("# HELP agenthero_dag_run_node_latency_ms_by_node_count Nodes with recorded latency by app, DAG type, node, kind, and tool.\n");
    out.push_str("# TYPE agenthero_dag_run_node_latency_ms_by_node_count gauge\n");
    for ((app, dag_type, node_id, node_kind, tool_id), latency) in
        &metrics.dag_run_node_latency_ms_by_node
    {
        out.push_str(&format!(
            "agenthero_dag_run_node_latency_ms_by_node_count{{app=\"{}\",dag_type=\"{}\",node_id=\"{}\",node_kind=\"{}\",tool_id=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(node_id),
            prometheus_label_value(node_kind),
            prometheus_label_value(tool_id),
            latency.count
        ));
    }
    out.push_str("# HELP agenthero_dag_run_node_latency_ms_by_node_sum Sum of recorded node latency by app, DAG type, node, kind, and tool.\n");
    out.push_str("# TYPE agenthero_dag_run_node_latency_ms_by_node_sum gauge\n");
    for ((app, dag_type, node_id, node_kind, tool_id), latency) in
        &metrics.dag_run_node_latency_ms_by_node
    {
        out.push_str(&format!(
            "agenthero_dag_run_node_latency_ms_by_node_sum{{app=\"{}\",dag_type=\"{}\",node_id=\"{}\",node_kind=\"{}\",tool_id=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(node_id),
            prometheus_label_value(node_kind),
            prometheus_label_value(tool_id),
            latency.sum
        ));
    }
    out.push_str("# HELP agenthero_dag_node_retries_by_node Node retry schedules by app, DAG type, node, kind, and tool.\n");
    out.push_str("# TYPE agenthero_dag_node_retries_by_node gauge\n");
    for ((app, dag_type, node_id, node_kind, tool_id), count) in &metrics.dag_node_retries_by_node {
        out.push_str(&format!(
            "agenthero_dag_node_retries_by_node{{app=\"{}\",dag_type=\"{}\",node_id=\"{}\",node_kind=\"{}\",tool_id=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(node_id),
            prometheus_label_value(node_kind),
            prometheus_label_value(tool_id),
            count
        ));
    }
    out.push_str("# HELP agenthero_dag_events_total Total durable DAG/app events recorded.\n");
    out.push_str("# TYPE agenthero_dag_events_total gauge\n");
    out.push_str(&format!(
        "agenthero_dag_events_total {}\n",
        metrics.dag_events_total
    ));
    out.push_str("# HELP agenthero_dag_events Durable DAG/app events by event type.\n");
    out.push_str("# TYPE agenthero_dag_events gauge\n");
    for event_type in DAG_EVENT_METRIC_TYPES {
        out.push_str(&format!(
            "agenthero_dag_events{{event_type=\"{}\"}} {}\n",
            prometheus_label_value(event_type),
            metrics
                .dag_events_by_type
                .get(*event_type)
                .copied()
                .unwrap_or_default()
        ));
    }
    out.push_str("# HELP agenthero_dag_events_by_app_dag Durable DAG/app events by app, DAG type, and event type.\n");
    out.push_str("# TYPE agenthero_dag_events_by_app_dag gauge\n");
    for ((app, dag_type, event_type), count) in &metrics.dag_events_by_app_dag {
        out.push_str(&format!(
            "agenthero_dag_events_by_app_dag{{app=\"{}\",dag_type=\"{}\",event_type=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(event_type),
            count
        ));
    }
    out.push_str("# HELP agenthero_dag_events_by_app_action Durable DAG/app events by app, action, and event type.\n");
    out.push_str("# TYPE agenthero_dag_events_by_app_action gauge\n");
    for ((app, action, event_type), count) in &metrics.dag_events_by_app_action {
        out.push_str(&format!(
            "agenthero_dag_events_by_app_action{{app=\"{}\",action=\"{}\",event_type=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(action),
            prometheus_label_value(event_type),
            count
        ));
    }
    out.push_str(
        "# HELP agenthero_dag_artifact_bytes_total Total bytes recorded for durable artifacts.\n",
    );
    out.push_str("# TYPE agenthero_dag_artifact_bytes_total gauge\n");
    out.push_str(&format!(
        "agenthero_dag_artifact_bytes_total {}\n",
        metrics.dag_artifact_bytes_total
    ));
    out.push_str("# HELP agenthero_dag_artifact_bytes_by_app_dag Durable artifact bytes by app, DAG type, and artifact name.\n");
    out.push_str("# TYPE agenthero_dag_artifact_bytes_by_app_dag gauge\n");
    for ((app, dag_type, artifact_name), bytes) in &metrics.dag_artifact_bytes_by_app_dag {
        out.push_str(&format!(
            "agenthero_dag_artifact_bytes_by_app_dag{{app=\"{}\",dag_type=\"{}\",artifact_name=\"{}\"}} {}\n",
            prometheus_label_value(app),
            prometheus_label_value(dag_type),
            prometheus_label_value(artifact_name),
            bytes
        ));
    }
    out
}

fn bool_metric(value: bool) -> i32 {
    if value {
        1
    } else {
        0
    }
}

fn prometheus_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_metrics_include_live_dag_run_state_counts() {
        let mut metrics = PlatformMetrics::default();
        metrics.dag_runs.insert("running".to_string(), 2);
        metrics.dag_runs.insert("awaiting_approval".to_string(), 1);

        let text = format_platform_metrics(&metrics);

        assert!(text.contains("# HELP agenthero_dag_runs Current DAG runs by durable state."));
        assert!(text.contains("# TYPE agenthero_dag_runs gauge"));
        assert!(text.contains("agenthero_dag_runs{state=\"running\"} 2"));
        assert!(text.contains("agenthero_dag_runs{state=\"awaiting_approval\"} 1"));
        assert!(text.contains("agenthero_dag_runs{state=\"done\"} 0"));
    }

    #[test]
    fn platform_metrics_include_app_and_dag_label_dimensions() {
        let mut metrics = PlatformMetrics::default();
        metrics.app_runs_by_action.insert(
            (
                "formal-proofs".to_string(),
                "open-problem-search".to_string(),
                "running".to_string(),
            ),
            2,
        );
        metrics.dag_runs_by_app_dag.insert(
            (
                "formal-proofs".to_string(),
                "open-problem-search".to_string(),
                "running".to_string(),
            ),
            2,
        );
        metrics.app_runs_by_action.insert(
            (
                "quotes\"and\\slashes".to_string(),
                "line\nbreak".to_string(),
                "awaiting_approval".to_string(),
            ),
            1,
        );

        let text = format_platform_metrics(&metrics);

        assert!(text.contains("# HELP agenthero_app_runs_by_action Current app runs by app, action, and durable state."));
        assert!(text.contains("# TYPE agenthero_app_runs_by_action gauge"));
        assert!(text.contains("agenthero_app_runs_by_action{app=\"formal-proofs\",action=\"open-problem-search\",state=\"running\"} 2"));
        assert!(text.contains("# HELP agenthero_dag_runs_by_app_dag Current DAG runs by app, DAG type, and durable state."));
        assert!(text.contains("# TYPE agenthero_dag_runs_by_app_dag gauge"));
        assert!(text.contains("agenthero_dag_runs_by_app_dag{app=\"formal-proofs\",dag_type=\"open-problem-search\",state=\"running\"} 2"));
        assert!(text.contains(
            "agenthero_app_runs_by_action{app=\"quotes\\\"and\\\\slashes\",action=\"line\\nbreak\",state=\"awaiting_approval\"} 1"
        ));
    }

    #[test]
    fn platform_metrics_include_node_label_dimensions() {
        let mut metrics = PlatformMetrics::default();
        metrics.dag_run_nodes_by_node.insert(
            (
                "sample-app".to_string(),
                "sample-dag".to_string(),
                "extract_items".to_string(),
                "llm".to_string(),
                "reviewer".to_string(),
                "running".to_string(),
            ),
            3,
        );

        let text = format_platform_metrics(&metrics);

        assert!(text.contains("# HELP agenthero_dag_run_nodes_by_node Current DAG run nodes by app, DAG type, node, kind, tool, and durable state."));
        assert!(text.contains("# TYPE agenthero_dag_run_nodes_by_node gauge"));
        assert!(text.contains("agenthero_dag_run_nodes_by_node{app=\"sample-app\",dag_type=\"sample-dag\",node_id=\"extract_items\",node_kind=\"llm\",tool_id=\"reviewer\",state=\"running\"} 3"));
    }

    #[test]
    fn platform_metrics_include_event_and_artifact_label_dimensions() {
        let mut metrics = PlatformMetrics::default();
        metrics.dag_events_by_app_dag.insert(
            (
                "sample-app".to_string(),
                "sample-dag".to_string(),
                "node.completed".to_string(),
            ),
            12,
        );
        metrics.dag_events_by_app_action.insert(
            (
                "sample-app".to_string(),
                "review".to_string(),
                "app_action.completed".to_string(),
            ),
            4,
        );
        metrics.dag_artifact_bytes_by_app_dag.insert(
            (
                "sample-app".to_string(),
                "sample-dag".to_string(),
                "result_report".to_string(),
            ),
            4096,
        );

        let text = format_platform_metrics(&metrics);

        assert!(text.contains("# HELP agenthero_dag_events_by_app_dag Durable DAG/app events by app, DAG type, and event type."));
        assert!(text.contains("# TYPE agenthero_dag_events_by_app_dag gauge"));
        assert!(text.contains("agenthero_dag_events_by_app_dag{app=\"sample-app\",dag_type=\"sample-dag\",event_type=\"node.completed\"} 12"));
        assert!(text.contains("# HELP agenthero_dag_events_by_app_action Durable DAG/app events by app, action, and event type."));
        assert!(text.contains("# TYPE agenthero_dag_events_by_app_action gauge"));
        assert!(text.contains("agenthero_dag_events_by_app_action{app=\"sample-app\",action=\"review\",event_type=\"app_action.completed\"} 4"));
        assert!(text.contains("# HELP agenthero_dag_artifact_bytes_by_app_dag Durable artifact bytes by app, DAG type, and artifact name."));
        assert!(text.contains("# TYPE agenthero_dag_artifact_bytes_by_app_dag gauge"));
        assert!(text.contains("agenthero_dag_artifact_bytes_by_app_dag{app=\"sample-app\",dag_type=\"sample-dag\",artifact_name=\"result_report\"} 4096"));
    }

    #[test]
    fn platform_metrics_include_node_latency_and_retry_dimensions() {
        let mut metrics = PlatformMetrics::default();
        metrics.dag_run_node_latency_ms_by_node.insert(
            (
                "sample-app".to_string(),
                "sample-dag".to_string(),
                "compile_check".to_string(),
                "verifier".to_string(),
                "compiler".to_string(),
            ),
            MetricSum {
                count: 2,
                sum: 1_500,
            },
        );
        metrics.dag_node_retries_by_node.insert(
            (
                "sample-app".to_string(),
                "sample-dag".to_string(),
                "compile_check".to_string(),
                "verifier".to_string(),
                "compiler".to_string(),
            ),
            4,
        );

        let text = format_platform_metrics(&metrics);

        assert!(text.contains("# HELP agenthero_dag_run_node_latency_ms_by_node_count Nodes with recorded latency by app, DAG type, node, kind, and tool."));
        assert!(text.contains("# TYPE agenthero_dag_run_node_latency_ms_by_node_count gauge"));
        assert!(text.contains("agenthero_dag_run_node_latency_ms_by_node_count{app=\"sample-app\",dag_type=\"sample-dag\",node_id=\"compile_check\",node_kind=\"verifier\",tool_id=\"compiler\"} 2"));
        assert!(text.contains("# HELP agenthero_dag_run_node_latency_ms_by_node_sum Sum of recorded node latency by app, DAG type, node, kind, and tool."));
        assert!(text.contains("# TYPE agenthero_dag_run_node_latency_ms_by_node_sum gauge"));
        assert!(text.contains("agenthero_dag_run_node_latency_ms_by_node_sum{app=\"sample-app\",dag_type=\"sample-dag\",node_id=\"compile_check\",node_kind=\"verifier\",tool_id=\"compiler\"} 1500"));
        assert!(text.contains("# HELP agenthero_dag_node_retries_by_node Node retry schedules by app, DAG type, node, kind, and tool."));
        assert!(text.contains("# TYPE agenthero_dag_node_retries_by_node gauge"));
        assert!(text.contains("agenthero_dag_node_retries_by_node{app=\"sample-app\",dag_type=\"sample-dag\",node_id=\"compile_check\",node_kind=\"verifier\",tool_id=\"compiler\"} 4"));
    }

    #[test]
    fn app_run_log_tail_reads_bounded_recent_lines() {
        let path = std::env::temp_dir().join(format!("agenthero-log-tail-{}.log", Uuid::new_v4()));
        std::fs::write(&path, "line-1\nline-2\nline-3\nline-4\n").expect("write test log");

        let tail = read_app_run_log_tail(&path, 2, 16).expect("read log tail");
        let _ = std::fs::remove_file(&path);

        assert_eq!(tail, "line-3\nline-4\n");
    }

    #[test]
    fn app_run_log_tail_honors_byte_bound() {
        let path =
            std::env::temp_dir().join(format!("agenthero-log-tail-bytes-{}.log", Uuid::new_v4()));
        std::fs::write(&path, "old-1\nold-2\nnew-1\nnew-2\n").expect("write test log");

        let tail = read_app_run_log_tail(&path, 10, 12).expect("read log tail");
        let _ = std::fs::remove_file(&path);

        assert!(!tail.contains("old-1"));
        assert!(!tail.contains("old-2"));
        assert!(tail.contains("new-1"));
        assert!(tail.contains("new-2"));
    }
}

async fn enqueue_app_run(
    State(state): State<PlatformState>,
    headers: HeaderMap,
    AxumPath((app, action)): AxumPath<(String, String)>,
    Json(body): Json<app_runs::AppRunRequest>,
) -> impl IntoResponse {
    if let Err(response) = authorize_write(&state, &headers) {
        return response;
    }
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    if let Err(err) = dag_apps::app_action_binding(&app, &action) {
        return error_response(StatusCode::NOT_FOUND, "app_action_not_found", err);
    }
    match app_runs::insert_queued(pool, &app, &action, body).await {
        Ok(run_id) => (
            StatusCode::ACCEPTED,
            Json(json!({ "run_id": run_id, "state": "queued" })),
        )
            .into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "enqueue_failed", err),
    }
}

#[derive(Debug, Deserialize)]
struct AppRunsQuery {
    app: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AppRunEventsQuery {
    after_id: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AppRunLogsQuery {
    tail: Option<usize>,
    max_bytes: Option<usize>,
}

async fn app_runs_index(
    State(state): State<PlatformState>,
    Query(query): Query<AppRunsQuery>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "database_unconfigured",
                "detail": "DATABASE_URL is unset",
                "list_contract": app_run_list_contract_json(),
            })),
        )
            .into_response();
    };
    match app_runs::list_run_items(
        pool,
        query.app.as_deref(),
        query.state.as_deref(),
        query.limit.unwrap_or(50),
    )
    .await
    {
        Ok(runs) => Json(json!({ "runs": runs })).into_response(),
        Err(err) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_runs_load_failed",
            err,
        ),
    }
}

fn app_run_list_contract_json() -> serde_json::Value {
    json!({
        "item": "AppRunListItem",
        "shape": "flattened AppRunRecord plus observability",
        "observability": {
            "event_count": "durable dag_events rows for this app run",
            "log_exists": "whether the durable app-run log file exists",
            "log_bytes": "durable app-run log file size in bytes when readable",
            "links": {
                "logs_path": "/app-runs/<run_id>/logs",
                "events_path": "/app-runs/<run_id>/events",
                "event_stream_path": "/app-runs/<run_id>/events/stream",
                "metrics_path": "/metrics",
            }
        }
    })
}

async fn app_runs_show(
    State(state): State<PlatformState>,
    AxumPath(id): AxumPath<Uuid>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unconfigured",
            "DATABASE_URL is unset",
        );
    };
    match app_runs::get_run_detail(pool, id).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "app_run_not_found", "run not found"),
        Err(err) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "app_run_load_failed",
            err,
        ),
    }
}

async fn app_run_logs(
    AxumPath(id): AxumPath<Uuid>,
    Query(query): Query<AppRunLogsQuery>,
) -> impl IntoResponse {
    let tail_lines = query
        .tail
        .unwrap_or(DEFAULT_APP_RUN_LOG_TAIL_LINES)
        .clamp(1, MAX_APP_RUN_LOG_TAIL_LINES);
    let max_bytes = query
        .max_bytes
        .unwrap_or(DEFAULT_APP_RUN_LOG_TAIL_BYTES)
        .clamp(1, MAX_APP_RUN_LOG_TAIL_BYTES);
    let log_path = dag_apps::app_run_log_path(id);
    let exists = log_path.is_file();
    let tail = if exists {
        match read_app_run_log_tail(&log_path, tail_lines, max_bytes) {
            Ok(tail) => tail,
            Err(err) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "log_read_failed", err);
            }
        }
    } else {
        String::new()
    };

    Json(json!({
        "run_id": id,
        "log_path": log_path.to_string_lossy(),
        "exists": exists,
        "tail_lines": tail_lines,
        "max_bytes": max_bytes,
        "log_contract": agenthero_agent_runtime::agenthero_log_contract(),
        "tail": tail,
    }))
    .into_response()
}

async fn app_run_events(
    State(state): State<PlatformState>,
    AxumPath(id): AxumPath<Uuid>,
    Query(query): Query<AppRunEventsQuery>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "database_unconfigured",
                "detail": "DATABASE_URL is unset",
                "event_contract": event_contract_json(),
            })),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 5_000);
    let result = if let Some(after_id) = query.after_id {
        app_runs::list_events_after(pool, id, after_id, limit).await
    } else {
        app_runs::list_events_limited(pool, id, limit).await
    };
    match result {
        Ok(events) => {
            let last_event_id = events.iter().map(|event| event.id).max();
            Json(json!({
                "run_id": id,
                "events": events,
                "event_contract": event_contract_json(),
                "cursor": {
                    "after_id": query.after_id,
                    "last_event_id": last_event_id,
                    "limit": limit,
                }
            }))
            .into_response()
        }
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "events_load_failed", err),
    }
}

async fn app_run_events_stream(
    State(state): State<PlatformState>,
    AxumPath(id): AxumPath<Uuid>,
    Query(query): Query<AppRunEventsQuery>,
) -> impl IntoResponse {
    let Some(pool) = state.pool.as_ref() else {
        return sse_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format_sse_event(
                "agenthero.error",
                None,
                json!({
                    "error": "database_unconfigured",
                    "detail": "DATABASE_URL is unset",
                    "event_contract": event_contract_json(),
                    "stream_contract": stream_contract_json(),
                }),
            ),
        );
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 5_000);
    let result = if let Some(after_id) = query.after_id {
        app_runs::list_events_after(pool, id, after_id, limit).await
    } else {
        app_runs::list_events_limited(pool, id, limit).await
    };
    match result {
        Ok(events) => {
            let last_event_id = events.iter().map(|event| event.id).max();
            let mut body = format_sse_event(
                "agenthero.event_contract",
                None,
                json!({
                    "run_id": id,
                    "event_contract": event_contract_json(),
                    "stream_contract": stream_contract_json(),
                    "cursor": {
                        "after_id": query.after_id,
                        "last_event_id": last_event_id,
                        "limit": limit,
                    }
                }),
            );
            for event in events {
                body.push_str(&format_sse_event(
                    &event.event_type,
                    Some(event.id),
                    serde_json::to_value(&event).unwrap_or_else(|_| json!({})),
                ));
            }
            sse_response(StatusCode::OK, body)
        }
        Err(err) => sse_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format_sse_event(
                "agenthero.error",
                None,
                json!({
                    "error": "events_load_failed",
                    "detail": err.to_string(),
                    "event_contract": event_contract_json(),
                    "stream_contract": stream_contract_json(),
                }),
            ),
        ),
    }
}

fn event_contract_json() -> serde_json::Value {
    json!({
        "trace_fields": agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS,
    })
}

fn stream_contract_json() -> serde_json::Value {
    json!({
        "format": "server_sent_events",
        "cursor_parameter": "after_id",
        "limit_parameter": "limit",
        "event_id_field": "id",
        "data": "AppRunEvent JSON",
    })
}

fn sse_response(status: StatusCode, body: String) -> axum::response::Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "text/event-stream; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn format_sse_event(event: &str, id: Option<i64>, data: serde_json::Value) -> String {
    let event = event.replace(['\r', '\n'], "_");
    let data = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::new();
    out.push_str(&format!("event: {event}\n"));
    if let Some(id) = id {
        out.push_str(&format!("id: {id}\n"));
    }
    out.push_str(&format!("data: {data}\n\n"));
    out
}

fn read_app_run_log_tail(
    path: &Path,
    tail_lines: usize,
    max_bytes: usize,
) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let max_bytes = max_bytes.max(1) as u64;
    let start = size.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail_lines);
    let mut tail = lines[start..].join("\n");
    if !tail.is_empty() && text.ends_with('\n') {
        tail.push('\n');
    }
    Ok(tail)
}

fn authorize_write(
    state: &PlatformState,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    let Some(expected) = state.service_token.as_deref() else {
        return Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unconfigured",
            "AGENTHERO_SERVICE_TOKEN is unset",
        ));
    };
    let Some(actual) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing bearer token",
        ));
    };
    if actual != expected {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        ));
    }
    Ok(())
}

fn error_response(
    status: StatusCode,
    code: &str,
    detail: impl std::fmt::Display,
) -> axum::response::Response {
    (
        status,
        Json(json!({ "error": code, "detail": detail.to_string() })),
    )
        .into_response()
}
