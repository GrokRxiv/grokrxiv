use std::fs;
use std::path::PathBuf;

use agenthero_dag_runtime::{
    AgentKind, DagManifest, DagNodeKind, DagRoleKey, DagTypeId, RoleId, ToolExecutorKind,
};

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
fn loads_tool_handler_metadata_and_cli_commands() {
    let manifest = DagManifest::from_str(
        r#"
id: citation-validation
version: 1
accepts: [verifier]
concurrency: 1
tools:
  - id: bibtex_reference_parser
    executor: rust
    handler: citation_validation::bibtex_reference_parser
    timeout_secs: 30
  - id: external_citation_audit
    executor: cli
    command: [citation-audit, --json]
    timeout_secs: 60
nodes:
  - id: bibtex_reference_parser
    kind: tool
    tool: bibtex_reference_parser
  - id: external_citation_audit
    kind: tool
    tool: external_citation_audit
edges:
  - from: bibtex_reference_parser
    to: external_citation_audit
"#,
    )
    .unwrap();

    assert_eq!(manifest.tools[0].executor, ToolExecutorKind::Rust);
    assert_eq!(
        manifest.tools[0].handler.as_deref(),
        Some("citation_validation::bibtex_reference_parser")
    );
    assert_eq!(manifest.tools[1].executor, ToolExecutorKind::Cli);
    assert_eq!(
        manifest.tools[1].command.as_deref(),
        Some(["citation-audit".to_string(), "--json".to_string()].as_slice())
    );
}

#[test]
fn loads_generic_tool_runner_kinds_and_policy_contracts() {
    let manifest = DagManifest::from_str(
        r#"
id: generic-runners
version: 1
accepts: []
tools:
  - id: shell_step
    executor: shell
    command: ["sh", "-c", "echo ok"]
    timeout_secs: 10
    policy:
      budget_units: 5
      approval_required: true
      network:
        allow: false
      filesystem:
        read: ["."]
        write: [".agenthero"]
  - id: python_step
    executor: python
    command: ["python", "-m", "tool"]
  - id: rust_step
    executor: rust_binary
    command: ["target/debug/tool"]
  - id: llm_step
    executor: llm
    command: ["llm-adapter", "--json"]
  - id: http_step
    executor: http
    command: ["GET", "https://example.invalid"]
  - id: lean_step
    executor: lean
    command: ["lean", "Proof.lean"]
  - id: haskell_step
    executor: haskell
    command: ["cabal", "test"]
  - id: docker_step
    executor: docker
    command: ["docker", "run", "--rm", "image"]
  - id: wasm_step
    executor: wasm
    command: ["wasmtime", "tool.wasm"]
  - id: approval_step
    executor: approval_gate
nodes:
  - id: shell_step
    kind: tool
    tool: shell_step
"#,
    )
    .unwrap();

    assert_eq!(manifest.tools[0].executor, ToolExecutorKind::Shell);
    assert_eq!(manifest.tools[1].executor, ToolExecutorKind::Python);
    assert_eq!(manifest.tools[2].executor, ToolExecutorKind::RustBinary);
    assert_eq!(manifest.tools[3].executor, ToolExecutorKind::Llm);
    assert_eq!(manifest.tools[4].executor, ToolExecutorKind::Http);
    assert_eq!(manifest.tools[5].executor, ToolExecutorKind::Lean);
    assert_eq!(manifest.tools[6].executor, ToolExecutorKind::Haskell);
    assert_eq!(manifest.tools[7].executor, ToolExecutorKind::Docker);
    assert_eq!(manifest.tools[8].executor, ToolExecutorKind::Wasm);
    assert_eq!(manifest.tools[9].executor, ToolExecutorKind::ApprovalGate);

    let policy = manifest.tools[0].policy.as_ref().expect("tool policy");
    assert_eq!(policy.budget_units, Some(5));
    assert!(policy.approval_required);
    assert!(!policy.network.allow);
    assert_eq!(policy.filesystem.read, vec!["."]);
    assert_eq!(policy.filesystem.write, vec![".agenthero"]);
}

#[test]
fn rejects_tool_schema_string_references_in_dag_manifests() {
    let err = DagManifest::from_str(
        r#"
id: bad_schema_ref
version: 1
accepts: []
tools:
  - id: string_schema_tool
    executor: rust
    input_schema: schemas/input.schema.json
nodes:
  - id: string_schema_tool
    kind: tool
    tool: string_schema_tool
"#,
    )
    .expect_err("DAG tool schemas must be inline objects, not path strings");

    assert!(err.to_string().contains("input_schema"));
    assert!(err.to_string().contains("string_schema_tool"));
}

#[test]
fn rejects_unsafe_node_output_artifact_keys() {
    for output in [
        "../outside.txt",
        "/tmp/out.txt",
        "review_loop//out.json",
        "review_loop/../out.json",
    ] {
        let err = DagManifest::from_str(&format!(
            r#"
id: unsafe_output
version: 1
accepts: []
nodes:
  - id: write_output
    kind: artifact
    outputs: ["{output}"]
"#
        ))
        .expect_err("unsafe output artifact keys must be rejected");

        assert!(err.to_string().contains("unsafe artifact key"));
        assert!(err.to_string().contains("write_output"));
    }
}

#[test]
fn rejects_unsafe_filesystem_policy_paths() {
    for (field, value) in [
        ("read", "../secret"),
        ("write", "/tmp/output"),
        ("write", "nested/../output"),
    ] {
        let err = DagManifest::from_str(&format!(
            r#"
id: unsafe_policy
version: 1
accepts: []
tools:
  - id: unsafe_tool
    executor: rust
    policy:
      filesystem:
        {field}: ["{value}"]
nodes:
  - id: unsafe_tool
    kind: tool
    tool: unsafe_tool
"#
        ))
        .expect_err("unsafe filesystem policy paths must be rejected");

        assert!(err.to_string().contains("filesystem policy"));
        assert!(err.to_string().contains("unsafe_tool"));
        assert!(err.to_string().contains(value));
    }
}

#[test]
fn rejects_cli_tool_without_command() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: [verifier]
tools:
  - id: external_citation_audit
    executor: cli
nodes:
  - id: external_citation_audit
    kind: tool
    tool: external_citation_audit
"#,
    )
    .expect_err("command-backed tools must declare a command");

    assert!(err.to_string().contains("command-backed tool"));
    assert!(err.to_string().contains("external_citation_audit"));
}

#[test]
fn rejects_llm_tool_without_command() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: []
tools:
  - id: review_agent
    executor: llm
nodes:
  - id: review_agent
    kind: tool
    tool: review_agent
"#,
    )
    .expect_err("LLM tools must declare the command adapter they invoke");

    assert!(err.to_string().contains("command-backed tool"));
    assert!(err.to_string().contains("review_agent"));
}

#[test]
fn rejects_http_tool_with_invalid_command_contract() {
    for (command, expected) in [
        (r#"["GET"]"#, "declared as [METHOD, URL, ...]"),
        (
            r#"["POST", "https://example.invalid/write"]"#,
            "GET-only until unsafe methods have an explicit policy gate",
        ),
        (
            r#"["GET", "file:///tmp/input.json"]"#,
            "an http:// or https:// URL",
        ),
    ] {
        let err = DagManifest::from_str(&format!(
            r#"
id: bad
version: 1
accepts: []
tools:
  - id: fetch_metadata
    executor: http
    command: {command}
nodes:
  - id: fetch_metadata
    kind: tool
    tool: fetch_metadata
"#,
        ))
        .expect_err("HTTP tools must declare a bounded GET contract");

        assert!(err.to_string().contains("http tool `fetch_metadata`"));
        assert!(
            err.to_string().contains(expected),
            "expected `{expected}` in `{err}`"
        );
    }
}

#[test]
fn rejects_zero_concurrency() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: []
concurrency: 0
nodes:
  - id: step
    kind: artifact
"#,
    )
    .expect_err("zero concurrency must be rejected");

    assert!(err.to_string().contains("concurrency"));
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
fn loads_bounded_loop_node_policy() {
    let manifest = DagManifest::from_str(
        r#"
id: review-loop
version: 1
accepts: [verifier]
tools:
  - id: review_fix_code
    executor: rust
nodes:
  - id: haskell_review_fix_code
    kind: loop
    tool: review_fix_code
    loop:
      max_rounds: 3
      continue_key: review_loop/continue
    required: true
"#,
    )
    .unwrap();

    let node = &manifest.nodes[0];
    assert_eq!(node.kind, DagNodeKind::Loop);
    let policy = node.loop_policy.as_ref().expect("loop policy loads");
    assert_eq!(policy.max_rounds, 3);
    assert_eq!(policy.continue_key, "review_loop/continue");
}

#[test]
fn loads_node_retry_policy() {
    let manifest = DagManifest::from_str(
        r#"
id: retryable
version: 1
accepts: []
nodes:
  - id: flaky_compile
    kind: verify
    required: true
    retry:
      max_attempts: 3
      backoff_ms: 25
"#,
    )
    .unwrap();

    let retry = manifest.nodes[0]
        .retry
        .as_ref()
        .expect("retry policy loads");
    assert_eq!(retry.max_attempts, 3);
    assert_eq!(retry.backoff_ms, 25);
}

#[test]
fn rejects_retry_policy_with_zero_max_attempts() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: []
nodes:
  - id: never_runs
    kind: verify
    retry:
      max_attempts: 0
"#,
    )
    .expect_err("retry policy must have a nonzero attempt bound");

    assert!(err.to_string().contains("retry"));
    assert!(err.to_string().contains("max_attempts"));
}

#[test]
fn rejects_retry_policy_on_structured_control_flow_nodes() {
    for kind in ["loop", "map"] {
        let policy = if kind == "loop" {
            "loop:\n      max_rounds: 1"
        } else {
            "map:\n      items_key: items\n      max_items: 1"
        };
        let err = DagManifest::from_str(&format!(
            r#"
id: bad
version: 1
accepts: []
nodes:
  - id: dynamic
    kind: {kind}
    {policy}
    retry:
      max_attempts: 2
"#,
        ))
        .expect_err("structured control-flow retry must be explicit before it is accepted");

        assert!(err.to_string().contains("retry"));
        assert!(err.to_string().contains(kind));
    }
}

#[test]
fn loads_branch_map_and_approval_node_policies() {
    let manifest = DagManifest::from_str(
        r#"
id: agentapp
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
  - id: repair
    kind: artifact
  - id: fanout
    kind: map
    map:
      items_key: items
      item_key: item
      index_key: item_index
      max_items: 4
  - id: human_gate
    kind: approval
    approval:
      approved_key: approved
edges:
  - from: decide
    to: [publish, repair]
  - from: publish
    to: fanout
  - from: fanout
    to: human_gate
"#,
    )
    .unwrap();

    let decide = manifest
        .nodes
        .iter()
        .find(|node| node.id == "decide")
        .expect("branch node");
    assert_eq!(decide.kind, DagNodeKind::Branch);
    let branch = decide.branch.as_ref().expect("branch policy");
    assert_eq!(branch.decision_key, "route");
    assert_eq!(branch.cases["publish"], vec!["publish".to_string()]);
    assert_eq!(branch.default, vec!["repair".to_string()]);

    let fanout = manifest
        .nodes
        .iter()
        .find(|node| node.id == "fanout")
        .expect("map node");
    assert_eq!(fanout.kind, DagNodeKind::Map);
    let map = fanout.map.as_ref().expect("map policy");
    assert_eq!(map.items_key, "items");
    assert_eq!(map.item_key, "item");
    assert_eq!(map.index_key, "item_index");
    assert_eq!(map.max_items, 4);

    let approval = manifest
        .nodes
        .iter()
        .find(|node| node.id == "human_gate")
        .expect("approval node");
    assert_eq!(approval.kind, DagNodeKind::Approval);
    assert_eq!(
        approval
            .approval
            .as_ref()
            .expect("approval policy")
            .approved_key,
        "approved"
    );
}

#[test]
fn rejects_dynamic_nodes_without_required_policies() {
    for (kind, expected) in [
        ("branch", "branch"),
        ("map", "map"),
        ("approval", "approval"),
    ] {
        let err = DagManifest::from_str(&format!(
            r#"
id: bad
version: 1
accepts: []
nodes:
  - id: dynamic
    kind: {kind}
"#,
        ))
        .expect_err("dynamic nodes require policies");
        assert!(
            err.to_string().contains(expected),
            "{kind}: expected {expected} policy error, got {err:#}"
        );
    }
}

#[test]
fn rejects_loop_node_without_bounded_policy() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: []
nodes:
  - id: hidden_loop
    kind: loop
"#,
    )
    .expect_err("loop nodes must declare bounds");

    assert!(err.to_string().contains("loop"));
    assert!(err.to_string().contains("policy"));
}

#[test]
fn rejects_loop_node_with_zero_max_rounds() {
    let err = DagManifest::from_str(
        r#"
id: bad
version: 1
accepts: []
nodes:
  - id: hidden_loop
    kind: loop
    loop:
      max_rounds: 0
"#,
    )
    .expect_err("loop nodes must have a nonzero bound");

    assert!(err.to_string().contains("max_rounds"));
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
        root.join("agenthero/apps/grokrxiv/dags/paper-review.yaml"),
        root.join("agenthero/apps/grokrxiv/dags/paper-extract.yaml"),
        root.join("agenthero/apps/grokrxiv/dags/citation-validation.yaml"),
        root.join("agenthero/apps/grokrxiv/dags/paper-revise.yaml"),
        root.join("agenthero/apps/c2rust/dags/c2rust.yaml"),
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
        vec!["paper-revise".to_string(), "c2rust".to_string()]
    );
    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::Extractor),
        vec!["paper-extract".to_string(), "c2rust".to_string()]
    );
    assert_eq!(
        DagManifest::compatible_dag_ids(&manifests, AgentKind::Verifier),
        vec![
            "paper-review".to_string(),
            "citation-validation".to_string(),
            "c2rust".to_string(),
        ]
    );
}
