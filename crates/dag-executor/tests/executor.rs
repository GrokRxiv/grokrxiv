use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::Duration;

use agenthero_dag_executor::{
    ArtifactRef, DagCancellationToken, DagExecutor, DagIo, GenericToolRunner, NodeExecutionContext,
    NodeExecutionResult, NodeHandler, LOOP_ROUND_INPUT, MAP_INDEX_INPUT,
};
use agenthero_dag_runtime::{DagManifest, DagNodeStatus};
use async_trait::async_trait;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Barrier, Notify};
use tracing::{
    field::{Field, Visit},
    Id,
};
use tracing_subscriber::{layer::Context, prelude::*, registry::LookupSpan, Layer};

#[derive(Clone, Default)]
struct RecordingHandler {
    calls: Arc<Mutex<Vec<String>>>,
    fail_nodes: Arc<BTreeSet<String>>,
    degrade_nodes: Arc<BTreeSet<String>>,
}

#[derive(Clone, Default)]
struct MapRecordingHandler {
    seen: Arc<Mutex<Vec<(u64, serde_json::Value)>>>,
}

#[derive(Clone)]
struct ParallelBarrierHandler {
    inner: RecordingHandler,
    barrier: Arc<Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    parallel_nodes: Arc<BTreeSet<String>>,
}

#[derive(Clone, Default)]
struct ArtifactProducingHandler;

#[derive(Clone, Default)]
struct UndeclaredArtifactHandler;

#[derive(Clone, Default)]
struct InvalidOutputHandler {
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Default)]
struct SchemaKeywordInvalidHandler;

#[derive(Clone, Default)]
struct NullableObjectHandler;

#[derive(Clone, Default)]
struct ApprovalPausingHandler {
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone, Default)]
struct ApprovalAndFailureHandler;

#[derive(Clone, Default)]
struct ProvenanceRecordingHandler;

#[derive(Clone, Default)]
struct TraceCollisionHandler;

#[derive(Clone, Default)]
struct RetryThenSucceedHandler {
    attempts: Arc<AtomicUsize>,
}

#[derive(Clone, Default)]
struct BlockingHandler {
    release: Arc<Notify>,
}

#[derive(Clone)]
struct CancellingHandler {
    inner: RecordingHandler,
    token: DagCancellationToken,
}

#[derive(Clone, Default)]
struct TracingLogHandler;

#[derive(Clone, Default)]
struct CapturedTracingEvents {
    fields: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
}

static TEST_TRACING_CAPTURE: OnceLock<Arc<Mutex<Option<CapturedTracingEvents>>>> = OnceLock::new();
static TEST_TRACING_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

impl CapturedTracingEvents {
    fn activate(&self) -> CapturedTracingGuard {
        self.fields.lock().expect("tracing events lock").clear();
        let active = install_test_tracing_capture();
        *active.lock().expect("active tracing capture lock") = Some(self.clone());
        CapturedTracingGuard { active }
    }

    fn events(&self) -> Vec<BTreeMap<String, String>> {
        self.fields.lock().expect("tracing events lock").clone()
    }
}

struct CapturedTracingGuard {
    active: Arc<Mutex<Option<CapturedTracingEvents>>>,
}

impl Drop for CapturedTracingGuard {
    fn drop(&mut self) {
        *self.active.lock().expect("active tracing capture lock") = None;
    }
}

#[derive(Clone)]
struct CapturedTracingLayer {
    active: Arc<Mutex<Option<CapturedTracingEvents>>>,
}

impl<S> Layer<S> for CapturedTracingLayer
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = CapturedTracingVisitor::default();
        attrs.record(&mut visitor);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut()
                .insert(CapturedSpanFields(visitor.fields));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(capture) = self
            .active
            .lock()
            .expect("active tracing capture lock")
            .clone()
        else {
            return;
        };
        let mut visitor = CapturedTracingVisitor::default();
        visitor
            .fields
            .insert("target".to_string(), event.metadata().target().to_string());
        visitor
            .fields
            .insert("level".to_string(), event.metadata().level().to_string());
        if let Some(scope) = _ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(fields) = span.extensions().get::<CapturedSpanFields>() {
                    for (key, value) in &fields.0 {
                        visitor
                            .fields
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        }
        event.record(&mut visitor);
        capture
            .fields
            .lock()
            .expect("tracing events lock")
            .push(visitor.fields);
    }
}

#[derive(Clone)]
struct CapturedSpanFields(BTreeMap<String, String>);

fn install_test_tracing_capture() -> Arc<Mutex<Option<CapturedTracingEvents>>> {
    TEST_TRACING_CAPTURE
        .get_or_init(|| {
            let active = Arc::new(Mutex::new(None));
            let subscriber = tracing_subscriber::registry().with(CapturedTracingLayer {
                active: active.clone(),
            });
            let _ = tracing::subscriber::set_global_default(subscriber);
            active
        })
        .clone()
}

fn tracing_test_lock() -> &'static Mutex<()> {
    TEST_TRACING_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Default)]
struct CapturedTracingVisitor {
    fields: BTreeMap<String, String>,
}

impl Visit for CapturedTracingVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let value = format!("{value:?}");
        self.fields.insert(
            field.name().to_string(),
            value.trim_matches('"').to_string(),
        );
    }
}

impl MapRecordingHandler {
    fn seen(&self) -> Vec<(u64, serde_json::Value)> {
        self.seen.lock().expect("seen lock").clone()
    }
}

impl ApprovalPausingHandler {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl InvalidOutputHandler {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl ParallelBarrierHandler {
    fn new(nodes: &[&str]) -> Self {
        Self {
            inner: RecordingHandler::default(),
            barrier: Arc::new(Barrier::new(nodes.len())),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            parallel_nodes: Arc::new(nodes.iter().map(|node| (*node).to_string()).collect()),
        }
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl NodeHandler for ParallelBarrierHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        if self.parallel_nodes.contains(&ctx.node.id) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
        self.inner.execute_node(ctx).await
    }
}

#[async_trait]
impl NodeHandler for MapRecordingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let index = ctx.inputs.values[MAP_INDEX_INPUT]
            .as_u64()
            .expect("map index is numeric");
        let item = ctx.inputs.values["item"].clone();
        self.seen.lock().expect("seen lock").push((index, item));
        Ok(NodeExecutionResult::ok()
            .with_value(format!("item_{index}"), json!({"processed": index})))
    }
}

impl RecordingHandler {
    fn fail(mut self, node: &str) -> Self {
        Arc::make_mut(&mut self.fail_nodes).insert(node.to_string());
        self
    }

    fn degrade(mut self, node: &str) -> Self {
        Arc::make_mut(&mut self.degrade_nodes).insert(node.to_string());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[async_trait]
impl NodeHandler for RecordingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(ctx.node.id.clone());
        if self.fail_nodes.contains(&ctx.node.id) {
            anyhow::bail!("{} failed by test", ctx.node.id);
        }
        if self.degrade_nodes.contains(&ctx.node.id) {
            return Ok(NodeExecutionResult::degraded(format!(
                "{} degraded by test",
                ctx.node.id
            )));
        }
        Ok(NodeExecutionResult::ok().with_value(
            ctx.node.id.clone(),
            json!({
                "kind": ctx.node.kind.to_string(),
                "seen_inputs": ctx.inputs.values.keys().cloned().collect::<Vec<_>>()
            }),
        ))
    }
}

#[async_trait]
impl NodeHandler for TracingLogHandler {
    async fn execute_node(
        &self,
        _ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        tracing::info!(
            handler_event = "inside_handler",
            "handler emitted observable log"
        );
        Ok(NodeExecutionResult::ok())
    }
}

#[async_trait]
impl NodeHandler for ArtifactProducingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let mut result = NodeExecutionResult::ok();
        for output in &ctx.node.outputs {
            result = result.with_artifact(
                output.clone(),
                ArtifactRef {
                    uri: format!("file:///tmp/{}", output),
                    media_type: Some("application/json".to_string()),
                    metadata: BTreeMap::new(),
                },
            );
        }
        Ok(result)
    }
}

#[async_trait]
impl NodeHandler for UndeclaredArtifactHandler {
    async fn execute_node(
        &self,
        _ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(NodeExecutionResult::ok().with_artifact(
            "undeclared/report.json",
            ArtifactRef {
                uri: "file:///tmp/undeclared/report.json".to_string(),
                media_type: Some("application/json".to_string()),
                metadata: BTreeMap::new(),
            },
        ))
    }
}

#[async_trait]
impl NodeHandler for InvalidOutputHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(ctx.node.id.clone());
        Ok(NodeExecutionResult::ok().with_value(
            "result",
            json!({
                "status": "missing_required_payload"
            }),
        ))
    }
}

#[async_trait]
impl NodeHandler for SchemaKeywordInvalidHandler {
    async fn execute_node(
        &self,
        _ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(NodeExecutionResult::ok().with_value(
            "result",
            json!({
                "status": "bad",
                "count": 0,
                "tags": [1],
                "maybe": null
            }),
        ))
    }
}

#[async_trait]
impl NodeHandler for NullableObjectHandler {
    async fn execute_node(
        &self,
        _ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        Ok(NodeExecutionResult::ok().with_value(
            "result",
            json!({
                "environment": null,
                "notes": null
            }),
        ))
    }
}

#[async_trait]
impl NodeHandler for ApprovalPausingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(ctx.node.id.clone());
        Ok(NodeExecutionResult {
            status: DagNodeStatus::AwaitingApproval,
            outputs: DagIo::default(),
            diagnostics: DagIo::default(),
            warning: Some(format!("{} waiting for approval", ctx.node.id)),
            error: None,
            command: None,
            exit_status: None,
            model: None,
            prompt_hash: None,
            trace: Default::default(),
        })
    }
}

#[async_trait]
impl NodeHandler for ApprovalAndFailureHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        match ctx.node.id.as_str() {
            "needs_approval" => Ok(NodeExecutionResult {
                status: DagNodeStatus::AwaitingApproval,
                outputs: DagIo::default(),
                diagnostics: DagIo::default(),
                warning: Some("waiting for approval".to_string()),
                error: None,
                command: None,
                exit_status: None,
                model: None,
                prompt_hash: None,
                trace: Default::default(),
            }),
            "fails_required" => anyhow::bail!("required sibling failed"),
            other => Ok(NodeExecutionResult::ok().with_value(other, json!(true))),
        }
    }
}

#[async_trait]
impl NodeHandler for ProvenanceRecordingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let mut result = NodeExecutionResult::ok()
            .with_model("gpt-loop-test")
            .with_prompt_hash("fnv1a64:prompttest")
            .with_command(vec!["llm-adapter".to_string(), ctx.node.id.clone()])
            .with_exit_status(Some(0));
        if ctx.inputs.values.contains_key(LOOP_ROUND_INPUT) {
            result = result.with_value("loop_continue", json!(false));
        }
        Ok(result)
    }
}

#[async_trait]
impl NodeHandler for TraceCollisionHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let mut result = NodeExecutionResult::ok()
            .with_trace_value("loop_round", json!("handler-loop-round"))
            .with_trace_value("handler_only", json!(true));
        if ctx.inputs.values.contains_key(LOOP_ROUND_INPUT) {
            result = result.with_value("loop_continue", json!(false));
        }
        Ok(result)
    }
}

impl RetryThenSucceedHandler {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl BlockingHandler {
    fn release(&self) {
        self.release.notify_waiters();
    }
}

impl CancellingHandler {
    fn new(token: DagCancellationToken) -> Self {
        Self {
            inner: RecordingHandler::default(),
            token,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.inner.calls()
    }
}

#[async_trait]
impl NodeHandler for RetryThenSucceedHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt == 1 {
            anyhow::bail!("{} transient failure", ctx.node.id);
        }
        Ok(NodeExecutionResult::ok().with_value(
            ctx.node.id.clone(),
            json!({
                "attempt": attempt,
                "recovered": true,
            }),
        ))
    }
}

#[async_trait]
impl NodeHandler for BlockingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        self.release.notified().await;
        Ok(NodeExecutionResult::ok().with_value(ctx.node.id.clone(), json!(true)))
    }
}

#[async_trait]
impl NodeHandler for CancellingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let result = self.inner.execute_node(ctx).await?;
        self.token.cancel();
        Ok(result)
    }
}

#[derive(Clone, Default)]
struct LoopingHandler {
    rounds: Arc<Mutex<Vec<u64>>>,
    stop_after_round: Option<u64>,
}

impl LoopingHandler {
    fn stop_after(round: u64) -> Self {
        Self {
            rounds: Arc::default(),
            stop_after_round: Some(round),
        }
    }

    fn never_stop() -> Self {
        Self {
            rounds: Arc::default(),
            stop_after_round: None,
        }
    }

    fn rounds(&self) -> Vec<u64> {
        self.rounds.lock().expect("rounds lock").clone()
    }
}

#[async_trait]
impl NodeHandler for LoopingHandler {
    async fn execute_node(
        &self,
        ctx: NodeExecutionContext<'_>,
    ) -> anyhow::Result<NodeExecutionResult> {
        let round = ctx.inputs.values[LOOP_ROUND_INPUT]
            .as_u64()
            .expect("loop round input is numeric");
        self.rounds.lock().expect("rounds lock").push(round);
        let continue_loop = self
            .stop_after_round
            .map(|stop_after| round < stop_after)
            .unwrap_or(true);
        Ok(NodeExecutionResult::ok()
            .with_value("last_round", json!(round))
            .with_value("loop_continue", json!(continue_loop)))
    }
}

fn manifest(text: &str) -> DagManifest {
    DagManifest::from_str(text).expect("manifest parses")
}

fn temp_workspace(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "agenthero-dag-executor-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("temp workspace exists");
    path
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::write(path, contents).expect("test executable is written");
    let mut permissions = std::fs::metadata(path)
        .expect("test executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("test executable is executable");
}

#[tokio::test]
async fn runs_manifest_layers_and_collects_json_and_artifact_refs() {
    let manifest = manifest(
        r#"
id: sample
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: left
    kind: tool
    tool: left_tool
    inputs: [prepared.json]
    outputs: [left.json]
    required: true
  - id: right
    kind: agent
    inputs: [prepared.json]
    outputs: [right.json]
    required: true
  - id: join
    kind: artifact
    inputs: [left.json, right.json]
    outputs: [joined.json]
    required: true
tools:
  - id: left_tool
    executor: rust
edges:
  - from: prepare
    to: [left, right]
  - from: [left, right]
    to: join
"#,
    );
    let mut input = DagIo::default();
    input.values.insert("seed".to_string(), json!({"ok": true}));
    input.artifacts.insert(
        "source.tar.gz".to_string(),
        ArtifactRef {
            uri: "file:///tmp/source.tar.gz".to_string(),
            media_type: Some("application/gzip".to_string()),
            metadata: BTreeMap::new(),
        },
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, input)
        .await
        .expect("dag runs");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.calls(), vec!["prepare", "left", "right", "join"]);
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("join"));
    assert!(report.outputs.artifacts.contains_key("source.tar.gz"));
}

#[tokio::test]
async fn runs_ready_nodes_concurrently_with_manifest_limit() {
    let manifest = manifest(
        r#"
id: concurrent
version: 1
concurrency: 2
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: left
    kind: artifact
    required: true
  - id: right
    kind: artifact
    required: true
  - id: join
    kind: artifact
    required: true
edges:
  - from: prepare
    to: [left, right]
  - from: [left, right]
    to: join
"#,
    );
    let handler = ParallelBarrierHandler::new(&["left", "right"]);

    let report = tokio::time::timeout(
        Duration::from_secs(1),
        DagExecutor::new(handler.clone()).execute(&manifest, DagIo::default()),
    )
    .await
    .expect("ready nodes should start concurrently instead of blocking each other")
    .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.max_active(), 2);
}

#[tokio::test]
async fn cancelled_executor_skips_unstarted_nodes_without_calling_handler() {
    let manifest = manifest(
        r#"
id: cancelled_before_start
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: prepare
    to: publish
"#,
    );
    let handler = RecordingHandler::default();
    let token = DagCancellationToken::new();
    token.cancel();

    let report = DagExecutor::new(handler.clone())
        .with_cancellation_token(token)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), Vec::<String>::new());
    assert_eq!(report.status, DagNodeStatus::Skipped);
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|node| (node.node_id.as_str(), node.status))
            .collect::<Vec<_>>(),
        vec![
            ("prepare", DagNodeStatus::Skipped),
            ("publish", DagNodeStatus::Skipped),
        ]
    );
    assert!(report
        .nodes
        .iter()
        .all(|node| node.trace["cancelled"] == json!(true)));
    assert_eq!(
        report
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.node_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("dag.started", None),
            ("node.skipped", Some("prepare")),
            ("node.skipped", Some("publish")),
            ("dag.cancelled", None),
        ]
    );
    assert_eq!(
        report.events.last().expect("terminal event").payload["status"],
        json!("cancelled")
    );
}

#[tokio::test]
async fn cancellation_between_layers_skips_remaining_nodes() {
    let manifest = manifest(
        r#"
id: cancelled_between_layers
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: prepare
    to: publish
"#,
    );
    let token = DagCancellationToken::new();
    let handler = CancellingHandler::new(token.clone());

    let report = DagExecutor::new(handler.clone())
        .with_cancellation_token(token)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), vec!["prepare"]);
    assert_eq!(report.status, DagNodeStatus::Skipped);
    assert_eq!(report.node_status("prepare"), Some(DagNodeStatus::Ok));
    assert_eq!(report.node_status("publish"), Some(DagNodeStatus::Skipped));
    assert_eq!(
        report
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.node_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("dag.started", None),
            ("node.started", Some("prepare")),
            ("node.completed", Some("prepare")),
            ("node.skipped", Some("publish")),
            ("dag.cancelled", None),
        ]
    );
}

#[tokio::test]
async fn report_emits_node_status_events_for_observability() {
    let manifest = manifest(
        r#"
id: observable
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: prepare
    to: publish
"#,
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(
        report
            .events
            .iter()
            .map(|event| (event.event_type.as_str(), event.node_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("dag.started", None),
            ("node.started", Some("prepare")),
            ("node.completed", Some("prepare")),
            ("node.started", Some("publish")),
            ("node.completed", Some("publish")),
            ("dag.completed", None),
        ]
    );
    let started = report.events.first().expect("dag started event");
    assert_eq!(started.payload["dag_type"], json!("observable"));
    assert_eq!(started.payload["manifest_version"], json!(1));
    let manifest_hash = started.payload["manifest_hash"]
        .as_str()
        .expect("manifest hash");
    assert!(manifest_hash.starts_with("fnv1a64:"));
    for event in report.events.iter().filter(|event| event.node_id.is_some()) {
        assert_eq!(event.payload["dag_type"], json!("observable"));
        assert_eq!(event.payload["manifest_version"], json!(1));
        assert_eq!(event.payload["manifest_hash"], json!(manifest_hash));
    }
    let completed = report.events.last().expect("dag completed event");
    assert_eq!(completed.payload["status"], json!("ok"));
    assert_eq!(completed.payload["node_count"], json!(2));
}

#[tokio::test]
async fn report_events_carry_runtime_identity_for_audit() {
    let manifest = manifest(
        r#"
id: observable_identity
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
"#,
    );
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );
    input.values.insert(
        "dag_run_id".to_string(),
        json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
    );
    input.values.insert(
        "lease_id".to_string(),
        json!("a9353847-48b3-472e-b88e-89770fcdbf7a"),
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    for event in &report.events {
        assert_eq!(
            event.payload["app_run_id"],
            json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
            "{} should carry app_run_id",
            event.event_type
        );
        assert_eq!(
            event.payload["dag_run_id"],
            json!("f78c57db-89e3-4b63-8c1a-2c07e3331f0c"),
            "{} should carry dag_run_id",
            event.event_type
        );
        assert_eq!(
            event.payload["lease_id"],
            json!("a9353847-48b3-472e-b88e-89770fcdbf7a"),
            "{} should carry lease_id",
            event.event_type
        );
    }
}

#[tokio::test]
async fn report_events_carry_mandatory_agenthero_trace_fields() {
    let manifest = manifest(
        r#"
id: observable_trace_contract
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
"#,
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    let required_fields = [
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
    for event in &report.events {
        for field in required_fields {
            assert!(
                event.payload.contains_key(field),
                "{} missing AgentHero trace field `{field}` in payload {:?}",
                event.event_type,
                event.payload
            );
        }
    }
}

#[tokio::test]
async fn event_sink_receives_node_started_before_handler_completes() {
    let manifest = manifest(
        r#"
id: live_events
version: 1
accepts: []
nodes:
  - id: long_running
    kind: artifact
    required: true
"#,
    );
    let handler = BlockingHandler::default();
    let release = handler.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let executor = DagExecutor::new(handler).with_event_sink(move |event| {
        tx.send(event).expect("event receiver remains open");
    });

    let task = tokio::spawn(async move { executor.execute(&manifest, DagIo::default()).await });
    let started = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("dag.started should be emitted before the handler starts")
        .expect("event is sent");

    assert_eq!(started.event_type, "dag.started");
    assert_eq!(started.node_id, None);
    assert_eq!(started.payload["dag_type"], json!("live_events"));
    let manifest_hash = started.payload["manifest_hash"]
        .as_str()
        .expect("dag started manifest hash")
        .to_string();

    let node_started = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("node.started should be emitted before the handler completes")
        .expect("node event is sent");

    assert_eq!(node_started.event_type, "node.started");
    assert_eq!(node_started.node_id.as_deref(), Some("long_running"));
    assert_eq!(node_started.payload["attempt"], json!(1));
    assert_eq!(node_started.payload["dag_type"], json!("live_events"));
    assert_eq!(node_started.payload["manifest_version"], json!(1));
    assert_eq!(node_started.payload["manifest_hash"], json!(manifest_hash));

    release.release();
    let node_completed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("node.completed should be emitted after the handler completes")
        .expect("node terminal event is sent");
    assert_eq!(node_completed.event_type, "node.completed");
    assert_eq!(node_completed.node_id.as_deref(), Some("long_running"));
    assert!(node_completed
        .payload
        .get("latency_ms")
        .and_then(serde_json::Value::as_u64)
        .is_some());

    let report = task
        .await
        .expect("executor task joins")
        .expect("dag report is returned");
    assert_eq!(report.status, DagNodeStatus::Ok);
    let report_completed = report
        .events
        .iter()
        .find(|event| {
            event.event_type == "node.completed" && event.node_id.as_deref() == Some("long_running")
        })
        .expect("report node terminal event");
    assert_eq!(
        report_completed.payload.get("latency_ms"),
        node_completed.payload.get("latency_ms")
    );
    assert!(report
        .events
        .iter()
        .any(|event| event.event_type == "dag.completed" && event.node_id.is_none()));
}

#[tokio::test]
async fn node_events_include_runtime_identity_from_input() {
    let manifest = manifest(
        r#"
id: runtime_identity_events
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
"#,
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    let mut input = DagIo::default();
    input.values.insert(
        "app_run_id".to_string(),
        json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"),
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .with_event_sink(move |event| {
            captured.lock().expect("event capture lock").push(event);
        })
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    for event in events
        .lock()
        .expect("event capture lock")
        .iter()
        .chain(report.events.iter())
        .filter(|event| event.node_id.is_some())
    {
        assert_eq!(
            event.payload.get("app_run_id"),
            Some(&json!("2d0a1d88-b9f9-4e8f-848e-605b86717330"))
        );
        assert_eq!(
            event.payload.get("dag_type"),
            Some(&json!("runtime_identity_events"))
        );
        assert!(event.payload.contains_key("node_kind"));
    }
}

#[test]
fn executor_emits_structured_tracing_events_for_dag_and_node_lifecycle() {
    let _tracing_lock = tracing_test_lock()
        .lock()
        .expect("tracing test lock is not poisoned");
    let manifest = manifest(
        r#"
id: traceable
version: 7
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
"#,
    );
    let captured = CapturedTracingEvents::default();
    let _capture_guard = captured.activate();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");
    let report = runtime
        .block_on(async {
            DagExecutor::new(RecordingHandler::default())
                .execute(&manifest, DagIo::default())
                .await
        })
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let events = captured.events();
    let dag_started = events
        .iter()
        .find(|event| {
            event
                .get("event_type")
                .is_some_and(|value| value == "dag.started")
                && event
                    .get("dag_type")
                    .is_some_and(|value| value == "traceable")
        })
        .expect("dag started tracing event");
    assert_eq!(
        dag_started.get("target").map(String::as_str),
        Some("agenthero::dag")
    );
    assert_eq!(
        dag_started.get("dag_type").map(String::as_str),
        Some("traceable")
    );
    assert_eq!(
        dag_started.get("manifest_version").map(String::as_str),
        Some("7")
    );
    let manifest_hash = dag_started
        .get("manifest_hash")
        .expect("manifest hash field")
        .clone();
    assert!(manifest_hash.starts_with("fnv1a64:"));

    let node_started = events
        .iter()
        .find(|event| {
            event
                .get("event_type")
                .is_some_and(|value| value == "node.started")
                && event
                    .get("dag_type")
                    .is_some_and(|value| value == "traceable")
        })
        .expect("node started tracing event");
    assert_eq!(
        node_started.get("dag_type").map(String::as_str),
        Some("traceable")
    );
    assert_eq!(node_started.get("manifest_hash"), Some(&manifest_hash));
    assert_eq!(
        node_started.get("node_id").map(String::as_str),
        Some("prepare")
    );
    assert_eq!(
        node_started.get("kind").map(String::as_str),
        Some("prepare_inputs")
    );
    assert_eq!(
        node_started.get("node_kind").map(String::as_str),
        Some("prepare_inputs")
    );
    for field in [
        "app_run_id",
        "dag_run_id",
        "tool_id",
        "artifact_id",
        "lease_id",
    ] {
        assert!(node_started.contains_key(field), "missing {field}");
    }
    assert_eq!(node_started.get("attempt").map(String::as_str), Some("1"));

    let node_completed = events
        .iter()
        .find(|event| {
            event
                .get("event_type")
                .is_some_and(|value| value == "node.completed")
                && event
                    .get("dag_type")
                    .is_some_and(|value| value == "traceable")
        })
        .expect("node completed tracing event");
    assert_eq!(node_completed.get("status").map(String::as_str), Some("ok"));
    assert_eq!(
        node_completed.get("node_id").map(String::as_str),
        Some("prepare")
    );
    assert!(node_completed
        .get("latency_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some());
    assert!(node_completed
        .get("duration_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some());
}

#[cfg(unix)]
#[test]
fn executor_tracing_node_terminal_event_includes_command_provenance() {
    let _tracing_lock = tracing_test_lock()
        .lock()
        .expect("tracing test lock is not poisoned");
    let workspace = temp_workspace("traceable-command-provenance");
    let adapter = workspace.join("llm-adapter");
    write_executable(&adapter, "#!/bin/sh\nprintf traced > result.txt\n");
    let manifest = manifest(&format!(
        r#"
id: traceable_command
version: 3
accepts: []
tools:
  - id: writer
    executor: llm
    command: ["{}"]
nodes:
  - id: run_writer
    kind: tool
    tool: writer
    outputs: [result.txt]
    required: true
"#,
        adapter.display()
    ));
    let mut input = DagIo::default();
    input
        .values
        .insert("model".to_string(), json!("local-trace-model"));
    input
        .values
        .insert("prompt".to_string(), json!("Trace this command node."));
    let captured = CapturedTracingEvents::default();
    let _capture_guard = captured.activate();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    let report = runtime
        .block_on(async {
            DagExecutor::new(GenericToolRunner::new(&workspace))
                .execute(&manifest, input)
                .await
        })
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let events = captured.events();
    let node_completed = events
        .iter()
        .find(|event| {
            event
                .get("event_type")
                .is_some_and(|value| value == "node.completed")
                && event
                    .get("dag_type")
                    .is_some_and(|value| value == "traceable_command")
        })
        .expect("node completed tracing event");
    let expected_command =
        serde_json::to_string(&vec![adapter.display().to_string()]).expect("expected command");
    assert_eq!(
        node_completed.get("command").map(String::as_str),
        Some(expected_command.as_str())
    );
    assert_eq!(
        node_completed.get("exit_status").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        node_completed.get("model").map(String::as_str),
        Some("local-trace-model")
    );
    assert!(node_completed
        .get("prompt_hash")
        .expect("prompt hash")
        .starts_with("fnv1a64:"));
    assert!(node_completed
        .get("output_refs")
        .expect("output refs")
        .contains("result.txt"));
    assert!(node_completed
        .get("diagnostic_refs")
        .expect("diagnostic refs")
        .contains("status.json"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[test]
fn handler_tracing_logs_inherit_agenthero_node_span_context() {
    let _tracing_lock = tracing_test_lock()
        .lock()
        .expect("tracing test lock is not poisoned");
    let manifest = manifest(
        r#"
id: handler_span_context
version: 11
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
"#,
    );
    let captured = CapturedTracingEvents::default();
    let _capture_guard = captured.activate();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");

    let report = runtime
        .block_on(async {
            DagExecutor::new(TracingLogHandler)
                .execute(&manifest, DagIo::default())
                .await
        })
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let events = captured.events();
    let handler_log = events
        .iter()
        .find(|event| {
            event
                .get("handler_event")
                .is_some_and(|value| value == "inside_handler")
        })
        .expect("handler log tracing event");

    assert_eq!(
        handler_log.get("dag_type").map(String::as_str),
        Some("handler_span_context")
    );
    assert_eq!(
        handler_log.get("manifest_version").map(String::as_str),
        Some("11")
    );
    assert!(handler_log
        .get("manifest_hash")
        .expect("manifest hash field")
        .starts_with("fnv1a64:"));
    assert_eq!(
        handler_log.get("node_id").map(String::as_str),
        Some("prepare")
    );
    assert_eq!(
        handler_log.get("node_kind").map(String::as_str),
        Some("prepare_inputs")
    );
    assert_eq!(handler_log.get("attempt").map(String::as_str), Some("1"));
}

#[tokio::test]
async fn report_orders_synthetic_node_events_before_terminal_dag_event() {
    let manifest = manifest(
        r#"
id: terminal_order
version: 1
accepts: []
nodes:
  - id: a
    kind: artifact
    required: true
  - id: b
    kind: artifact
    required: true
  - id: quorum
    kind: gate
    gate:
      min_usable: 2
      sources: [a, b]
edges:
  - from: [a, b]
    to: quorum
"#,
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert!(report
        .events
        .iter()
        .any(|event| event.event_type == "node.completed"
            && event.node_id.as_deref() == Some("quorum")));
    assert_eq!(
        report
            .events
            .last()
            .map(|event| (event.event_type.as_str(), event.node_id.as_deref())),
        Some(("dag.completed", None))
    );
}

#[tokio::test]
async fn report_records_manifest_identity_frozen_input_and_attempt_provenance() {
    let manifest = manifest(
        r#"
id: provenance
version: 7
accepts: []
tools:
  - id: cli_tool
    executor: cli
    command: ["echo", "ok"]
    timeout_secs: 45
    policy:
      budget_units: 3
nodes:
  - id: run_cli
    kind: tool
    tool: cli_tool
    inputs: [source.json]
    outputs: [result.json]
    required: true
"#,
    );
    let mut input = DagIo::default();
    input
        .values
        .insert("source".to_string(), json!({"frozen": true}));
    input.artifacts.insert(
        "source.json".to_string(),
        ArtifactRef {
            uri: "artifact://inputs/source.json".to_string(),
            media_type: Some("application/json".to_string()),
            metadata: BTreeMap::new(),
        },
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, input.clone())
        .await
        .expect("dag report is returned");

    assert_eq!(report.manifest_version, 7);
    assert_eq!(report.input, input);
    assert!(!report.manifest_hash.is_empty());
    assert_eq!(report.nodes.len(), 1);

    let node = &report.nodes[0];
    assert_eq!(node.attempt, 1);
    assert_eq!(node.tool.as_deref(), Some("cli_tool"));
    assert!(node.required);
    assert_eq!(
        node.command,
        Some(vec!["echo".to_string(), "ok".to_string()])
    );
    assert_eq!(node.exit_status, None);
    assert_eq!(node.policy["tool"]["executor"], json!("cli"));
    assert_eq!(node.policy["tool"]["timeout_secs"], json!(45));
    assert_eq!(node.policy["tool"]["budget_units"], json!(3));
    assert_eq!(node.policy["tool"]["approval_required"], json!(false));
    assert_eq!(node.policy["tool"]["network"]["allow"], json!(true));
    assert_eq!(node.policy["tool"]["filesystem"]["read"], json!([]));
    assert_eq!(node.policy["tool"]["filesystem"]["write"], json!([]));
    assert_eq!(
        node.input_refs.get("source.json").map(String::as_str),
        Some("artifact://inputs/source.json")
    );
    assert_eq!(node.outputs, vec!["result.json"]);
    assert!(node.output_refs.is_empty());
}

#[tokio::test]
async fn report_records_tool_runtime_policy_even_without_explicit_tool_policy() {
    let manifest = manifest(
        r#"
id: default_tool_policy_snapshot
version: 1
accepts: []
tools:
  - id: shell_tool
    executor: shell
    command: ["sh", "-c", "printf ok"]
    timeout_secs: 12
nodes:
  - id: run_shell
    kind: tool
    tool: shell_tool
    required: true
"#,
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    let node = &report.nodes[0];
    assert_eq!(node.policy["tool"]["executor"], json!("shell"));
    assert_eq!(node.policy["tool"]["timeout_secs"], json!(12));
    assert_eq!(node.policy["tool"]["budget_units"], serde_json::Value::Null);
    assert_eq!(node.policy["tool"]["approval_required"], json!(false));
    assert_eq!(node.policy["tool"]["network"]["allow"], json!(true));
    assert_eq!(node.policy["tool"]["filesystem"]["read"], json!([]));
    assert_eq!(node.policy["tool"]["filesystem"]["write"], json!([]));
}

#[tokio::test]
async fn tool_input_schema_rejects_invalid_dag_io_without_calling_handler() {
    let manifest = manifest(
        r#"
id: contract_input
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    input_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [source]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), Vec::<String>::new());
    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert!(node
        .error
        .as_deref()
        .expect("schema error")
        .contains("input schema"));
}

#[tokio::test]
async fn tool_input_schema_fails_before_approval_policy_pause() {
    let manifest = manifest(
        r#"
id: contract_before_approval
version: 1
accepts: []
tools:
  - id: gated_contract_tool
    executor: rust
    input_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [source]
    policy:
      approval_required: true
nodes:
  - id: run_contract
    kind: tool
    tool: gated_contract_tool
    required: true
"#,
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), Vec::<String>::new());
    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_ne!(node.status, DagNodeStatus::AwaitingApproval);
    assert!(node
        .error
        .as_deref()
        .expect("schema error")
        .contains("input schema"));
}

#[tokio::test]
async fn tool_input_schema_validates_declared_values_and_artifacts() {
    let manifest = manifest(
        r#"
id: contract_input_artifacts
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    input_schema:
      type: object
      required: [values, artifacts]
      properties:
        values:
          type: object
          required: [review_id]
        artifacts:
          type: object
          required: [body.md]
          properties:
            body.md:
              type: object
              required: [uri, media_type]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    inputs: [review_id, body.md]
    required: true
"#,
    );
    let mut input = DagIo::default();
    input
        .values
        .insert("review_id".to_string(), json!("review-1"));
    input
        .values
        .insert("unrelated".to_string(), json!("ignored"));
    input.artifacts.insert(
        "body.md".to_string(),
        ArtifactRef {
            uri: "file:///tmp/body.md".to_string(),
            media_type: Some("text/markdown".to_string()),
            metadata: BTreeMap::new(),
        },
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), vec!["run_contract"]);
    assert_eq!(report.status, DagNodeStatus::Ok);
}

#[tokio::test]
async fn tool_output_schema_rejects_invalid_dag_io_and_blocks_descendants() {
    let manifest = manifest(
        r#"
id: contract_output
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [result]
          properties:
            result:
              type: object
              required: [payload]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    outputs: [result]
    required: true
  - id: child
    kind: artifact
    required: true
edges:
  - from: run_contract
    to: child
"#,
    );
    let handler = InvalidOutputHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), vec!["run_contract"]);
    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(
        report.node_status("run_contract"),
        Some(DagNodeStatus::Failed)
    );
    assert_eq!(report.node_status("child"), Some(DagNodeStatus::Skipped));
    assert!(!report.outputs.values.contains_key("result"));
    assert!(report.nodes[0]
        .error
        .as_deref()
        .expect("schema error")
        .contains("output schema"));
}

#[tokio::test]
async fn tool_output_schema_validates_artifact_refs() {
    let manifest = manifest(
        r#"
id: contract_output_artifacts
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [artifacts]
      properties:
        artifacts:
          type: object
          required: [report.json]
          properties:
            report.json:
              type: object
              required: [uri, media_type]
nodes:
  - id: write_report
    kind: tool
    tool: contract_tool
    outputs: [report.json]
    required: true
"#,
    );

    let report = DagExecutor::new(ArtifactProducingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.node_status("write_report"), Some(DagNodeStatus::Ok));
    assert!(report.outputs.artifacts.contains_key("report.json"));
}

#[tokio::test]
async fn tool_output_schema_does_not_mask_awaiting_approval_status() {
    let manifest = manifest(
        r#"
id: contract_approval
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [result]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );

    let report = DagExecutor::new(ApprovalPausingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(
        report.node_status("run_contract"),
        Some(DagNodeStatus::AwaitingApproval)
    );
}

#[tokio::test]
async fn tool_policy_approval_required_pauses_before_app_handler() {
    let manifest = manifest(
        r#"
id: policy_approval
version: 1
accepts: []
tools:
  - id: gated_tool
    executor: rust
    policy:
      approval_required: true
nodes:
  - id: run_gated
    kind: tool
    tool: gated_tool
    required: true
"#,
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.calls(), Vec::<String>::new());
    assert_eq!(report.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(
        report.node_status("run_gated"),
        Some(DagNodeStatus::AwaitingApproval)
    );
}

#[tokio::test]
async fn loop_node_tool_output_schema_validates_each_round() {
    let manifest = manifest(
        r#"
id: contract_loop
version: 1
accepts: []
tools:
  - id: loop_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [missing_round_output]
nodes:
  - id: repair
    kind: loop
    tool: loop_tool
    loop:
      max_rounds: 1
    required: true
"#,
    );

    let report = DagExecutor::new(LoopingHandler::stop_after(1))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(report.node_status("repair"), Some(DagNodeStatus::Failed));
    assert!(report.nodes[0]
        .error
        .as_deref()
        .expect("schema error")
        .contains("output schema"));
}

#[tokio::test]
async fn tool_output_schema_enforces_common_json_schema_keywords() {
    let manifest = manifest(
        r#"
id: contract_keywords
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [result]
          properties:
            result:
              type: object
              required: [status, count, tags, maybe]
              properties:
                status:
                  type: string
                  enum: [ok]
                count:
                  type: integer
                  minimum: 1
                tags:
                  type: array
                  items:
                    type: string
                maybe:
                  type: [string, "null"]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );

    let report = DagExecutor::new(SchemaKeywordInvalidHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let error = report.nodes[0].error.as_deref().expect("schema error");
    assert!(
        error.contains("enum") || error.contains("minimum") || error.contains("items"),
        "{error}"
    );
}

#[tokio::test]
async fn tool_output_schema_rejects_malformed_inline_keywords() {
    let manifest = manifest(
        r#"
id: contract_malformed_schema
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: values
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );

    let report = DagExecutor::new(RecordingHandler::default())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert!(report.nodes[0]
        .error
        .as_deref()
        .expect("schema error")
        .contains("required must be an array"));
}

#[tokio::test]
async fn tool_output_schema_allows_nullable_objects_with_properties() {
    let manifest = manifest(
        r#"
id: contract_nullable_object
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [result]
          properties:
            result:
              type: object
              required: [environment, notes]
              properties:
                environment:
                  type: [object, "null"]
                  properties:
                    hardware:
                      type: [string, "null"]
                    software:
                      type: [string, "null"]
                  additionalProperties: false
                notes:
                  type: [array, "null"]
                  items:
                    type: string
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );

    let report = DagExecutor::new(NullableObjectHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.node_status("run_contract"), Some(DagNodeStatus::Ok));
}

#[tokio::test]
async fn tool_output_schema_accepts_valid_dag_io() {
    let manifest = manifest(
        r#"
id: contract_valid
version: 1
accepts: []
tools:
  - id: contract_tool
    executor: rust
    output_schema:
      type: object
      required: [values]
      properties:
        values:
          type: object
          required: [run_contract]
          properties:
            run_contract:
              type: object
              required: [kind]
nodes:
  - id: run_contract
    kind: tool
    tool: contract_tool
    required: true
"#,
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.node_status("run_contract"), Some(DagNodeStatus::Ok));
    assert_eq!(report.outputs.values["run_contract"]["kind"], "tool");
}

#[tokio::test]
async fn output_refs_record_actual_artifacts_returned_by_handler() {
    let manifest = manifest(
        r#"
id: artifact_refs
version: 1
accepts: []
nodes:
  - id: write_report
    kind: artifact
    outputs: [report.json]
    required: true
"#,
    );

    let report = DagExecutor::new(ArtifactProducingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    let node = &report.nodes[0];
    assert_eq!(
        node.output_refs.get("report.json").map(String::as_str),
        Some("file:///tmp/report.json")
    );
    assert_eq!(
        report.outputs.artifacts["report.json"].uri,
        "file:///tmp/report.json"
    );
}

#[tokio::test]
async fn undeclared_artifact_outputs_fail_the_node_contract() {
    let manifest = manifest(
        r#"
id: undeclared_artifact
version: 1
accepts: []
nodes:
  - id: write_report
    kind: artifact
    outputs: [report.json]
    required: true
"#,
    );

    let report = DagExecutor::new(UndeclaredArtifactHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("artifact contract error")
        .contains("undeclared artifact output"));
}

#[tokio::test]
async fn generic_tool_runner_executes_command_and_records_materialized_artifacts() {
    let manifest = manifest(
        r#"
id: command_runner
version: 1
accepts: []
tools:
  - id: write_result
    executor: cli
    command: ["sh", "-c", "printf 'payload' > result.txt; printf 'hello stdout'; printf 'hello stderr' >&2"]
nodes:
  - id: run_command
    kind: tool
    tool: write_result
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-ok");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, Some(0));
    assert_eq!(
        node.command,
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf 'payload' > result.txt; printf 'hello stdout'; printf 'hello stderr' >&2"
                .to_string()
        ])
    );
    let result_path =
        std::path::PathBuf::from(node.output_refs.get("result.txt").expect("result ref"));
    assert_eq!(
        node.output_refs.get("result.txt").map(String::as_str),
        Some(result_path.to_string_lossy().as_ref())
    );
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_command/stdout.log"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_command/stderr.log"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_command/status.json"));
    assert!(!node.output_refs.contains_key("logs/run_command/stdout.log"));
    assert_eq!(
        std::fs::read_to_string(&result_path).expect("result artifact written"),
        "payload"
    );
    assert_eq!(
        std::fs::read_to_string(
            node.diagnostic_refs
                .get("logs/run_command/stdout.log")
                .expect("stdout ref")
        )
        .expect("stdout log written"),
        "hello stdout"
    );
    assert_eq!(
        std::fs::read_to_string(
            node.diagnostic_refs
                .get("logs/run_command/stderr.log")
                .expect("stderr ref")
        )
        .expect("stderr log written"),
        "hello stderr"
    );
    assert!(report.outputs.artifacts.contains_key("result.txt"));
    let artifact = report
        .outputs
        .artifacts
        .get("result.txt")
        .expect("final artifact ref");
    assert_eq!(
        artifact.metadata.get("sha256"),
        Some(&json!(
            "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
        ))
    );
    assert_eq!(artifact.metadata.get("size_bytes"), Some(&json!(7)));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_live_events_include_tool_identity_before_report_enrichment() {
    let manifest = manifest(
        r#"
id: live_tool_identity
version: 1
accepts: []
tools:
  - id: write_result
    executor: cli
    command: ["sh", "-c", "printf 'payload' > result.txt"]
nodes:
  - id: run_command
    kind: tool
    tool: write_result
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("live-tool-identity");
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .with_event_sink(move |event| {
            captured.lock().expect("event lock").push(event);
        })
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let events = events.lock().expect("event lock");
    for event_type in ["node.started", "node.completed"] {
        let event = events
            .iter()
            .find(|event| {
                event.event_type == event_type && event.node_id.as_deref() == Some("run_command")
            })
            .unwrap_or_else(|| panic!("{event_type} event should be emitted"));
        assert_eq!(event.payload["tool"], json!("write_result"));
        assert_eq!(event.payload["tool_id"], json!("write_result"));
        assert_eq!(event.payload["executor"], json!("cli"));
        assert_eq!(event.payload["required"], json!(true));
    }

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_uses_unique_artifact_paths_per_node_execution() {
    let manifest = manifest(
        r#"
id: command_replay
version: 1
accepts: []
tools:
  - id: write_result
    executor: cli
    command: ["sh", "-c", "printf 'payload' > result.txt"]
nodes:
  - id: run_command
    kind: tool
    tool: write_result
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-unique");
    let runner = GenericToolRunner::new(&workspace);

    let first = DagExecutor::new(runner.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("first run succeeds");
    let second = DagExecutor::new(runner)
        .execute(&manifest, DagIo::default())
        .await
        .expect("second run succeeds");

    let first_ref = first.nodes[0]
        .output_refs
        .get("result.txt")
        .expect("first result ref");
    let second_ref = second.nodes[0]
        .output_refs
        .get("result.txt")
        .expect("second result ref");
    assert_ne!(first_ref, second_ref);
    assert_eq!(
        std::fs::read_to_string(first_ref).expect("first artifact remains readable"),
        "payload"
    );
    assert_eq!(
        std::fs::read_to_string(second_ref).expect("second artifact remains readable"),
        "payload"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_uses_unique_artifact_paths_per_runner_instance() {
    let manifest = manifest(
        r#"
id: command_replay
version: 1
accepts: []
tools:
  - id: write_result
    executor: cli
    command: ["sh", "-c", "printf 'payload' > result.txt"]
nodes:
  - id: run_command
    kind: tool
    tool: write_result
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-unique-runners");

    let first = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("first run succeeds");
    let second = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("second run succeeds");

    let first_ref = first.nodes[0]
        .output_refs
        .get("result.txt")
        .expect("first result ref");
    let second_ref = second.nodes[0]
        .output_refs
        .get("result.txt")
        .expect("second result ref");
    assert_ne!(first_ref, second_ref);
    assert_eq!(
        std::fs::read_to_string(first_ref).expect("first artifact remains readable"),
        "payload"
    );
    assert_eq!(
        std::fs::read_to_string(second_ref).expect("second artifact remains readable"),
        "payload"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_times_out_command_and_records_failed_status() {
    let manifest = manifest(
        r#"
id: command_timeout
version: 1
accepts: []
tools:
  - id: slow_tool
    executor: cli
    command: ["sh", "-c", "sleep 2"]
    timeout_secs: 1
nodes:
  - id: run_slow
    kind: tool
    tool: slow_tool
    required: true
"#,
    );
    let workspace = temp_workspace("command-timeout");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, None);
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_slow/status.json"));
    assert!(node
        .error
        .as_deref()
        .expect("timeout error recorded")
        .contains("timed out"));
    assert!(std::fs::metadata(
        node.diagnostic_refs
            .get("logs/run_slow/status.json")
            .expect("status ref")
    )
    .is_ok());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_timeout_captures_partial_logs_and_kills_child() {
    let workspace = temp_workspace("command-timeout-diagnostics");
    let marker = workspace.join("timed_out_child_marker");
    let command = format!(
        "printf 'before-timeout'; printf 'err-before-timeout' >&2; sleep 2; touch {}; printf 'after-timeout'",
        marker.display()
    );
    let manifest = manifest(&format!(
        r#"
id: command_timeout_diagnostics
version: 1
accepts: []
tools:
  - id: slow_tool
    executor: cli
    command: ["sh", "-c", {command:?}]
    timeout_secs: 1
nodes:
  - id: run_slow
    kind: tool
    tool: slow_tool
    required: true
"#
    ));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    let stdout = node
        .diagnostic_refs
        .get("logs/run_slow/stdout.log")
        .expect("stdout diagnostic ref");
    let stderr = node
        .diagnostic_refs
        .get("logs/run_slow/stderr.log")
        .expect("stderr diagnostic ref");
    assert_eq!(std::fs::read_to_string(stdout).unwrap(), "before-timeout");
    assert_eq!(
        std::fs::read_to_string(stderr).unwrap(),
        "err-before-timeout"
    );
    assert!(
        std::fs::metadata(&marker).is_err(),
        "timed out child should be killed before late side effects"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn generic_tool_runner_timeout_kills_descendant_processes_before_late_side_effect() {
    let workspace = temp_workspace("command-timeout-descendant");
    let marker = workspace.join("timed_out_descendant_marker");
    let command = format!(
        "printf 'before-timeout'; /bin/sh -c 'sleep 2; printf leaked > \"$1\"' child '{}'",
        marker.display()
    );
    let manifest = manifest(&format!(
        r#"
id: command_timeout_descendant
version: 1
accepts: []
tools:
  - id: slow_tool
    executor: cli
    command: ["sh", "-c", {command:?}]
    timeout_secs: 1
nodes:
  - id: run_slow
    kind: tool
    tool: slow_tool
    required: true
"#
    ));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert!(
        std::fs::metadata(&marker).is_err(),
        "timed out tool left a descendant process alive long enough to write {marker:?}"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_optional_timeout_degrades_dag() {
    let manifest = manifest(
        r#"
id: command_timeout
version: 1
accepts: []
tools:
  - id: slow_tool
    executor: cli
    command: ["sh", "-c", "sleep 2"]
    timeout_secs: 1
nodes:
  - id: run_slow
    kind: tool
    tool: slow_tool
"#,
    );
    let workspace = temp_workspace("command-timeout-optional");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Degraded);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Degraded);
    assert_eq!(node.exit_status, None);
    assert!(node.error.is_none());
    assert!(node
        .warning
        .as_deref()
        .expect("timeout warning recorded")
        .contains("timed out"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_approval_required_tool_pauses_without_spawning() {
    let manifest = manifest(
        r#"
id: command_approval
version: 1
accepts: []
tools:
  - id: gated_tool
    executor: cli
    command: ["sh", "-c", "printf 'should-not-run' > result.txt"]
    policy:
      approval_required: true
nodes:
  - id: run_gated
    kind: tool
    tool: gated_tool
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-approval");

    let paused = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(paused.status, DagNodeStatus::AwaitingApproval);
    let node = &paused.nodes[0];
    assert_eq!(node.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node.output_refs.is_empty());

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approval/gated_tool".to_string(), json!(true));
    let approved = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, approved_input)
        .await
        .expect("approved dag report is returned");

    assert_eq!(approved.status, DagNodeStatus::Ok);
    assert_eq!(
        std::fs::read_to_string(
            approved.nodes[0]
                .output_refs
                .get("result.txt")
                .expect("approved result ref")
        )
        .expect("approved command writes artifact"),
        "should-not-run"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_policy_error_takes_precedence_over_approval_pause() {
    let manifest = manifest(
        r#"
id: command_policy_before_approval
version: 1
accepts: []
tools:
  - id: gated_policy_tool
    executor: cli
    command: ["sh", "-c", "printf 'should-not-run' > result.txt"]
    policy:
      approval_required: true
      network:
        allow: false
nodes:
  - id: run_gated
    kind: tool
    tool: gated_policy_tool
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-policy-before-approval");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_ne!(node.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("isolation policy error")
        .contains("requires isolated runner"));
    assert!(!workspace.join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_approval_gate_pauses_and_resumes() {
    let manifest = manifest(
        r#"
id: approval_gate_runner
version: 1
accepts: []
tools:
  - id: human_release
    executor: approval_gate
nodes:
  - id: wait_for_release
    kind: tool
    tool: human_release
    required: true
"#,
    );
    let workspace = temp_workspace("approval-gate");

    let paused = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(paused.status, DagNodeStatus::AwaitingApproval);
    let node = &paused.nodes[0];
    assert_eq!(node.status, DagNodeStatus::AwaitingApproval);
    assert!(node
        .warning
        .as_deref()
        .expect("approval warning")
        .contains("approval/human_release"));

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approval/human_release".to_string(), json!(true));
    let approved = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, approved_input)
        .await
        .expect("approved dag report is returned");

    assert_eq!(approved.status, DagNodeStatus::Ok);
    assert_eq!(
        approved.outputs.values["wait_for_release"]["approved_key"],
        "approval/human_release"
    );
    assert_eq!(
        approved.outputs.values["wait_for_release"]["approved"],
        true
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_executes_http_get_and_records_response_artifact() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local http server");
    let addr = listener.local_addr().expect("local server addr");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept request");
        let mut buffer = [0_u8; 1024];
        let _ = socket.read(&mut buffer).await.expect("read request");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}",
            )
            .await
            .expect("write response");
    });
    let url = format!("http://{addr}/status");
    let manifest = manifest(&format!(
        r#"
id: http_runner
version: 1
accepts: []
tools:
  - id: fetch_status
    executor: http
    command: ["GET", "{url}"]
nodes:
  - id: run_fetch
    kind: tool
    tool: fetch_status
    outputs: [response.json]
    required: true
"#
    ));
    let workspace = temp_workspace("http-get");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("local http server received a request")
        .expect("local http server completed");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, Some(200));
    assert_eq!(node.command, Some(vec!["GET".to_string(), url.clone()]));
    let response_path = node
        .output_refs
        .get("response.json")
        .expect("response artifact ref");
    assert_eq!(
        std::fs::read_to_string(response_path).expect("response artifact is readable"),
        r#"{"ok":true}"#
    );
    assert_eq!(
        report.outputs.artifacts["response.json"]
            .media_type
            .as_deref(),
        Some("application/json")
    );
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_fetch/status.json"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_denies_http_when_network_policy_forbids_it() {
    let manifest = manifest(
        r#"
id: http_policy
version: 1
accepts: []
tools:
  - id: fetch_blocked
    executor: http
    command: ["GET", "http://127.0.0.1:9/blocked"]
    policy:
      network:
        allow: false
nodes:
  - id: run_blocked_fetch
    kind: tool
    tool: fetch_blocked
    outputs: [response.json]
    required: true
"#,
    );
    let workspace = temp_workspace("http-denied");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("network policy error")
        .contains("network policy denies"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_blocked_fetch/status.json"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_optional_http_network_deny_records_degraded_status_artifact() {
    let manifest = manifest(
        r#"
id: http_policy_optional
version: 1
accepts: []
tools:
  - id: fetch_blocked
    executor: http
    command: ["GET", "http://127.0.0.1:9/blocked"]
    policy:
      network:
        allow: false
nodes:
  - id: run_blocked_fetch
    kind: tool
    tool: fetch_blocked
    outputs: [response.json]
    required: false
"#,
    );
    let workspace = temp_workspace("http-denied-optional");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Degraded);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Degraded);
    assert_eq!(node.exit_status, None);
    assert!(node.error.is_none());
    assert!(node.output_refs.is_empty());
    assert!(node
        .warning
        .as_deref()
        .expect("network policy warning")
        .contains("network policy denies"));
    let status_ref = node
        .diagnostic_refs
        .get("logs/run_blocked_fetch/status.json")
        .expect("status diagnostic ref");
    let status_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(status_ref).expect("status diagnostic is readable"),
    )
    .expect("status diagnostic is json");
    assert_eq!(status_json["status"], json!("degraded"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn generic_tool_runner_records_llm_model_and_prompt_hash_for_command_backed_nodes() {
    let workspace = temp_workspace("llm-command");
    let adapter = workspace.join("llm-adapter");
    write_executable(
        &adapter,
        "#!/bin/sh\nprintf '%s\\n%s' \"$AGENTHERO_LLM_MODEL\" \"$AGENTHERO_LLM_PROMPT_HASH\" > \"$1\"\n",
    );
    let manifest = manifest(&format!(
        r#"
id: llm_runner
version: 1
accepts: []
tools:
  - id: summarize_with_adapter
    executor: llm
    command: ["{}", "completion.txt"]
nodes:
  - id: summarize
    kind: tool
    tool: summarize_with_adapter
    outputs: [completion.txt]
    required: true
"#,
        adapter.display()
    ));
    let mut input = DagIo::default();
    input
        .values
        .insert("model".to_string(), json!("gpt-test-runtime"));
    input.values.insert(
        "prompt".to_string(),
        json!("Summarize the runtime contract."),
    );

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let node = &report.nodes[0];
    assert_eq!(node.model.as_deref(), Some("gpt-test-runtime"));
    assert!(node
        .prompt_hash
        .as_deref()
        .expect("prompt hash recorded")
        .starts_with("fnv1a64:"));
    let completion = std::fs::read_to_string(
        node.output_refs
            .get("completion.txt")
            .expect("completion artifact ref"),
    )
    .expect("completion artifact is readable");
    assert!(completion.contains("gpt-test-runtime"));
    assert!(completion.contains(node.prompt_hash.as_deref().expect("prompt hash")));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn generic_tool_runner_reports_declared_executor_kind_separately_from_tool_id() {
    let workspace = temp_workspace("executor-provenance");
    let rust_tool = workspace.join("agenthero-test-rust-tool");
    write_executable(&rust_tool, "#!/bin/sh\nprintf 'ok' > translated.txt\n");
    let manifest = manifest(&format!(
        r#"
id: executor_provenance
version: 1
accepts: []
tools:
  - id: translate
    executor: rust_binary
    command: ["{}"]
nodes:
  - id: translate_node
    kind: tool
    tool: translate
    outputs: [translated.txt]
    required: true
"#,
        rust_tool.display()
    ));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    let node = &report.nodes[0];
    assert_eq!(node.tool.as_deref(), Some("translate"));
    assert_eq!(node.executor.as_deref(), Some("rust_binary"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn generic_tool_runner_executes_command_backed_runner_kinds_with_identity_env() {
    let workspace = temp_workspace("command-runner-kind-identities");
    let fake_bin = workspace.join("bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin exists");
    let fake_tool = "#!/bin/sh\nout=\"$1\"\nprintf '%s\\n%s\\n%s\\n%s\\n' \"$AGENTHERO_DAG_TYPE\" \"$AGENTHERO_NODE_ID\" \"$AGENTHERO_TOOL_ID\" \"$AGENTHERO_EXECUTOR_KIND\" > \"$out\"\n";
    for tool in [
        "python",
        "agenthero-test-rust-tool",
        "llm-adapter",
        "lean",
        "cabal",
        "docker",
        "wasmtime",
    ] {
        write_executable(&fake_bin.join(tool), fake_tool);
    }

    let manifest = manifest(&format!(
        r#"
id: generic_command_identities
version: 1
accepts: []
tools:
  - id: shell_tool
    executor: shell
    command: ["sh", "-c", "printf '%s\n%s\n%s\n%s\n' \"$AGENTHERO_DAG_TYPE\" \"$AGENTHERO_NODE_ID\" \"$AGENTHERO_TOOL_ID\" \"$AGENTHERO_EXECUTOR_KIND\" > shell.txt"]
  - id: python_tool
    executor: python
    command: ["{python}", "python.txt"]
  - id: rust_tool
    executor: rust_binary
    command: ["{rust}", "rust.txt"]
  - id: llm_tool
    executor: llm
    command: ["{llm}", "llm.txt"]
  - id: lean_tool
    executor: lean
    command: ["{lean}", "lean.txt"]
  - id: haskell_tool
    executor: haskell
    command: ["{haskell}", "haskell.txt"]
  - id: docker_tool
    executor: docker
    command: ["{docker}", "docker.txt"]
  - id: wasm_tool
    executor: wasm
    command: ["{wasm}", "wasm.txt"]
nodes:
  - id: run_shell
    kind: tool
    tool: shell_tool
    outputs: [shell.txt]
    required: true
  - id: run_python
    kind: tool
    tool: python_tool
    outputs: [python.txt]
    required: true
  - id: run_rust
    kind: tool
    tool: rust_tool
    outputs: [rust.txt]
    required: true
  - id: run_llm
    kind: tool
    tool: llm_tool
    outputs: [llm.txt]
    required: true
  - id: run_lean
    kind: tool
    tool: lean_tool
    outputs: [lean.txt]
    required: true
  - id: run_haskell
    kind: tool
    tool: haskell_tool
    outputs: [haskell.txt]
    required: true
  - id: run_docker
    kind: tool
    tool: docker_tool
    outputs: [docker.txt]
    required: true
  - id: run_wasm
    kind: tool
    tool: wasm_tool
    outputs: [wasm.txt]
    required: true
"#,
        python = fake_bin.join("python").display(),
        rust = fake_bin.join("agenthero-test-rust-tool").display(),
        llm = fake_bin.join("llm-adapter").display(),
        lean = fake_bin.join("lean").display(),
        haskell = fake_bin.join("cabal").display(),
        docker = fake_bin.join("docker").display(),
        wasm = fake_bin.join("wasmtime").display(),
    ));
    let mut input = DagIo::default();
    input
        .values
        .insert("model".to_string(), json!("local-identity-model"));
    input.values.insert(
        "prompt".to_string(),
        json!("Record the command-backed runner identity contract."),
    );

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    for (node_id, tool_id, executor, output) in [
        ("run_shell", "shell_tool", "shell", "shell.txt"),
        ("run_python", "python_tool", "python", "python.txt"),
        ("run_rust", "rust_tool", "rust_binary", "rust.txt"),
        ("run_llm", "llm_tool", "llm", "llm.txt"),
        ("run_lean", "lean_tool", "lean", "lean.txt"),
        ("run_haskell", "haskell_tool", "haskell", "haskell.txt"),
        ("run_docker", "docker_tool", "docker", "docker.txt"),
        ("run_wasm", "wasm_tool", "wasm", "wasm.txt"),
    ] {
        let node = report
            .nodes
            .iter()
            .find(|node| node.node_id == node_id)
            .unwrap_or_else(|| panic!("{node_id} report exists"));
        assert_eq!(node.status, DagNodeStatus::Ok, "{node_id} status");
        assert_eq!(node.tool.as_deref(), Some(tool_id), "{node_id} tool");
        assert_eq!(
            node.executor.as_deref(),
            Some(executor),
            "{node_id} executor"
        );
        assert_eq!(node.exit_status, Some(0), "{node_id} exit status");
        assert!(node
            .diagnostic_refs
            .contains_key(&format!("logs/{node_id}/status.json")));
        let output_ref = node
            .output_refs
            .get(output)
            .unwrap_or_else(|| panic!("{node_id} output artifact `{output}` is recorded"));
        let expected = format!("generic_command_identities\n{node_id}\n{tool_id}\n{executor}\n");
        assert_eq!(
            std::fs::read_to_string(output_ref).expect("identity output is readable"),
            expected,
            "{node_id} identity env"
        );
    }

    let llm_node = report
        .nodes
        .iter()
        .find(|node| node.node_id == "run_llm")
        .expect("llm node report exists");
    assert_eq!(llm_node.model.as_deref(), Some("local-identity-model"));
    assert!(llm_node
        .prompt_hash
        .as_deref()
        .expect("llm prompt hash recorded")
        .starts_with("fnv1a64:"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn generic_verifier_runner_passes_agenthero_identity_env_to_lean_tool() {
    let workspace = temp_workspace("lean-identity-env");
    let fake_bin = workspace.join("bin");
    std::fs::create_dir_all(&fake_bin).expect("fake bin exists");
    let lean = fake_bin.join("lean");
    write_executable(
        &lean,
        "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n%s\\n' \"$AGENTHERO_DAG_TYPE\" \"$AGENTHERO_NODE_ID\" \"$AGENTHERO_TOOL_ID\" \"$AGENTHERO_EXECUTOR_KIND\" > lean_env.txt\n",
    );
    let manifest = manifest(&format!(
        r#"
id: verifier_identity
version: 1
accepts: []
tools:
  - id: lean_kernel
    executor: lean
    command: ["{}", "Proof.lean"]
nodes:
  - id: check_lean
    kind: tool
    tool: lean_kernel
    outputs: [lean_env.txt]
    required: true
"#,
        lean.display()
    ));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let env_path = report.nodes[0]
        .output_refs
        .get("lean_env.txt")
        .expect("lean env output ref");
    assert_eq!(
        std::fs::read_to_string(env_path).expect("lean env output is readable"),
        "verifier_identity\ncheck_lean\nlean_kernel\nlean\n"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_optional_nonzero_exit_degrades_with_exit_status() {
    let manifest = manifest(
        r#"
id: command_degraded
version: 1
accepts: []
tools:
  - id: failing_optional
    executor: cli
    command: ["sh", "-c", "printf 'bad stderr' >&2; exit 7"]
nodes:
  - id: run_optional
    kind: tool
    tool: failing_optional
    required: false
"#,
    );
    let workspace = temp_workspace("command-degraded");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Degraded);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Degraded);
    assert_eq!(node.exit_status, Some(7));
    assert!(node.error.is_none());
    assert!(node
        .warning
        .as_deref()
        .expect("warning recorded")
        .contains("exited with status 7"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_optional/stderr.log"));
    assert_eq!(
        std::fs::read_to_string(
            node.diagnostic_refs
                .get("logs/run_optional/stderr.log")
                .expect("stderr ref")
        )
        .expect("stderr log written"),
        "bad stderr"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_fails_required_command_when_declared_output_is_missing() {
    let manifest = manifest(
        r#"
id: missing_declared_output
version: 1
accepts: []
tools:
  - id: writer
    executor: cli
    command: ["sh", "-c", "printf 'nothing useful'"]
nodes:
  - id: run_writer
    kind: tool
    tool: writer
    outputs: [proof.json]
    required: true
"#,
    );
    let workspace = temp_workspace("missing-required-output");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_eq!(node.exit_status, Some(0));
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("missing output error")
        .contains("missing declared output artifact `proof.json`"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_writer/status.json"));

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[cfg(unix)]
#[tokio::test]
async fn command_tool_terminal_event_includes_audit_provenance() {
    let workspace = temp_workspace("observable-command-provenance");
    let adapter = workspace.join("llm-adapter");
    write_executable(
        &adapter,
        "#!/bin/sh\nprintf '{\"ok\":true}' > result.json\n",
    );
    let manifest = manifest(&format!(
        r#"
id: observable_command_provenance
version: 1
accepts: []
tools:
  - id: writer
    executor: llm
    command: ["{}"]
nodes:
  - id: run_writer
    kind: tool
    tool: writer
    outputs: [result.json]
    required: true
"#,
        adapter.display()
    ));
    let mut input = DagIo::default();
    input
        .values
        .insert("model".to_string(), json!("local-test-model"));
    input.values.insert(
        "prompt".to_string(),
        json!("Summarize the observable command contract."),
    );

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    let event = report
        .events
        .iter()
        .find(|event| {
            event.event_type == "node.completed" && event.node_id.as_deref() == Some("run_writer")
        })
        .expect("terminal node event emitted");
    assert_eq!(event.payload["node_id"], json!("run_writer"));
    assert_eq!(event.payload["status"], json!("ok"));
    assert_eq!(event.payload["kind"], json!("tool"));
    assert_eq!(event.payload["attempt"], json!(1));
    assert_eq!(
        event.payload["command"],
        json!([adapter.display().to_string()])
    );
    assert_eq!(event.payload["exit_status"], json!(0));
    assert_eq!(event.payload["model"], json!("local-test-model"));
    assert!(event.payload["prompt_hash"]
        .as_str()
        .expect("prompt hash")
        .starts_with("fnv1a64:"));
    assert!(event.payload["output_refs"]["result.json"]
        .as_str()
        .expect("output artifact ref")
        .ends_with("/result.json"));
    assert!(
        event.payload["diagnostic_refs"]["logs/run_writer/stdout.log"]
            .as_str()
            .expect("stdout diagnostic ref")
            .ends_with("/stdout.log")
    );
    assert!(
        event.payload["diagnostic_refs"]["logs/run_writer/status.json"]
            .as_str()
            .expect("status diagnostic ref")
            .ends_with("/status.json")
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_mismatched_python_executor_command_without_spawning() {
    let manifest = manifest(
        r#"
id: executor_kind_boundary
version: 1
accepts: []
tools:
  - id: python_tool
    executor: python
    command: ["sh", "-c", "printf 'should-not-run' > result.txt"]
nodes:
  - id: run_python
    kind: tool
    tool: python_tool
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("executor-kind-python-boundary");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("executor mismatch error")
        .contains("executor `python` requires command"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_python/status.json"));
    assert!(!workspace.join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_mismatched_runner_kind_commands_without_spawning() {
    for (executor, program, expected) in [
        (
            "shell",
            "python",
            "executor `shell` requires command to start with a shell executable",
        ),
        (
            "rust_binary",
            "sh",
            "executor `rust_binary` requires command to start with a direct compiled binary",
        ),
        (
            "llm",
            "sh",
            "executor `llm` requires command to start with an LLM adapter",
        ),
        (
            "lean",
            "sh",
            "executor `lean` requires command to start with `lean` or `lake`",
        ),
        (
            "haskell",
            "sh",
            "executor `haskell` requires command to start with a Haskell toolchain executable",
        ),
        (
            "docker",
            "sh",
            "executor `docker` requires command to start with `docker`",
        ),
        (
            "wasm",
            "sh",
            "executor `wasm` requires command to start with a WebAssembly runtime",
        ),
    ] {
        let manifest = manifest(&format!(
            r#"
id: executor_kind_boundary
version: 1
accepts: []
tools:
  - id: checked_tool
    executor: {executor}
    command: ["{program}", "-c", "printf 'should-not-run' > result.txt"]
nodes:
  - id: run_checked
    kind: tool
    tool: checked_tool
    outputs: [result.txt]
    required: true
"#
        ));
        let workspace = temp_workspace(&format!("executor-kind-{executor}-boundary"));

        let report = DagExecutor::new(GenericToolRunner::new(&workspace))
            .execute(&manifest, DagIo::default())
            .await
            .expect("dag report is returned");

        assert_eq!(report.status, DagNodeStatus::Failed, "{executor} report");
        let node = &report.nodes[0];
        assert_eq!(node.status, DagNodeStatus::Failed, "{executor} status");
        assert_eq!(node.exit_status, None, "{executor} exit status");
        assert!(
            node.output_refs.is_empty(),
            "{executor} should not record outputs"
        );
        assert!(
            node.error
                .as_deref()
                .expect("executor mismatch error")
                .contains(expected),
            "{executor} error: {:?}",
            node.error
        );
        assert!(
            node.diagnostic_refs
                .contains_key("logs/run_checked/status.json"),
            "{executor} status diagnostic"
        );
        assert!(
            !workspace.join("result.txt").exists(),
            "{executor} should not spawn"
        );

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn generic_tool_runner_rejects_unsafe_container_and_wasm_flags_without_spawning() {
    for (executor, binary, flags, expected) in [
        (
            "docker",
            "docker",
            "\"run\", \"--privileged\", \"agenthero/test\"",
            "executor `docker` rejected unsafe isolation flag `--privileged`",
        ),
        (
            "docker",
            "docker",
            "\"run\", \"--network=host\", \"agenthero/test\"",
            "executor `docker` rejected unsafe isolation flag `--network=host`",
        ),
        (
            "wasm",
            "wasmtime",
            "\"--dir=/\", \"module.wasm\"",
            "executor `wasm` rejected unsafe isolation flag `--dir=/`",
        ),
    ] {
        let workspace = temp_workspace(&format!("unsafe-{executor}-{binary}"));
        let tool_path = workspace.join(binary);
        write_executable(&tool_path, "#!/bin/sh\nprintf 'spawned' > result.txt\n");
        let manifest = manifest(&format!(
            r#"
id: unsafe_tool_boundary
version: 1
accepts: []
tools:
  - id: checked_tool
    executor: {executor}
    command: ["{}", {flags}]
nodes:
  - id: run_checked
    kind: tool
    tool: checked_tool
    outputs: [result.txt]
    required: true
"#,
            tool_path.display()
        ));

        let report = DagExecutor::new(GenericToolRunner::new(&workspace))
            .execute(&manifest, DagIo::default())
            .await
            .expect("dag report is returned");

        assert_eq!(report.status, DagNodeStatus::Failed, "{executor} report");
        let node = &report.nodes[0];
        assert_eq!(node.status, DagNodeStatus::Failed, "{executor} status");
        assert_eq!(node.exit_status, None, "{executor} exit status");
        assert!(
            node.output_refs.is_empty(),
            "{executor} should not record outputs"
        );
        assert!(
            node.error
                .as_deref()
                .expect("unsafe isolation error")
                .contains(expected),
            "{executor} error: {:?}",
            node.error
        );
        assert!(
            !workspace.join("result.txt").exists(),
            "{executor} command should not spawn"
        );

        std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
    }
}

#[tokio::test]
async fn generic_tool_runner_passes_budget_policy_as_env() {
    let manifest = manifest(
        r#"
id: command_policy
version: 1
accepts: []
tools:
  - id: policy_tool
    executor: cli
    command: ["sh", "-c", "printf '%s\n%s' \"$AGENTHERO_NETWORK\" \"$AGENTHERO_BUDGET_UNITS\" > policy.txt"]
    policy:
      budget_units: 42
nodes:
  - id: run_policy
    kind: tool
    tool: policy_tool
    outputs: [policy.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-policy");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let path = report.nodes[0]
        .output_refs
        .get("policy.txt")
        .expect("policy artifact ref");
    let contents = std::fs::read_to_string(path).expect("policy artifact written");
    assert_eq!(contents, "allow\n42");

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_tool_when_budget_policy_exceeds_remaining_without_spawning() {
    let manifest = manifest(
        r#"
id: command_budget_policy
version: 1
accepts: []
tools:
  - id: budgeted_tool
    executor: cli
    command: ["sh", "-c", "printf 'should-not-run' > result.txt"]
    policy:
      budget_units: 5
nodes:
  - id: run_budgeted
    kind: tool
    tool: budgeted_tool
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-budget-policy-denied");
    let mut inputs = DagIo::default();
    inputs
        .values
        .insert("agenthero_budget_units_remaining".to_string(), json!(3));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, inputs)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("budget policy error")
        .contains("budget policy requires 5 units but only 3 remain"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_budgeted/status.json"));
    assert!(!workspace.join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn executor_carries_remaining_budget_between_budgeted_tool_nodes() {
    let manifest = manifest(
        r#"
id: command_budget_consumption
version: 1
accepts: []
tools:
  - id: first_budgeted_tool
    executor: cli
    command: ["sh", "-c", "printf 'first' > first.txt"]
    policy:
      budget_units: 2
  - id: second_budgeted_tool
    executor: cli
    command: ["sh", "-c", "printf 'second' > second.txt"]
    policy:
      budget_units: 2
nodes:
  - id: first
    kind: tool
    tool: first_budgeted_tool
    outputs: [first.txt]
    required: true
  - id: second
    kind: tool
    tool: second_budgeted_tool
    outputs: [second.txt]
    required: true
edges:
  - from: first
    to: second
"#,
    );
    let workspace = temp_workspace("command-budget-consumption");
    let mut inputs = DagIo::default();
    inputs
        .values
        .insert("agenthero_budget_units_remaining".to_string(), json!(3));

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, inputs)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(report.nodes.len(), 2);
    assert_eq!(report.nodes[0].status, DagNodeStatus::Ok);
    assert_eq!(report.nodes[1].status, DagNodeStatus::Failed);
    assert_eq!(report.nodes[1].exit_status, None);
    assert!(report.nodes[1].output_refs.is_empty());
    assert!(report.nodes[1]
        .error
        .as_deref()
        .expect("remaining budget error")
        .contains("budget policy requires 2 units but only 1 remain"));
    assert_eq!(
        report
            .outputs
            .values
            .get("agenthero_budget_units_remaining"),
        Some(&json!(1))
    );
    let first_path = report.nodes[0]
        .output_refs
        .get("first.txt")
        .expect("first artifact ref");
    assert_eq!(
        std::fs::read_to_string(first_path).expect("first artifact written"),
        "first"
    );

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_host_command_filesystem_write_policy_even_for_declared_outputs(
) {
    let manifest = manifest(
        r#"
id: command_policy_write
version: 1
accepts: []
tools:
  - id: policy_tool
    executor: cli
    command: ["sh", "-c", "mkdir -p allowed && printf 'payload' > allowed/result.txt"]
    policy:
      filesystem:
        write: ["allowed"]
nodes:
  - id: run_policy
    kind: tool
    tool: policy_tool
    outputs: [allowed/result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-policy-write-allowed");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.status, DagNodeStatus::Failed);
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("isolation policy error")
        .contains("requires isolated runner"));
    assert!(!workspace.join("allowed").join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_outputs_outside_filesystem_write_policy_without_spawning() {
    let manifest = manifest(
        r#"
id: command_policy_write
version: 1
accepts: []
tools:
  - id: policy_tool
    executor: cli
    command: ["sh", "-c", "mkdir -p denied && printf 'should-not-run' > denied/result.txt"]
    policy:
      filesystem:
        write: ["allowed"]
nodes:
  - id: run_policy
    kind: tool
    tool: policy_tool
    outputs: [denied/result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-policy-write-denied");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("isolation policy error")
        .contains("requires isolated runner"));
    assert!(node
        .diagnostic_refs
        .contains_key("logs/run_policy/status.json"));
    assert!(!workspace.join("denied").join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn generic_tool_runner_rejects_host_command_network_deny_without_spawning() {
    let manifest = manifest(
        r#"
id: command_policy_network
version: 1
accepts: []
tools:
  - id: policy_tool
    executor: cli
    command: ["sh", "-c", "printf 'should-not-run' > result.txt"]
    policy:
      network:
        allow: false
nodes:
  - id: run_policy
    kind: tool
    tool: policy_tool
    outputs: [result.txt]
    required: true
"#,
    );
    let workspace = temp_workspace("command-policy-network-denied");

    let report = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    let node = &report.nodes[0];
    assert_eq!(node.exit_status, None);
    assert!(node.output_refs.is_empty());
    assert!(node
        .error
        .as_deref()
        .expect("isolation policy error")
        .contains("requires isolated runner"));
    assert!(!workspace.join("result.txt").exists());

    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn required_node_failure_fails_dag_and_skips_descendants() {
    let manifest = manifest(
        r#"
id: sample
version: 1
accepts: []
tools:
  - id: required_tool
    executor: rust
nodes:
  - id: required_step
    kind: tool
    tool: required_tool
    required: true
  - id: child
    kind: artifact
    required: true
edges:
  - from: required_step
    to: child
"#,
    );
    let handler = RecordingHandler::default().fail("required_step");

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(handler.calls(), vec!["required_step"]);
    assert_eq!(
        report.node_status("required_step"),
        Some(DagNodeStatus::Failed)
    );
    assert_eq!(report.node_status("child"), Some(DagNodeStatus::Skipped));
}

#[tokio::test]
async fn optional_node_failure_degrades_dag_and_allows_descendants() {
    let manifest = manifest(
        r#"
id: sample
version: 1
accepts: []
tools:
  - id: optional_tool
    executor: rust
nodes:
  - id: optional_step
    kind: tool
    tool: optional_tool
  - id: child
    kind: artifact
    required: true
edges:
  - from: optional_step
    to: child
"#,
    );
    let handler = RecordingHandler::default().fail("optional_step");

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Degraded);
    assert_eq!(handler.calls(), vec!["optional_step", "child"]);
    assert_eq!(
        report.node_status("optional_step"),
        Some(DagNodeStatus::Degraded)
    );
    assert_eq!(report.node_status("child"), Some(DagNodeStatus::Ok));
}

#[tokio::test]
async fn retry_policy_retries_failed_node_and_reports_each_attempt() {
    let manifest = manifest(
        r#"
id: retryable
version: 1
accepts: []
nodes:
  - id: flaky
    kind: verify
    required: true
    retry:
      max_attempts: 2
      backoff_ms: 0
"#,
    );
    let handler = RetryThenSucceedHandler::default();
    let live_events = Arc::new(Mutex::new(Vec::new()));
    let captured_live_events = live_events.clone();

    let report = DagExecutor::new(handler.clone())
        .with_event_sink(move |event| {
            captured_live_events.lock().expect("event lock").push((
                event.event_type,
                event.node_id,
                event.payload,
            ));
        })
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(handler.attempts(), 2);
    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.node_status("flaky"), Some(DagNodeStatus::Ok));
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|node| (node.node_id.as_str(), node.attempt, node.status))
            .collect::<Vec<_>>(),
        vec![
            ("flaky", 1, DagNodeStatus::Failed),
            ("flaky", 2, DagNodeStatus::Ok),
        ]
    );
    assert_eq!(report.outputs.values["flaky"]["attempt"], 2);
    assert_eq!(report.nodes[0].policy["retry"]["max_attempts"], json!(2));
    let retry_event = report
        .events
        .iter()
        .find(|event| event.event_type == "node.retry_scheduled")
        .expect("retry scheduling is observable");
    assert_eq!(retry_event.node_id.as_deref(), Some("flaky"));
    assert_eq!(retry_event.payload["attempt"], json!(1));
    assert_eq!(retry_event.payload["next_attempt"], json!(2));
    assert_eq!(retry_event.payload["max_attempts"], json!(2));
    assert_eq!(retry_event.payload["backoff_ms"], json!(0));
    assert_eq!(
        retry_event.message.as_deref(),
        Some("flaky retry scheduled")
    );
    assert_eq!(
        report
            .events
            .iter()
            .filter(|event| event.node_id.as_deref() == Some("flaky"))
            .map(|event| (event.event_type.as_str(), event.payload["attempt"].clone()))
            .collect::<Vec<_>>(),
        vec![
            ("node.started", json!(1)),
            ("node.failed", json!(1)),
            ("node.retry_scheduled", json!(1)),
            ("node.started", json!(2)),
            ("node.completed", json!(2)),
        ]
    );
    assert_eq!(
        live_events
            .lock()
            .expect("event lock")
            .iter()
            .map(|(event_type, node_id, payload)| (
                event_type.as_str(),
                node_id.as_deref(),
                payload
                    .get("attempt")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            ))
            .collect::<Vec<_>>(),
        vec![
            ("dag.started", None, serde_json::Value::Null),
            ("node.started", Some("flaky"), json!(1)),
            ("node.failed", Some("flaky"), json!(1)),
            ("node.retry_scheduled", Some("flaky"), json!(1)),
            ("node.started", Some("flaky"), json!(2)),
            ("node.completed", Some("flaky"), json!(2)),
            ("dag.completed", None, serde_json::Value::Null),
        ]
    );
}

#[tokio::test]
async fn concurrent_layer_failure_takes_precedence_over_approval_pause() {
    let manifest = manifest(
        r#"
id: approval_failure
version: 1
concurrency: 2
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: needs_approval
    kind: verify
    required: true
  - id: fails_required
    kind: verify
    required: true
  - id: side_effect
    kind: artifact
    required: true
edges:
  - from: prepare
    to: [needs_approval, fails_required]
  - from: needs_approval
    to: side_effect
"#,
    );

    let report = DagExecutor::new(ApprovalAndFailureHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(
        report.node_status("needs_approval"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(
        report.node_status("fails_required"),
        Some(DagNodeStatus::Failed)
    );
    assert_eq!(
        report.node_status("side_effect"),
        Some(DagNodeStatus::Skipped)
    );
}

#[tokio::test]
async fn gate_node_evaluates_manifest_policy_without_app_handler() {
    let manifest = manifest(
        r#"
id: reviewish
version: 1
accepts: []
nodes:
  - id: a
    kind: artifact
    required: true
  - id: b
    kind: artifact
    required: true
  - id: c
    kind: artifact
  - id: quorum
    kind: gate
    gate:
      min_usable: 2
      sources: [a, b, c]
  - id: after_gate
    kind: artifact
    required: true
edges:
  - from: [a, b, c]
    to: quorum
  - from: quorum
    to: after_gate
"#,
    );
    let handler = RecordingHandler::default().degrade("c");

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Degraded);
    assert_eq!(handler.calls(), vec!["a", "b", "c", "after_gate"]);
    assert_eq!(report.node_status("quorum"), Some(DagNodeStatus::Ok));
}

#[tokio::test]
async fn branch_node_skips_unselected_direct_dependents() {
    let manifest = manifest(
        r#"
id: branchy
version: 1
accepts: []
nodes:
  - id: decide
    kind: branch
    branch:
      decision_key: route
      cases:
        publish: [publish]
        repair: [repair]
      default: [repair]
  - id: publish
    kind: artifact
    required: true
  - id: repair
    kind: artifact
    required: true
edges:
  - from: decide
    to: [publish, repair]
"#,
    );
    let mut input = DagIo::default();
    input.values.insert("route".to_string(), json!("publish"));
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, input)
        .await
        .expect("dag runs");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.calls(), vec!["publish"]);
    assert_eq!(report.node_status("decide"), Some(DagNodeStatus::Ok));
    assert_eq!(report.node_status("publish"), Some(DagNodeStatus::Ok));
    assert_eq!(report.node_status("repair"), Some(DagNodeStatus::Skipped));
    assert_eq!(
        report.outputs.values["decide"]["selected"],
        json!(["publish"])
    );
    let decide_report = report
        .nodes
        .iter()
        .find(|node| node.node_id == "decide")
        .expect("branch report");
    assert_eq!(decide_report.trace["decision"], json!("publish"));
    assert_eq!(decide_report.trace["selected"], json!(["publish"]));
}

#[tokio::test]
async fn map_node_runs_handler_once_per_input_item_with_bounds() {
    let manifest = manifest(
        r#"
id: fanout
version: 1
accepts: []
nodes:
  - id: process_items
    kind: map
    map:
      items_key: items
      item_key: item
      index_key: item_index
      max_items: 3
"#,
    );
    let mut input = DagIo::default();
    input.values.insert("items".to_string(), json!(["a", "b"]));
    let handler = MapRecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, input)
        .await
        .expect("dag runs");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.seen(), vec![(0, json!("a")), (1, json!("b"))]);
    assert_eq!(report.node_status("process_items"), Some(DagNodeStatus::Ok));
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "process_items#item-0",
            "process_items#item-1",
            "process_items"
        ]
    );
    assert_eq!(report.nodes[0].trace["map_index"], json!(0));
    assert_eq!(report.nodes[1].trace["map_index"], json!(1));
    assert_eq!(report.outputs.values["item_1"], json!({"processed": 1}));
}

#[tokio::test]
async fn map_node_awaiting_approval_pauses_dag_and_skips_descendants() {
    let manifest = manifest(
        r#"
id: fanout_approval
version: 1
accepts: []
nodes:
  - id: process_items
    kind: map
    map:
      items_key: items
      item_key: item
      max_items: 2
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: process_items
    to: publish
"#,
    );
    let mut input = DagIo::default();
    input.values.insert("items".to_string(), json!(["a"]));
    let handler = ApprovalPausingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(handler.calls(), vec!["process_items"]);
    assert_eq!(
        report.node_status("process_items#item-0"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(
        report.node_status("process_items"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(report.node_status("publish"), None);
}

#[tokio::test]
async fn map_node_item_and_aggregate_reports_keep_handler_provenance() {
    let manifest = manifest(
        r#"
id: fanout_provenance
version: 1
accepts: []
nodes:
  - id: process_items
    kind: map
    map:
      items_key: items
      item_key: item
      max_items: 1
    required: true
"#,
    );
    let mut input = DagIo::default();
    input.values.insert("items".to_string(), json!(["a"]));

    let report = DagExecutor::new(ProvenanceRecordingHandler)
        .execute(&manifest, input)
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let item = report
        .nodes
        .iter()
        .find(|node| node.node_id == "process_items#item-0")
        .expect("map item report");
    assert_eq!(item.model.as_deref(), Some("gpt-loop-test"));
    assert_eq!(item.prompt_hash.as_deref(), Some("fnv1a64:prompttest"));
    assert_eq!(
        item.command.as_deref(),
        Some(["llm-adapter".to_string(), "process_items".to_string()].as_slice())
    );
    assert_eq!(item.exit_status, Some(0));
    let aggregate = report
        .nodes
        .iter()
        .find(|node| node.node_id == "process_items")
        .expect("map aggregate report");
    assert_eq!(aggregate.model.as_deref(), Some("gpt-loop-test"));
    assert_eq!(aggregate.prompt_hash.as_deref(), Some("fnv1a64:prompttest"));
    assert_eq!(aggregate.exit_status, Some(0));
}

#[tokio::test]
async fn approval_node_pauses_until_declared_key_is_true() {
    let manifest = manifest(
        r#"
id: approval
version: 1
accepts: []
nodes:
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
  - id: publish
    kind: artifact
    required: true
edges:
  - from: human_review
    to: publish
"#,
    );
    let handler = RecordingHandler::default();

    let paused = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    assert_eq!(paused.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(
        paused.node_status("human_review"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(handler.calls(), Vec::<String>::new());

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approved".to_string(), json!(true));
    let approved = DagExecutor::new(handler.clone())
        .execute(&manifest, approved_input)
        .await
        .expect("approved dag runs");

    assert_eq!(approved.status, DagNodeStatus::Ok);
    assert_eq!(
        approved.node_status("human_review"),
        Some(DagNodeStatus::Ok)
    );
    assert_eq!(approved.node_status("publish"), Some(DagNodeStatus::Ok));
    assert_eq!(handler.calls(), vec!["publish"]);
}

#[tokio::test]
async fn checkpoint_replay_skips_completed_nodes_and_resumes_after_approval() {
    let manifest = manifest(
        r#"
id: approval_replay
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
  - id: publish
    kind: artifact
    inputs: [prepared.json]
    required: true
edges:
  - from: prepare
    to: human_review
  - from: human_review
    to: publish
"#,
    );
    let handler = RecordingHandler::default();

    let paused = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");

    assert_eq!(paused.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(handler.calls(), vec!["prepare"]);

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approved".to_string(), json!(true));
    let resumed = DagExecutor::new(handler.clone())
        .execute_with_checkpoint(&manifest, approved_input, Some(&paused))
        .await
        .expect("checkpoint resume succeeds");

    assert_eq!(resumed.status, DagNodeStatus::Ok);
    assert_eq!(handler.calls(), vec!["prepare", "publish"]);
    assert_eq!(resumed.node_status("prepare"), Some(DagNodeStatus::Ok));
    assert_eq!(resumed.node_status("human_review"), Some(DagNodeStatus::Ok));
    assert_eq!(resumed.node_status("publish"), Some(DagNodeStatus::Ok));
    assert!(resumed.outputs.values.contains_key("prepare"));
    assert_eq!(
        resumed
            .nodes
            .iter()
            .find(|node| node.node_id == "prepare")
            .and_then(|node| node.trace.get("replay"))
            .cloned(),
        Some(json!("checkpoint"))
    );
}

#[tokio::test]
async fn checkpoint_replay_preserves_loop_round_reports() {
    let manifest = manifest(
        r#"
id: loop_replay
version: 1
accepts: []
nodes:
  - id: repair
    kind: loop
    loop:
      max_rounds: 2
    required: true
"#,
    );
    let handler = LoopingHandler::stop_after(1);

    let completed = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("completed loop report is returned");

    assert_eq!(completed.status, DagNodeStatus::Ok);
    assert_eq!(
        completed
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["repair#round-1", "repair"]
    );

    let replayed = DagExecutor::new(handler.clone())
        .execute_with_checkpoint(&manifest, DagIo::default(), Some(&completed))
        .await
        .expect("loop checkpoint replay succeeds");

    assert_eq!(handler.rounds(), vec![1]);
    assert_eq!(
        replayed
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["repair#round-1", "repair"]
    );
    assert_eq!(
        replayed
            .nodes
            .iter()
            .find(|node| node.node_id == "repair#round-1")
            .and_then(|node| node.trace.get("replay"))
            .cloned(),
        Some(json!("checkpoint"))
    );
}

#[tokio::test]
async fn checkpoint_replay_emits_replayed_node_events_before_terminal_event() {
    let manifest = manifest(
        r#"
id: replay_events
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: prepare
    to: publish
"#,
    );
    let handler = RecordingHandler::default();

    let completed = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("completed report is returned");

    assert_eq!(completed.status, DagNodeStatus::Ok);
    assert_eq!(handler.calls(), vec!["prepare", "publish"]);

    let live_events = Arc::new(Mutex::new(Vec::new()));
    let live_events_sink = Arc::clone(&live_events);
    let replayed = DagExecutor::new(handler.clone())
        .with_event_sink(move |event| {
            live_events_sink.lock().expect("live events lock").push((
                event.event_type,
                event.node_id,
                event
                    .payload
                    .get("trace")
                    .and_then(|trace| trace.get("replay"))
                    .cloned(),
            ));
        })
        .execute_with_checkpoint(&manifest, DagIo::default(), Some(&completed))
        .await
        .expect("checkpoint replay succeeds");

    assert_eq!(replayed.status, DagNodeStatus::Ok);
    assert_eq!(
        handler.calls(),
        vec!["prepare", "publish"],
        "fully replayed nodes must not call the handler again"
    );
    assert_eq!(
        live_events.lock().expect("live events lock").as_slice(),
        &[
            ("dag.started".to_string(), None, None),
            (
                "node.started".to_string(),
                Some("prepare".to_string()),
                Some(json!("checkpoint"))
            ),
            (
                "node.completed".to_string(),
                Some("prepare".to_string()),
                Some(json!("checkpoint"))
            ),
            (
                "node.started".to_string(),
                Some("publish".to_string()),
                Some(json!("checkpoint"))
            ),
            (
                "node.completed".to_string(),
                Some("publish".to_string()),
                Some(json!("checkpoint"))
            ),
            ("dag.completed".to_string(), None, None),
        ]
    );
}

#[tokio::test]
async fn checkpoint_replay_rejects_manifest_hash_mismatch() {
    let first_manifest = manifest(
        r#"
id: approval_replay
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let changed_manifest = manifest(
        r#"
id: approval_replay
version: 2
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let paused = DagExecutor::new(RecordingHandler::default())
        .execute(&first_manifest, DagIo::default())
        .await
        .expect("paused report is returned");

    let error = DagExecutor::new(RecordingHandler::default())
        .execute_with_checkpoint(&changed_manifest, DagIo::default(), Some(&paused))
        .await
        .expect_err("manifest drift must reject checkpoint replay");

    assert!(format!("{error:#}").contains("checkpoint manifest hash"));
}

#[tokio::test]
async fn checkpoint_replay_rejects_undeclared_artifact_refs() {
    let manifest = manifest(
        r#"
id: approval_replay_artifacts
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let mut paused = DagExecutor::new(ArtifactProducingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    paused
        .nodes
        .iter_mut()
        .find(|node| node.node_id == "prepare")
        .expect("prepare checkpoint report")
        .output_refs
        .insert(
            "undeclared.json".to_string(),
            "file:///tmp/undeclared.json".to_string(),
        );

    let error = DagExecutor::new(RecordingHandler::default())
        .execute_with_checkpoint(&manifest, DagIo::default(), Some(&paused))
        .await
        .expect_err("undeclared replay artifact must be rejected");

    assert!(format!("{error:#}").contains("undeclared artifact output"));
}

#[tokio::test]
async fn checkpoint_replay_rejects_artifact_snapshot_drift() {
    let manifest = manifest(
        r#"
id: approval_replay_artifact_drift
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let mut paused = DagExecutor::new(ArtifactProducingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    paused
        .outputs
        .artifacts
        .get_mut("prepared.json")
        .expect("checkpoint final artifact")
        .uri = "file:///tmp/drifted-prepared.json".to_string();

    let error = DagExecutor::new(RecordingHandler::default())
        .execute_with_checkpoint(&manifest, DagIo::default(), Some(&paused))
        .await
        .expect_err("drifted replay artifact must be rejected");

    assert!(format!("{error:#}").contains("checkpoint artifact `prepared.json`"));
}

#[tokio::test]
async fn checkpoint_replay_rejects_same_uri_artifact_content_drift() {
    let manifest = manifest(
        r#"
id: approval_replay_artifact_content_drift
version: 1
accepts: []
tools:
  - id: prepare_tool
    executor: cli
    command: ["sh", "-c", "printf original > prepared.json"]
nodes:
  - id: prepare
    kind: tool
    tool: prepare_tool
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let workspace = temp_workspace("checkpoint-content-drift");
    let mut paused = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    assert_eq!(paused.status, DagNodeStatus::AwaitingApproval);
    let artifact = paused
        .outputs
        .artifacts
        .get_mut("prepared.json")
        .expect("checkpoint final artifact");
    artifact.metadata.insert(
        "sha256".to_string(),
        json!("0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5"),
    );
    artifact.metadata.insert("size_bytes".to_string(), json!(8));
    std::fs::write(&artifact.uri, "changed").expect("mutate artifact contents");

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approved".to_string(), json!(true));
    let error = DagExecutor::new(GenericToolRunner::new(&workspace))
        .execute_with_checkpoint(&manifest, approved_input, Some(&paused))
        .await
        .expect_err("same-uri content drift must reject checkpoint replay");

    assert!(format!("{error:#}").contains("content drift"));
    std::fs::remove_dir_all(workspace).expect("cleanup temp workspace");
}

#[tokio::test]
async fn checkpoint_replay_rejects_missing_frozen_artifact_snapshot() {
    let manifest = manifest(
        r#"
id: approval_replay_missing_artifact
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: prepare
    to: human_review
"#,
    );
    let mut paused = DagExecutor::new(ArtifactProducingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    paused.outputs.artifacts.remove("prepared.json");

    let error = DagExecutor::new(RecordingHandler::default())
        .execute_with_checkpoint(&manifest, DagIo::default(), Some(&paused))
        .await
        .expect_err("missing replay artifact snapshot must be rejected");

    assert!(format!("{error:#}").contains("missing from frozen outputs"));
}

#[tokio::test]
async fn checkpoint_replay_does_not_seed_unattributed_final_outputs() {
    let manifest = manifest(
        r#"
id: approval_replay_sanitized_outputs
version: 1
accepts: []
nodes:
  - id: prepare
    kind: prepare_inputs
    outputs: [prepared.json]
    required: true
  - id: human_review
    kind: approval
    approval:
      approved_key: approved
  - id: publish
    kind: artifact
    required: true
edges:
  - from: prepare
    to: human_review
  - from: human_review
    to: publish
"#,
    );
    let handler = RecordingHandler::default();
    let mut paused = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("paused report is returned");
    paused
        .outputs
        .values
        .insert("injected".to_string(), json!(true));
    paused.outputs.artifacts.insert(
        "injected.json".to_string(),
        ArtifactRef {
            uri: "file:///tmp/injected.json".to_string(),
            media_type: Some("application/json".to_string()),
            metadata: BTreeMap::new(),
        },
    );

    let mut approved_input = DagIo::default();
    approved_input
        .values
        .insert("approved".to_string(), json!(true));
    let resumed = DagExecutor::new(handler)
        .execute_with_checkpoint(&manifest, approved_input, Some(&paused))
        .await
        .expect("checkpoint resume succeeds");

    assert!(!resumed.outputs.values.contains_key("injected"));
    assert!(!resumed.outputs.artifacts.contains_key("injected.json"));
    assert!(resumed.outputs.values.contains_key("prepare"));
    let publish_inputs = resumed.outputs.values["publish"]["seen_inputs"]
        .as_array()
        .expect("publish seen input keys")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert!(!publish_inputs.contains(&"injected"));
}

#[tokio::test]
async fn dag_call_nodes_are_dispatched_like_other_app_nodes() {
    let manifest = manifest(
        r#"
id: parent
version: 1
accepts: []
nodes:
  - id: call_child
    kind: dag_call
    dag_type: child-dag
    required: true
"#,
    );
    let handler = RecordingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.calls(), vec!["call_child"]);
    assert_eq!(report.nodes[0].kind, "dag_call");
}

#[tokio::test]
async fn loop_node_records_each_round_and_stops_when_handler_clears_continue() {
    let manifest = manifest(
        r#"
id: parent
version: 1
accepts: []
tools:
  - id: repair_tool
    executor: rust
nodes:
  - id: repair
    kind: loop
    tool: repair_tool
    loop:
      max_rounds: 3
    required: true
"#,
    );
    let handler = LoopingHandler::stop_after(2);

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(handler.rounds(), vec![1, 2]);
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["repair#round-1", "repair#round-2", "repair"]
    );
    assert_eq!(report.node_status("repair"), Some(DagNodeStatus::Ok));
    assert_eq!(report.outputs.values["last_round"], json!(2));
}

#[tokio::test]
async fn loop_node_reports_have_matching_started_and_terminal_events() {
    let manifest = manifest(
        r#"
id: parent
version: 1
accepts: []
tools:
  - id: repair_tool
    executor: rust
nodes:
  - id: repair
    kind: loop
    tool: repair_tool
    loop:
      max_rounds: 3
    required: true
"#,
    );
    let handler = LoopingHandler::stop_after(1);
    let live_events = Arc::new(Mutex::new(Vec::new()));
    let live_events_sink = Arc::clone(&live_events);

    let report = DagExecutor::new(handler)
        .with_event_sink(move |event| {
            live_events_sink
                .lock()
                .expect("live events lock")
                .push(event.event_type);
        })
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    let reported_nodes = report
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();
    let started_nodes = report
        .events
        .iter()
        .filter(|event| event.event_type == "node.started")
        .filter_map(|event| event.node_id.clone())
        .collect::<BTreeSet<_>>();
    let terminal_nodes = report
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "node.completed" | "node.failed" | "node.skipped" | "node.awaiting_approval"
            )
        })
        .filter_map(|event| event.node_id.clone())
        .collect::<BTreeSet<_>>();

    assert_eq!(started_nodes, reported_nodes);
    assert_eq!(terminal_nodes, reported_nodes);
    assert_eq!(
        live_events.lock().expect("live events lock").as_slice(),
        &[
            "dag.started",
            "node.started",
            "node.completed",
            "node.started",
            "node.completed",
            "dag.completed",
        ]
    );
}

#[tokio::test]
async fn loop_node_awaiting_approval_pauses_dag_and_skips_descendants() {
    let manifest = manifest(
        r#"
id: loop_approval
version: 1
accepts: []
nodes:
  - id: repair
    kind: loop
    loop:
      max_rounds: 2
    required: true
  - id: publish
    kind: artifact
    required: true
edges:
  - from: repair
    to: publish
"#,
    );
    let handler = ApprovalPausingHandler::default();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::AwaitingApproval);
    assert_eq!(handler.calls(), vec!["repair"]);
    assert_eq!(
        report.node_status("repair#round-1"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(
        report.node_status("repair"),
        Some(DagNodeStatus::AwaitingApproval)
    );
    assert_eq!(report.node_status("publish"), None);
}

#[tokio::test]
async fn loop_node_round_and_aggregate_reports_keep_handler_provenance() {
    let manifest = manifest(
        r#"
id: loop_provenance
version: 1
accepts: []
nodes:
  - id: repair
    kind: loop
    loop:
      max_rounds: 2
    required: true
"#,
    );

    let report = DagExecutor::new(ProvenanceRecordingHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let round = report
        .nodes
        .iter()
        .find(|node| node.node_id == "repair#round-1")
        .expect("loop round report");
    assert_eq!(round.model.as_deref(), Some("gpt-loop-test"));
    assert_eq!(round.prompt_hash.as_deref(), Some("fnv1a64:prompttest"));
    assert_eq!(
        round.command.as_deref(),
        Some(["llm-adapter".to_string(), "repair".to_string()].as_slice())
    );
    assert_eq!(round.exit_status, Some(0));
    let aggregate = report
        .nodes
        .iter()
        .find(|node| node.node_id == "repair")
        .expect("loop aggregate report");
    assert_eq!(aggregate.model.as_deref(), Some("gpt-loop-test"));
    assert_eq!(aggregate.prompt_hash.as_deref(), Some("fnv1a64:prompttest"));
    assert_eq!(aggregate.exit_status, Some(0));
}

#[tokio::test]
async fn loop_node_trace_merge_preserves_executor_keys_when_handler_collides() {
    let manifest = manifest(
        r#"
id: loop_trace_collision
version: 1
accepts: []
nodes:
  - id: repair
    kind: loop
    loop:
      max_rounds: 1
    required: true
"#,
    );

    let report = DagExecutor::new(TraceCollisionHandler)
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Ok);
    let round = report
        .nodes
        .iter()
        .find(|node| node.node_id == "repair#round-1")
        .expect("loop round report");
    assert_eq!(round.trace["loop_round"], json!(1));
    assert_eq!(round.trace["handler_only"], json!(true));
    assert_eq!(
        round.trace["app_trace"]["loop_round"],
        json!("handler-loop-round")
    );
}

#[tokio::test]
async fn required_loop_node_fails_when_max_rounds_are_exhausted() {
    let manifest = manifest(
        r#"
id: parent
version: 1
accepts: []
tools:
  - id: repair_tool
    executor: rust
nodes:
  - id: repair
    kind: loop
    tool: repair_tool
    loop:
      max_rounds: 2
    required: true
"#,
    );
    let handler = LoopingHandler::never_stop();

    let report = DagExecutor::new(handler.clone())
        .execute(&manifest, DagIo::default())
        .await
        .expect("dag report is returned");

    assert_eq!(report.status, DagNodeStatus::Failed);
    assert_eq!(handler.rounds(), vec![1, 2]);
    assert_eq!(report.node_status("repair"), Some(DagNodeStatus::Failed));
    let final_report = report.nodes.last().expect("final loop report");
    assert!(final_report
        .error
        .as_ref()
        .expect("max round error")
        .contains("max_rounds"));
}
