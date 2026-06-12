use agenthero_dag_runtime::{DagManifest, DagNodeStatus};
use serde_json::json;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn app_registry_groups_dag_types_behind_product_apps() {
    let _guard = EnvGuard::clear_apps_root();
    let ids = agenthero_orchestrator::dag_apps::registered_app_ids().expect("registered app ids");
    assert_eq!(ids, vec!["c2rust".to_string(), "grokrxiv".to_string()]);

    let grokrxiv = agenthero_orchestrator::dag_apps::registered_app("grokrxiv")
        .expect("GrokRxiv app descriptor loads")
        .expect("GrokRxiv app descriptor");
    assert_eq!(grokrxiv.deployments.len(), 1);
    let deployment = &grokrxiv.deployments[0];
    let (project, root, env) = match deployment {
        agenthero_orchestrator::dag_apps::AppDeployment::Vercel {
            project, root, env, ..
        } => (project, root, env),
    };
    assert_eq!(project, "grokrxiv");
    assert_eq!(root, "web");
    for required in [
        "NEXT_PUBLIC_SITE_URL",
        "NEXT_PUBLIC_SUPABASE_URL",
        "NEXT_PUBLIC_SUPABASE_ANON_KEY",
        "SUPABASE_SERVICE_ROLE_KEY",
        "ORCHESTRATOR_INTERNAL_URL",
        "AGENTHERO_SERVICE_TOKEN",
        "REVALIDATE_SECRET",
        "GROKRXIV_PUBLIC_URL",
        "GROKRXIV_BILLING_ENABLED",
        "STRIPE_SECRET_KEY",
        "STRIPE_WEBHOOK_SECRET",
        "STRIPE_SUPPORTER_PRICE_ID",
        "STRIPE_RESEARCHER_PRICE_ID",
        "GROKRXIV_SUPER_ADMIN_EMAIL",
        "GROKRXIV_FREE_REVIEW_LIMIT",
    ] {
        assert!(
            env.iter().any(|name| name == required),
            "GrokRxiv Vercel deployment must declare `{required}`"
        );
    }

    let grokrxiv_actions = grokrxiv
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    for action in [
        "extract",
        "review",
        "review-extracted",
        "show",
        "list",
        "open",
        "approve",
        "request-revisions",
        "request-changes",
        "reject",
    ] {
        assert!(
            grokrxiv_actions.contains(&action),
            "GrokRxiv app must expose `{action}`"
        );
    }

    let c2rust = agenthero_orchestrator::dag_apps::registered_app("c2rust")
        .expect("c2rust app descriptor loads")
        .expect("c2rust app descriptor");
    assert_eq!(
        c2rust
            .actions
            .iter()
            .map(|action| action.dag_type.as_str())
            .collect::<Vec<_>>(),
        vec!["c2rust"]
    );
}

#[test]
fn registry_contains_grokrxiv_chain_and_c2rust_apps() {
    let _guard = EnvGuard::clear_apps_root();
    let ids =
        agenthero_orchestrator::dag_apps::registered_dag_app_ids().expect("registered DAG app ids");

    assert_eq!(
        ids,
        vec![
            "c2rust".to_string(),
            "citation-validation".to_string(),
            "paper-extract".to_string(),
            "paper-ingest".to_string(),
            "paper-publish".to_string(),
            "paper-review".to_string(),
            "paper-revise".to_string(),
            "review-loop".to_string(),
        ]
    );
}

#[test]
fn broken_app_manifest_bubbles_from_registry_helpers() {
    let root = TempRoot::new("broken-app");
    std::fs::create_dir_all(root.path().join("bad")).unwrap();
    std::fs::write(root.path().join("bad/app.yaml"), "slug: [not-a-string]\n").unwrap();

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::registered_app_ids()
        .expect_err("broken app manifest must bubble as error");
    assert!(
        err.to_string().contains("parse app manifest"),
        "expected parse error, got {err:#}"
    );
}

#[test]
fn app_manifest_requires_version() {
    let root = TempRoot::new("missing-app-version");
    std::fs::create_dir_all(root.path().join("demo")).unwrap();
    std::fs::write(
        root.path().join("demo/app.yaml"),
        r#"slug: demo
label: Demo
adapter:
  kind: process
  command: demo-adapter
actions:
  - id: run
    command: [run]
    dag_type: demo
"#,
    )
    .unwrap();

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::registered_app_ids()
        .expect_err("app manifests must declare version");
    assert!(
        err.to_string().contains("version"),
        "expected version error, got {err:#}"
    );
}

#[test]
fn app_action_dag_type_must_reference_existing_manifest() {
    let root = TempRoot::new("missing-action-dag");
    let app = root.path().join("demo");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.yaml"),
        r#"version: 1
slug: demo
label: Demo
adapter:
  kind: process
  command: demo-adapter
actions:
  - id: run
    command: [run]
    dag_type: missing-dag
"#,
    )
    .unwrap();

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::registered_app_ids()
        .expect_err("action dag_type must resolve during app discovery");
    assert!(
        err.to_string().contains("missing DAG manifest"),
        "expected missing manifest error, got {err:#}"
    );
}

#[test]
fn duplicate_dag_type_across_apps_is_rejected() {
    let root = TempRoot::new("duplicate-dag");
    write_app_manifest(root.path(), "alpha", "shared", "run", "/bin/true", &[]);
    write_app_manifest(root.path(), "beta", "shared", "run", "/bin/true", &[]);

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::registered_dag_apps()
        .expect_err("duplicate dag_type must fail");
    assert!(
        err.to_string().contains("dag_type `shared`"),
        "expected duplicate dag_type error, got {err:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adapter_response_identity_mismatch_is_rejected() {
    let root = TempRoot::new("identity-mismatch");
    let script = write_adapter_script(
        root.path(),
        "mismatch.sh",
        r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agenthero.app.v1","app":"wrong","action":"run","dag_type":"demo","ok":true}'
"#,
    );
    write_app_manifest(
        root.path(),
        "alpha",
        "demo",
        "run",
        script.to_str().unwrap(),
        &[],
    );

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::run_app_action(
        "alpha",
        "run",
        Vec::new(),
        agenthero_dag_executor::DagIo::default(),
        true,
        false,
    )
    .await
    .expect_err("identity mismatch must fail");
    assert!(
        err.to_string().contains("response app `wrong`"),
        "expected identity error, got {err:#}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adapter_request_uses_typed_args_and_dry_run() {
    let root = TempRoot::new("typed-request");
    let capture = root.path().join("request.json");
    let script = write_adapter_script(
        root.path(),
        "capture.sh",
        &format!(
            r#"#!/bin/sh
cat > "{}"
printf '%s' '{{"protocol":"agenthero.app.v1","app":"alpha","action":"run","dag_type":"demo","ok":true}}'
"#,
            capture.display()
        ),
    );
    write_app_manifest(
        root.path(),
        "alpha",
        "demo",
        "run",
        script.to_str().unwrap(),
        &[],
    );

    let _guard = EnvGuard::set_apps_root(root.path());
    let mut input = agenthero_dag_executor::DagIo::default();
    input.values.insert("seed".to_string(), json!(true));
    agenthero_orchestrator::dag_apps::run_app_action(
        "alpha",
        "run",
        vec!["--flag".to_string(), "value".to_string()],
        input,
        true,
        true,
    )
    .await
    .expect("captured request succeeds");

    let request: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(capture).unwrap()).unwrap();
    assert_eq!(request["args"], json!(["--flag", "value"]));
    assert_eq!(request["dry_run"], json!(true));
    assert!(
        request["idempotency_key"]
            .as_str()
            .is_some_and(|key| !key.is_empty()),
        "adapter request must carry idempotency_key"
    );
    assert_eq!(request["input"]["values"]["seed"], json!(true));
    assert!(
        request["input"]["values"].get("args").is_none(),
        "args must not be duplicated into DagIo values"
    );
    assert!(
        request["input"]["values"].get("dry_run").is_none(),
        "dry_run must not be duplicated into DagIo values"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn adapter_malformed_json_nonzero_exit_and_timeout_are_rejected() {
    let root = TempRoot::new("adapter-failures");
    let malformed = write_adapter_script(
        root.path(),
        "malformed.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf 'not json'\n",
    );
    let nonzero = write_adapter_script(
        root.path(),
        "nonzero.sh",
        "#!/bin/sh\ncat >/dev/null\necho boom >&2\nexit 42\n",
    );
    let timeout = write_adapter_script(root.path(), "timeout.sh", "#!/bin/sh\nsleep 2\n");
    let oversized = write_adapter_script(
        root.path(),
        "oversized.sh",
        "#!/bin/sh\ncat >/dev/null\nprintf '0123456789abcdef0123456789abcdef'\n",
    );
    write_app_manifest(
        root.path(),
        "malformed",
        "malformed-dag",
        "run",
        malformed.to_str().unwrap(),
        &[],
    );
    write_app_manifest(
        root.path(),
        "nonzero",
        "nonzero-dag",
        "run",
        nonzero.to_str().unwrap(),
        &[],
    );
    write_app_manifest(
        root.path(),
        "timeout",
        "timeout-dag",
        "run",
        timeout.to_str().unwrap(),
        &["timeout_secs: 1"],
    );
    write_app_manifest(
        root.path(),
        "oversized",
        "oversized-dag",
        "run",
        oversized.to_str().unwrap(),
        &["output_limit_bytes: 16"],
    );

    let _guard = EnvGuard::set_apps_root(root.path());
    for (app, expected) in [
        (
            "malformed",
            "parse app `malformed` adapter response as JSON",
        ),
        ("nonzero", "adapter exited"),
        ("timeout", "timed out"),
        ("oversized", "stdout exceeded 16 bytes"),
    ] {
        let err = agenthero_orchestrator::dag_apps::run_app_action(
            app,
            "run",
            Vec::new(),
            agenthero_dag_executor::DagIo::default(),
            true,
            false,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "{app}: expected `{expected}`, got {err:#}"
        );
    }
}

#[test]
fn orchestrator_does_not_depend_on_dag_app_crates() {
    let _guard = EnvGuard::clear_apps_root();
    let manifest = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read orchestrator Cargo.toml");

    for forbidden in [
        "agenthero-dag-app-c2rust",
        "agenthero-dag-app-grokrxiv",
        "grokrxiv-app-runtime",
        "grokrxiv-extraction",
        "grokrxiv-ingest",
        "grokrxiv-publisher",
        "grokrxiv-render",
        "grokrxiv-review-loop",
        "grokrxiv-schemas",
        "grokrxiv-storage",
        "grokrxiv-verifier",
        "grokrxiv-dag-app-citation-validation",
        "grokrxiv-dag-app-paper-extract",
        "grokrxiv-dag-app-paper-ingest",
        "grokrxiv-dag-app-paper-publish",
        "grokrxiv-dag-app-paper-review",
        "grokrxiv-dag-app-paper-revise",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "orchestrator must not depend on app crate `{forbidden}`; app manifests declare adapters"
        );
    }
}

#[test]
fn root_workspace_only_contains_platform_crates() {
    let _guard = EnvGuard::clear_apps_root();
    let root = workspace_root();
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let parsed: toml::Value = toml::from_str(&manifest).expect("parse root Cargo.toml");
    let members = parsed["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|value| value.as_str().expect("member string"))
        .collect::<Vec<_>>();

    assert_eq!(
        members,
        vec![
            "crates/app-sdk",
            "crates/dag-runtime",
            "crates/dag-executor",
            "crates/agent-runtime",
            "crates/orchestrator",
            "crates/llm-adapter",
        ]
    );
    assert!(
        members
            .iter()
            .all(|member| !member.starts_with("agenthero/apps/")),
        "apps must build from their own manifests, not root workspace membership"
    );
}

#[test]
fn top_level_crates_directory_is_platform_only() {
    let _guard = EnvGuard::clear_apps_root();
    let root = workspace_root();
    let mut crates = std::fs::read_dir(root.join("crates"))
        .expect("read crates/")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.is_dir() {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    crates.sort();
    assert_eq!(
        crates,
        vec![
            "agent-runtime",
            "app-sdk",
            "dag-executor",
            "dag-runtime",
            "llm-adapter",
            "orchestrator",
        ]
    );
}

#[test]
fn orchestrator_source_has_no_grokrxiv_domain_code() {
    let _guard = EnvGuard::clear_apps_root();
    let root = workspace_root();
    let src = root.join("crates").join("orchestrator").join("src");
    let forbidden = [
        "grokrxiv",
        "GrokRxiv",
        "arxiv",
        "paper-review",
        "paper_extract",
        "paper-extract",
    ];
    for path in walk_rs_files(&src) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for needle in forbidden {
            assert!(
                !text.contains(needle),
                "{} must not contain app-specific token `{needle}`",
                path.display()
            );
        }
    }
}

#[test]
fn app_manifest_resolves_action_command_paths() {
    let _guard = EnvGuard::clear_apps_root();
    let review = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["review".into(), "2605.17307".into()],
    )
    .expect("review action resolves");
    assert_eq!(review.id, "review");
    assert_eq!(review.dag_type, "review-loop");
    assert_eq!(review.args, vec!["2605.17307"]);

    let citations = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["validate".into(), "citations".into()],
    )
    .expect("nested validate citations action resolves");
    assert_eq!(citations.id, "validate-citations");
    assert_eq!(citations.dag_type, "citation-validation");
    assert!(citations.args.is_empty());

    let err = agenthero_orchestrator::dag_apps::resolve_app_action_args(
        "grokrxiv",
        &["validate".into(), "metadata".into()],
    )
    .expect_err("unknown nested app action must fail");
    assert!(err.to_string().contains("unknown app action"));
}

#[test]
fn app_action_descriptors_expose_retry_policy() {
    let _guard = EnvGuard::clear_apps_root();
    let review = agenthero_orchestrator::dag_apps::registered_app_action("grokrxiv", "review")
        .expect("registered action loads")
        .expect("review action");

    assert_eq!(review.retry.max_attempts, 2);
}

#[test]
fn app_descriptors_surface_agentapp_contract_files() {
    let _guard = EnvGuard::clear_apps_root();

    let c2rust = agenthero_orchestrator::dag_apps::registered_app("c2rust")
        .expect("c2rust app descriptor loads")
        .expect("c2rust app descriptor");
    assert!(
        c2rust
            .contracts
            .state_schemas
            .contains(&"state/run_state.schema.json".to_string()),
        "c2rust must expose its StateSchema contract"
    );
    assert_eq!(c2rust.contracts.tools.as_deref(), Some("tools.yaml"));
    assert!(
        c2rust
            .contracts
            .evals
            .contains(&"evals/smoke.yaml".to_string()),
        "c2rust must expose its eval contract"
    );

    let grokrxiv = agenthero_orchestrator::dag_apps::registered_app("grokrxiv")
        .expect("GrokRxiv app descriptor loads")
        .expect("GrokRxiv app descriptor");
    assert!(
        grokrxiv
            .contracts
            .policies
            .contains(&"policies/release_tiers.yaml".to_string()),
        "GrokRxiv release tiers must remain app-owned and discoverable"
    );
    assert_eq!(grokrxiv.contracts.tools.as_deref(), Some("tools.yaml"));
}

#[test]
fn app_contract_validation_rejects_unknown_tool_permissions() {
    let root = TempRoot::new("bad-tools-contract");
    write_app_manifest(root.path(), "demo", "demo", "run", "/bin/true", &[]);
    std::fs::write(
        root.path().join("demo/tools.yaml"),
        r#"version: 1
tools:
  - id: demo_tool
    permissions: [read, sudo]
"#,
    )
    .unwrap();

    let _guard = EnvGuard::set_apps_root(root.path());
    let err = agenthero_orchestrator::dag_apps::registered_app_ids()
        .expect_err("unknown tool permissions must fail app discovery");
    assert!(
        err.to_string().contains("unknown tool permission `sudo`"),
        "expected tools.yaml permission error, got {err:#}"
    );
}

#[test]
fn every_registered_app_has_a_valid_manifest() {
    let _guard = EnvGuard::clear_apps_root();
    for app in
        agenthero_orchestrator::dag_apps::registered_dag_apps().expect("registered DAG apps load")
    {
        let path = app.manifest_path;
        let manifest = DagManifest::from_path(&path)
            .unwrap_or_else(|err| panic!("{} should be valid: {err}", path.display()));
        assert_eq!(manifest.id.as_str(), app.dag_type);
    }
}

#[test]
fn app_contracts_are_owned_by_app_roots() {
    let _guard = EnvGuard::clear_apps_root();
    let root = workspace_root();

    for app in ["grokrxiv", "c2rust"] {
        let app_root = root.join("agenthero").join("apps").join(app);
        assert!(
            app_root.join("app.yaml").is_file(),
            "{} must declare its product app manifest inside the app root",
            app_root.display()
        );
        assert!(
            app_root.join("dags").is_dir(),
            "{} must own its DAG manifests",
            app_root.display()
        );
    }

    for legacy_root in [
        "dags",
        "agents",
        "prompts",
        "apps",
        "scripts",
        "grokrxiv-skills",
        "research",
        "migrations",
    ] {
        assert!(
            !root.join(legacy_root).exists(),
            "legacy root-level `{legacy_root}/` must not remain an app contract source"
        );
    }

    assert!(
        root.join("agenthero").join("migrations").is_dir(),
        "generic platform migrations stay under agenthero/migrations"
    );
    assert!(
        root.join("agenthero")
            .join("apps")
            .join("grokrxiv")
            .join("migrations")
            .is_dir(),
        "GrokRxiv migrations live with the app"
    );
}

#[test]
fn grokrxiv_env_templates_use_agenthero_operator_contract() {
    let _guard = EnvGuard::clear_apps_root();
    let root = workspace_root();
    let keys = active_env_template_keys(&root);

    for required in [
        "AGENTHERO_ENV_FILES",
        "AGENTHERO_ARTIFACTS_DIR",
        "AGENTHERO_RUNNER",
        "AGENTHERO_EXTRACTOR",
        "AGENTHERO_ALLOW_PROVIDER_API",
        "AGENTHERO_SERVICE_TOKEN",
        "AGENTHERO_SCHEDULER_WORKERS",
        "AGENTHERO_CLAUDE_BIN",
        "AGENTHERO_CODEX_BIN",
        "AGENTHERO_GEMINI_BIN",
        "AGENTHERO_CLI_TIMEOUT_SECS",
        "AGENTHERO_MAX_COST_USD",
        "AGENTHERO_MODE",
        "AGENTHERO_OFFLINE",
        "AGENTHERO_SANDBOX",
        "AGENTHERO_EXTRACTION_TOOL_FALLBACK",
        "AGENTHERO_APPS_ROOT",
        "AGENTHERO_ADAPTER_BIN_DIR",
        "AGENTHERO_APP_BIN_DIR",
        "AGENTHERO_ADAPTER_CWD",
        "AGENTHERO_ADAPTER_TIMEOUT_SECS",
        "AGENTHERO_ADAPTER_OUTPUT_LIMIT_BYTES",
        "AGENTHERO_ALLOW_ADAPTER_FALLBACK",
    ] {
        assert!(
            keys.contains(required),
            "template must declare `{required}`"
        );
    }

    for forbidden in [
        "GROKRXIV_RUNNER",
        "GROKRXIV_EXTRACTOR",
        "GROKRXIV_ALLOW_PROVIDER_API",
        "GROKRXIV_SERVICE_TOKEN",
        "GROKRXIV_CLOUD_PROVIDER",
        "GROKRXIV_LITELLM_URL",
        "AGENTHERO_CLOUD_PROVIDER",
        "AGENTHERO_LITELLM_URL",
        "AGENTHERO_LOCAL_TIMEOUT_SECS",
        "OLLAMA_HOST",
        "VERCEL_OPEN_AGENTS_URL",
        "VERCEL_OPEN_AGENTS_TOKEN",
        "E2B_API_KEY",
        "GROKRXIV_CLAUDE_BIN",
        "GROKRXIV_CODEX_BIN",
        "GROKRXIV_RUNNER_OVERRIDE",
        "GROKRXIV_RUNNER_OVERRIDE_CITATION",
        "GROKRXIV_MODEL_OVERRIDE_CITATION",
        "GROKRXIV_CLI_TIMEOUT_SECS",
        "GROKRXIV_LOCAL_TIMEOUT_SECS",
        "GROKRXIV_MAX_COST_USD",
        "GROKRXIV_MODERATOR",
        "GROKRXIV_MODE",
        "GROKRXIV_OFFLINE",
        "GROKRXIV_SANDBOX",
        "GROKRXIV_EXTRACTION_TOOL_FALLBACK",
        "GROKRXIV_AGENTS_DIR",
        "GROKRXIV_SCHEMAS_DIR",
        "GROKRXIV_PROMPTS_DIR",
        "SUPABASE_ANON_KEY",
    ] {
        assert!(
            !keys.contains(forbidden),
            "template must not declare stale env key `{forbidden}`"
        );
    }
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn active_env_template_keys(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    let mut paths = vec![root.join(".env.example")];
    let env_dir = root.join("agenthero/apps/grokrxiv/env");
    for entry in std::fs::read_dir(&env_dir).expect("read GrokRxiv env templates") {
        let path = entry.expect("env template entry").path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(".env_") && name.ends_with(".example") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut keys = std::collections::BTreeSet::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, _)) = trimmed.split_once('=') {
                keys.insert(key.trim().to_string());
            }
        }
    }
    keys
}

fn walk_rs_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path)
            .unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
        {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

struct TempRoot {
    path: std::path::PathBuf,
}

impl TempRoot {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "agenthero-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct EnvGuard {
    previous_apps_root: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn clear_apps_root() -> Self {
        let guard = ENV_LOCK.lock().unwrap();
        let previous_apps_root = std::env::var_os("AGENTHERO_APPS_ROOT");
        std::env::remove_var("AGENTHERO_APPS_ROOT");
        Self {
            previous_apps_root,
            _guard: guard,
        }
    }

    fn set_apps_root(path: &std::path::Path) -> Self {
        let guard = ENV_LOCK.lock().unwrap();
        let previous_apps_root = std::env::var_os("AGENTHERO_APPS_ROOT");
        std::env::set_var("AGENTHERO_APPS_ROOT", path);
        Self {
            previous_apps_root,
            _guard: guard,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous_apps_root {
            Some(value) => std::env::set_var("AGENTHERO_APPS_ROOT", value),
            None => std::env::remove_var("AGENTHERO_APPS_ROOT"),
        }
    }
}

fn write_app_manifest(
    root: &std::path::Path,
    slug: &str,
    dag_type: &str,
    action: &str,
    command: &str,
    adapter_extra: &[&str],
) {
    let app = root.join(slug);
    std::fs::create_dir_all(&app).unwrap();
    let extra = if adapter_extra.is_empty() {
        String::new()
    } else {
        format!("  {}\n", adapter_extra.join("\n  "))
    };
    std::fs::write(
        app.join("app.yaml"),
        format!(
            r#"version: 1
slug: {slug}
label: {slug}
adapter:
  kind: process
  command: "{command}"
{extra}actions:
  - id: {action}
    command: [{action}]
    dag_type: {dag_type}
"#
        ),
    )
    .unwrap();
    write_dag_manifest(root, slug, dag_type);
}

fn write_dag_manifest(root: &std::path::Path, slug: &str, dag_type: &str) {
    let dags = root.join(slug).join("dags");
    std::fs::create_dir_all(&dags).unwrap();
    std::fs::write(
        dags.join(format!("{dag_type}.yaml")),
        format!(
            r#"id: {dag_type}
version: 1
"#
        ),
    )
    .unwrap();
}

fn write_adapter_script(root: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = root.join(name);
    std::fs::write(&script, body).unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    script
}

#[tokio::test(flavor = "current_thread")]
async fn registry_runs_c2rust_manifest_through_declared_adapter() {
    let _guard = EnvGuard::clear_apps_root();
    let report = agenthero_orchestrator::dag_apps::run_registered_dag_app(
        "c2rust",
        agenthero_dag_executor::DagIo::default(),
    )
    .await
    .expect("c2rust run");

    assert_eq!(report.status, DagNodeStatus::Ok);
    assert_eq!(report.nodes.len(), 4);
    assert!(report.outputs.values.contains_key("lint_pass"));
}
