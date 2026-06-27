//! FP-RPT3d MVP integration tests for the supervisor-as-parent runtime.
//!
//! Five tests exercise the contract end-to-end via `LocalCommandRunner`:
//! happy path, output-allowlist violation, timeout, schema rejection on
//! `verdict.json`, and the read-only-input-dir lock. A sixth `#[ignore]`d
//! live smoke test wires `ClaudeRunner` against the real `claude` binary
//! on PATH; operators opt in via `cargo test -- --ignored`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use grokrxiv_app_runtime::supervisor_runner::{
    supervisor::{RunnerConfigPartial, Supervisor},
    AgentRunner, ClaudeRunner, LocalCommandRunner, RunStatus, Stage,
};

const PAPER_REF: &str = "test-paper-id";

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the GrokRxiv app runtime crate. Pop two to
    // reach the GrokRxiv app root where `schemas/` lives.
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest)
        .parent()
        .and_then(Path::parent)
        .expect("app root")
        .to_path_buf()
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn verdict_schema_path() -> PathBuf {
    workspace_root().join("schemas").join("verdict.schema.json")
}

/// Scaffold a tempdir that mimics the operator's grokrxiv-data layout:
///
///   <tmp>/data/papers/test-paper-id/review_input.json
///   <tmp>/runs/
///
/// Returns `(data_root, runs_root, _tmpdir)`. Keep `_tmpdir` alive for the
/// duration of the test — dropping it removes the scratch space.
fn scaffold_paper() -> (PathBuf, PathBuf, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().join("data").join("papers");
    let runs_root = tmp.path().join("runs");
    let paper_dir = data_root.join(PAPER_REF);
    std::fs::create_dir_all(&paper_dir).unwrap();
    std::fs::create_dir_all(&runs_root).unwrap();
    let review_input = serde_json::json!({
        "arxiv_id": "test-paper-id",
        "title": "A Test Paper for the Supervisor MVP",
        "abstract": "We test the supervisor.",
        "sections": [],
        "bibliography": []
    });
    std::fs::write(
        paper_dir.join("review_input.json"),
        serde_json::to_string_pretty(&review_input).unwrap(),
    )
    .unwrap();
    (data_root, runs_root, tmp)
}

fn build_supervisor(data_root: &Path) -> Supervisor {
    Supervisor::new()
        .with_data_root(data_root)
        .with_verdict_schema(verdict_schema_path())
}

fn partial(model: PathBuf, timeout: u64) -> RunnerConfigPartial {
    RunnerConfigPartial {
        model: model.to_string_lossy().into_owned(),
        max_cost_usd: 0.10,
        timeout_seconds: timeout,
    }
}

#[tokio::test]
async fn local_runner_success_path() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(LocalCommandRunner::new());
    let supervisor = build_supervisor(&data_root);

    let (result, run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            partial(fixture_path("mock_agent_success.sh"), 30),
        )
        .await
        .expect("supervisor.execute");

    assert_eq!(
        result.status,
        RunStatus::Success,
        "stderr={}",
        std::fs::read_to_string(&result.stderr_path).unwrap_or_default()
    );
    let names: Vec<String> = result
        .output_files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();
    assert_eq!(
        names,
        vec!["audit.json", "review.md", "verdict.json"],
        "output filenames must match the allowlist exactly (sorted)"
    );

    // Verdict.json must parse + validate against the schema.
    let verdict_bytes =
        std::fs::read_to_string(run_dir.join("output/verdict.json")).expect("verdict.json");
    let verdict: serde_json::Value = serde_json::from_str(&verdict_bytes).expect("verdict json");
    assert_eq!(verdict["recommendation"], "minor_revision");

    // Manifest must exist and report success.
    let manifest_bytes =
        std::fs::read_to_string(run_dir.join("manifest.json")).expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_str(&manifest_bytes).expect("manifest json");
    assert_eq!(manifest["result"]["status"], "success");
    assert_eq!(manifest["job"]["allowed_outputs"][0], "review.md");

    // Print so we can paste it into the team report.
    println!("MANIFEST CONTENTS:\n{manifest_bytes}");
}

#[tokio::test]
async fn local_runner_invalid_output_path() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(LocalCommandRunner::new());
    let supervisor = build_supervisor(&data_root);

    let (result, run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            partial(fixture_path("mock_agent_bogus_file.sh"), 30),
        )
        .await
        .expect("supervisor.execute");

    assert_eq!(
        result.status,
        RunStatus::InvalidOutput,
        "bogus_file.txt must trip the allowlist check"
    );
    assert!(result.output_files.is_empty());
    // Stderr should mention the allowlist violation.
    let stderr = std::fs::read_to_string(run_dir.join("logs/stderr.log")).expect("stderr.log");
    assert!(
        stderr.contains("output allowlist violation") || stderr.contains("bogus_file.txt"),
        "stderr should explain the rejection; got: {stderr}"
    );
    // The supervisor must not have copied anything into a "reviews" path —
    // the test asserts simply that output/ retains the bogus file (so we
    // know it ran) plus the three expected files (which the runner ALSO
    // wrote, since the bogus script emits a valid wrapper). We just need
    // to verify the supervisor refused to accept the result, which we
    // already did via `RunStatus::InvalidOutput`.
    assert!(run_dir.join("output/bogus_file.txt").exists());
}

#[tokio::test]
async fn local_runner_timeout_path() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(LocalCommandRunner::new());
    let supervisor = build_supervisor(&data_root);

    let started = std::time::Instant::now();
    let (result, _run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            partial(fixture_path("mock_agent_slow.sh"), 2),
        )
        .await
        .expect("supervisor.execute");
    let elapsed = started.elapsed();

    assert_eq!(result.status, RunStatus::Timeout);
    assert!(result.exit_code.is_none(), "no exit code on SIGTERM");
    // The slow script sleeps 10s; we set timeout=2s. Allow generous
    // headroom for slow CI but cap so we know the kill happened.
    assert!(
        elapsed.as_secs() < 8,
        "supervisor took {}s — timeout did not kill the child",
        elapsed.as_secs()
    );
}

#[tokio::test]
async fn local_runner_invalid_verdict_schema_path() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(LocalCommandRunner::new());
    let supervisor = build_supervisor(&data_root);

    let (result, run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            partial(fixture_path("mock_agent_invalid_verdict.sh"), 30),
        )
        .await
        .expect("supervisor.execute");

    assert_eq!(
        result.status,
        RunStatus::InvalidOutput,
        "missing `recommendation` must fail schema validation"
    );
    let stderr = std::fs::read_to_string(run_dir.join("logs/stderr.log")).expect("stderr.log");
    assert!(
        stderr.contains("verdict.json schema") || stderr.contains("recommendation"),
        "stderr should cite the schema rejection; got: {stderr}"
    );
}

#[tokio::test]
async fn local_runner_input_dir_is_readonly() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(LocalCommandRunner::new());
    let supervisor = build_supervisor(&data_root);

    let (_result, run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            partial(fixture_path("mock_agent_writes_to_input.sh"), 30),
        )
        .await
        .expect("supervisor.execute");

    let stderr = std::fs::read_to_string(run_dir.join("logs/stderr.log")).expect("stderr.log");
    assert!(
        stderr.contains("INPUT_WRITE_OUTCOME=BLOCKED"),
        "agent attempt to write into input/ must be blocked by chmod a-w; stderr={stderr}"
    );
    // input/ must contain only the two files we put there (review_input.json
    // and prompt.md). No tamper.txt.
    assert!(
        !run_dir.join("input/tamper.txt").exists(),
        "tamper.txt leaked into input/ — chmod a-w did not stick"
    );
}

/// Live smoke against the real `claude` CLI. Skipped by default — run
/// with `cargo test -p agenthero-orchestrator --test supervisor_runner_mvp -- --ignored`
/// when Max-auth `claude` is on PATH.
#[tokio::test]
#[ignore]
async fn claude_runner_live_smoke() {
    let (data_root, runs_root, _tmp) = scaffold_paper();
    let runner: Arc<dyn AgentRunner> = Arc::new(ClaudeRunner::new());
    let supervisor = build_supervisor(&data_root);

    let (result, run_dir) = supervisor
        .execute(
            runner,
            uuid::Uuid::nil(),
            PAPER_REF,
            Stage::Review,
            &runs_root,
            RunnerConfigPartial {
                model: "opus[1m]".to_string(),
                max_cost_usd: 0.50,
                timeout_seconds: 600,
            },
        )
        .await
        .expect("supervisor.execute");

    assert_eq!(
        result.status,
        RunStatus::Success,
        "claude live smoke: status={:?}, stderr={}",
        result.status,
        std::fs::read_to_string(&result.stderr_path).unwrap_or_default()
    );
    assert!(run_dir.join("output/review.md").exists());
    assert!(run_dir.join("output/verdict.json").exists());
    assert!(run_dir.join("output/audit.json").exists());
}
