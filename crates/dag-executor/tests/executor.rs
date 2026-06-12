use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use agenthero_dag_executor::{
    ArtifactRef, DagExecutor, DagIo, NodeExecutionContext, NodeExecutionResult, NodeHandler,
    LOOP_ROUND_INPUT, MAP_INDEX_INPUT,
};
use agenthero_dag_runtime::{DagManifest, DagNodeStatus};
use async_trait::async_trait;
use serde_json::json;

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

impl MapRecordingHandler {
    fn seen(&self) -> Vec<(u64, serde_json::Value)> {
        self.seen.lock().expect("seen lock").clone()
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
