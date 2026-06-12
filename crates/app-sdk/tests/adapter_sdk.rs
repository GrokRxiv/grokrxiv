use agenthero_agent_runtime::{AppAdapterRequest, APP_ADAPTER_PROTOCOL};
use agenthero_app_sdk::{
    app_root_from_manifest_dir, dag_manifest_path, load_dag_manifest, parse_adapter_request,
    read_adapter_request, resolve_app_root, resolve_runtime_binary, response_to_json,
    write_adapter_response,
};
use agenthero_dag_executor::DagIo;

#[test]
fn parses_request_and_serializes_response_with_shared_protocol() {
    let request = AppAdapterRequest::new(
        "demo",
        "run",
        "demo-dag",
        vec!["input.txt".to_string()],
        DagIo::default(),
        true,
        false,
    );
    let json = serde_json::to_string(&request).expect("serialize request");

    let parsed = parse_adapter_request(&json).expect("parse request");
    assert_eq!(parsed.protocol, APP_ADAPTER_PROTOCOL);
    assert_eq!(parsed.app, "demo");
    assert_eq!(parsed.action, "run");

    let response_json = response_to_json(&agenthero_agent_runtime::AppAdapterResponse::failed(
        &parsed, "boom",
    ))
    .expect("response json");
    assert!(response_json.contains(r#""ok":false"#));
    assert!(response_json.contains("boom"));
}

#[test]
fn reads_and_writes_adapter_protocol_payloads() {
    let request = AppAdapterRequest::new(
        "demo",
        "run",
        "demo-dag",
        Vec::new(),
        DagIo::default(),
        false,
        false,
    );
    let request_json = serde_json::to_vec(&request).expect("request json");
    let parsed = read_adapter_request(std::io::Cursor::new(request_json)).expect("read request");
    assert_eq!(parsed.app, "demo");

    let response = agenthero_agent_runtime::AppAdapterResponse::failed(&parsed, "nope");
    let mut out = Vec::new();
    write_adapter_response(&mut out, &response).expect("write response");
    let written: serde_json::Value = serde_json::from_slice(&out).expect("valid response json");
    assert_eq!(written["ok"], false);
    assert_eq!(written["error"], "nope");
}

#[test]
fn resolves_app_root_and_loads_manifest_by_dag_type() {
    let temp = tempfile::tempdir().expect("tempdir");
    let app_root = temp.path().join("agenthero").join("apps").join("demo");
    std::fs::create_dir_all(app_root.join("rust")).expect("create rust dir");
    std::fs::create_dir_all(app_root.join("dags")).expect("create dags dir");
    std::fs::write(
        app_root.join("dags").join("demo-dag.yaml"),
        r#"
id: demo-dag
version: 1
accepts: []
nodes: []
edges: []
"#,
    )
    .expect("write dag manifest");

    let resolved = app_root_from_manifest_dir(&app_root.join("rust"));
    assert_eq!(resolved, app_root);
    assert_eq!(
        dag_manifest_path(&resolved, "demo-dag"),
        resolved.join("dags").join("demo-dag.yaml")
    );
    let manifest = load_dag_manifest(&resolved, "demo-dag").expect("load manifest");
    assert_eq!(manifest.id.as_str(), "demo-dag");
}

#[test]
fn resolves_app_root_from_env_or_manifest_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let explicit_root = temp.path().join("explicit");
    std::fs::create_dir_all(&explicit_root).expect("create explicit root");

    std::env::set_var("AGENTHERO_APP_ROOT", &explicit_root);
    let resolved = resolve_app_root("demo", temp.path().join("agenthero/apps/demo/rust"));
    std::env::remove_var("AGENTHERO_APP_ROOT");

    assert_eq!(resolved, explicit_root);
}

#[test]
fn resolves_runtime_binary_from_env_or_app_bin_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let direct = temp.path().join("direct-bin");
    std::fs::write(&direct, "").expect("write direct bin");
    std::env::set_var("DEMO_APP_BIN", &direct);
    assert_eq!(resolve_runtime_binary("DEMO_APP_BIN", "demo-bin"), direct);
    std::env::remove_var("DEMO_APP_BIN");

    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create bin dir");
    let from_dir = bin_dir.join("demo-bin");
    std::fs::write(&from_dir, "").expect("write app bin");
    std::env::set_var("AGENTHERO_APP_BIN_DIR", &bin_dir);
    assert_eq!(resolve_runtime_binary("DEMO_APP_BIN", "demo-bin"), from_dir);
    std::env::remove_var("AGENTHERO_APP_BIN_DIR");
}
