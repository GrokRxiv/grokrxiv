use std::fs;
use std::path::PathBuf;

use grokrxiv_dag_runtime::{AgentKind, DagManifest, DagNodeKind, DagRoleKey, DagTypeId, RoleId};

fn write_temp_file(name: &str, contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join(name), contents).expect("write manifest");
    dir
}

#[test]
fn loads_manifest_and_computes_execution_layers() {
    let dir = write_temp_file(
        "paper-review.yaml",
        r#"
id: paper-review
version: 1
accepts: [critic, synthesizer]
concurrency: 4
roles:
  - id: summary
    kind: critic
    config: agents/paper-review/summary.yaml
  - id: meta_reviewer
    kind: synthesizer
    config: agents/paper-review/meta_reviewer.yaml
nodes:
  - id: prepare
    kind: prepare_inputs
  - id: summary
    kind: agent
    role: summary
  - id: meta_reviewer
    kind: synthesizer
    role: meta_reviewer
edges:
  - from: prepare
    to: [summary]
  - from: summary
    to: [meta_reviewer]
"#,
    );

    let manifest = DagManifest::from_path(dir.path().join("paper-review.yaml")).unwrap();

    assert_eq!(manifest.id.as_str(), "paper-review");
    assert_eq!(
        manifest.accepts,
        vec![AgentKind::Critic, AgentKind::Synthesizer]
    );
    assert_eq!(
        manifest.execution_layers().unwrap(),
        vec![
            vec!["prepare".to_string()],
            vec!["summary".to_string()],
            vec!["meta_reviewer".to_string()],
        ]
    );
}

#[test]
fn dag_role_key_qualifies_dag_and_role_ids() {
    let key = DagRoleKey::new(DagTypeId::new("paper-review"), RoleId::new("summary"));

    assert_eq!(key.as_str(), "paper-review.summary");
    assert_eq!(key.dag_type(), "paper-review");
    assert_eq!(key.role_id(), "summary");
}

#[test]
fn loads_tool_nodes_and_top_level_tool_definitions() {
    let manifest = DagManifest::from_str(
        r#"
id: paper-extract-tex
version: 1
accepts: [extractor]
concurrency: 2
tools:
  - id: source_to_body
    executor: rust
    command: null
    timeout_secs: 30
nodes:
  - id: acquire_source
    kind: ingest_source
  - id: source_to_body
    kind: tool
    tool: source_to_body
    inputs: [source.tar.gz]
    outputs: [body.md, semantic_ast.json]
    required: true
  - id: paper_review
    kind: dag_call
    dag_type: paper-review
edges:
  - from: acquire_source
    to: source_to_body
  - from: source_to_body
    to: paper_review
"#,
    )
    .unwrap();

    assert_eq!(manifest.tools.len(), 1);
    assert_eq!(manifest.tools[0].id.as_str(), "source_to_body");
    assert_eq!(manifest.nodes[1].kind, DagNodeKind::Tool);
    assert_eq!(manifest.nodes[1].tool.as_deref(), Some("source_to_body"));
    assert_eq!(
        manifest.nodes[1].outputs,
        vec!["body.md", "semantic_ast.json"]
    );
    assert!(manifest.nodes[1].required);
    assert_eq!(manifest.nodes[2].dag_type.as_deref(), Some("paper-review"));
}

#[test]
fn rejects_unknown_node_kind() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: [critic]
nodes:
  - id: surprise
    kind: drone_strike
"#,
    )
    .expect_err("unknown node kind should be rejected");

    assert!(err.to_string().contains("drone_strike"));
}

#[test]
fn loads_feeds_meta_and_gate_policy() {
    let manifest = DagManifest::from_str(
        r#"
id: paper-review
version: 1
accepts: [critic]
roles:
  - id: summary
    kind: critic
    config: agents/paper-review/summary.yaml
nodes:
  - id: summary
    kind: agent
    role: summary
    feeds_meta: true
  - id: specialist_quorum
    kind: gate
    gate:
      min_usable: 1
      sources: [summary]
edges:
  - from: summary
    to: specialist_quorum
"#,
    )
    .unwrap();

    assert_eq!(manifest.nodes[0].kind, DagNodeKind::Agent);
    assert!(manifest.nodes[0].feeds_meta);
    let gate = manifest.nodes[1]
        .gate
        .as_ref()
        .expect("gate policy should load");
    assert_eq!(gate.min_usable, Some(1));
    assert_eq!(gate.sources, vec!["summary".to_string()]);
}

#[test]
fn rejects_tool_node_without_tool_reference() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: [extractor]
tools:
  - id: source_to_body
    executor: rust
nodes:
  - id: source_to_body
    kind: tool
"#,
    )
    .expect_err("tool node must point at a registered tool");

    assert!(err.to_string().contains("tool node"));
    assert!(err.to_string().contains("source_to_body"));
}

#[test]
fn rejects_agent_kind_not_accepted_by_dag() {
    let dir = write_temp_file(
        "paper-review.yaml",
        r#"
id: paper-review
version: 1
accepts: [critic]
roles:
  - id: patch_author
    kind: code_generator
    config: agents/paper-review/patch_author.yaml
nodes:
  - id: patch_author
    kind: agent
    role: patch_author
"#,
    );

    let err = DagManifest::from_path(dir.path().join("paper-review.yaml"))
        .expect_err("kind should be rejected");

    assert!(err.to_string().contains("code_generator"));
    assert!(err.to_string().contains("paper-review"));
}

#[test]
fn rejects_cycles() {
    let dir = write_temp_file(
        "bad.yaml",
        r#"
id: bad
version: 1
accepts: [critic]
roles:
  - id: a
    kind: critic
    config: agents/bad/a.yaml
nodes:
  - id: a
    kind: agent
    role: a
  - id: b
    kind: artifact
edges:
  - from: a
    to: [b]
  - from: b
    to: [a]
"#,
    );

    let err = DagManifest::from_path(dir.path().join("bad.yaml")).expect_err("cycle rejected");

    assert!(err.to_string().contains("cycle"));
}

#[test]
fn compatible_dags_are_selected_by_agent_kind() {
    let review = DagManifest::from_str(
        r#"
id: paper-review
version: 1
accepts: [critic, type_theory_validator]
roles: []
nodes: []
"#,
    )
    .unwrap();
    let revise = DagManifest::from_str(
        r#"
id: paper-revise
version: 1
accepts: [code_generator, synthesizer]
roles: []
nodes: []
"#,
    )
    .unwrap();

    let manifests = vec![review, revise];

    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::TypeTheoryValidator),
        vec!["paper-review".to_string()]
    );
    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::CodeGenerator),
        vec!["paper-revise".to_string()]
    );
}

#[test]
fn repo_manifests_validate_and_expose_expected_capabilities() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest_paths = [
        root.join("dags/paper-review.yaml"),
        root.join("dags/paper-extract.yaml"),
        root.join("dags/paper-revise.yaml"),
        root.join("dags/c-to-rust.yaml"),
    ];
    let manifests = manifest_paths
        .iter()
        .map(DagManifest::from_path)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::TypeTheoryValidator),
        vec!["paper-review".to_string()]
    );
    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::CodeGenerator),
        vec!["paper-revise".to_string(), "c-to-rust".to_string()]
    );
    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::Extractor),
        vec!["paper-extract".to_string(), "c-to-rust".to_string()]
    );
}
