//! Supervisor — the operator-locked parent process.
//!
//! [`Supervisor::execute`] is the only entry point. It:
//!
//! 1. Creates `runs/<run_uuid>/{input,output,logs}/`.
//! 2. Copies `grokrxiv-data/papers/<paper>/review_input.json` (or any path
//!    the caller supplies via `with_review_input`) into `input/`.
//! 3. Renders the agent prompt to `input/prompt.md`.
//! 4. Best-effort `chmod -R a-w` on `input/` so the agent cannot mutate it.
//! 5. Builds the `AgentJob` and dispatches the runner.
//! 6. Walks `output/`, rejects any file outside `allowed_outputs`.
//! 7. Schema-validates `verdict.json`.
//! 8. Writes `manifest.json` capturing the job + result.
//!
//! Construction is intentionally cheap (no DB, no network) so callers can
//! drive the supervisor from unit tests, from `agenthero grokrxiv review`,
//! or from a future queue worker.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use uuid::Uuid;

use super::{prompt, AgentJob, AgentRunResult, AgentRunner, RunStatus, RunnerConfig, Stage};

/// Default filenames the agent contract permits in `output/`.
pub const ALLOWED_REVIEW_OUTPUTS: [&str; 3] = ["review.md", "verdict.json", "audit.json"];

/// Top-level supervisor handle. Cheap to clone; holds no resources.
#[derive(Clone, Default)]
pub struct Supervisor {
    /// Path to the directory where `review_input.json` files live, keyed by
    /// arxiv id. Defaults to `./grokrxiv-data/papers`. Tests override.
    pub data_root: Option<PathBuf>,
    /// Path to the verdict JSON schema. Defaults to `schemas/verdict.schema.json`
    /// relative to CWD.
    pub verdict_schema_path: Option<PathBuf>,
}

impl Supervisor {
    /// Construct a supervisor with default search paths.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the data root (`grokrxiv-data/papers/`). Used by tests so
    /// they can scaffold a fixture paper inside a tempdir.
    pub fn with_data_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.data_root = Some(root.into());
        self
    }

    /// Override the path to `schemas/verdict.schema.json`. Used by tests.
    pub fn with_verdict_schema(mut self, path: impl Into<PathBuf>) -> Self {
        self.verdict_schema_path = Some(path.into());
        self
    }

    /// Run one job through the supervisor pipeline. `paper_ref` is either
    /// an arXiv id or a paper UUID (both are accepted; the supervisor
    /// looks up `grokrxiv-data/papers/<paper_ref>/review_input.json`).
    pub async fn execute(
        &self,
        runner: Arc<dyn AgentRunner>,
        paper_id: Uuid,
        paper_ref: &str,
        stage: Stage,
        runs_root: &Path,
        runner_config_partial: RunnerConfigPartial,
    ) -> Result<(AgentRunResult, PathBuf)> {
        let run_uuid = Uuid::new_v4();
        let run_dir = runs_root.join(run_uuid.to_string());
        let input_dir = run_dir.join("input");
        let output_dir = run_dir.join("output");
        let logs_dir = run_dir.join("logs");
        for d in [&run_dir, &input_dir, &output_dir, &logs_dir] {
            std::fs::create_dir_all(d)
                .map_err(|e| anyhow::anyhow!("create_dir_all {}: {e}", d.display()))?;
        }

        // 1. Copy review_input.json into input/.
        let src = self.review_input_source(paper_ref)?;
        let dst = input_dir.join("review_input.json");
        std::fs::copy(&src, &dst)
            .map_err(|e| anyhow::anyhow!("copy {} -> {}: {e}", src.display(), dst.display()))?;

        // 2. Render the prompt and write input/prompt.md.
        let prompt_text = prompt::render_review_prompt_from_path(&dst)?;
        let prompt_path = input_dir.join("prompt.md");
        std::fs::write(&prompt_path, &prompt_text)
            .map_err(|e| anyhow::anyhow!("write prompt.md: {e}"))?;

        // 3. Best-effort chmod -R a-w on input/ so the agent cannot edit
        //    the prepared context. On platforms where this fails we still
        //    proceed — the contract violation (writes to input/) is also
        //    surfaced post-hoc via output-dir validation.
        let _ = lock_input_dir(&input_dir);

        // 4. Build the job.
        let job = AgentJob {
            paper_id,
            stage,
            input_dir: input_dir.clone(),
            output_dir: output_dir.clone(),
            timeout_seconds: runner_config_partial.timeout_seconds,
            runner_config: RunnerConfig {
                model: runner_config_partial.model,
                max_cost_usd: runner_config_partial.max_cost_usd,
                prompt_path: prompt_path.clone(),
            },
            allowed_outputs: ALLOWED_REVIEW_OUTPUTS.iter().map(|s| (*s).into()).collect(),
        };

        // 5. Run.
        let mut result = runner.run(job.clone()).await;

        // 6. Walk output/ — reject any extraneous filenames. Only do this
        //    if the runner reported success; on Timeout / Failed /
        //    InvalidOutput we leave the existing status alone.
        if result.status == RunStatus::Success {
            match enforce_output_allowlist(&output_dir, &job.allowed_outputs) {
                Ok(files) => {
                    // 7. Validate verdict.json against the schema.
                    let verdict_path = output_dir.join("verdict.json");
                    if let Err(e) = self.validate_verdict(&verdict_path) {
                        result.status = RunStatus::InvalidOutput;
                        let stderr = &result.stderr_path;
                        let prior = std::fs::read_to_string(stderr).unwrap_or_default();
                        let _ = std::fs::write(
                            stderr,
                            format!(
                                "{prior}\n--- supervisor: verdict.json schema validation failed ---\n{e}\n",
                            ),
                        );
                        result.output_files.clear();
                    } else {
                        result.output_files = files;
                    }
                }
                Err(e) => {
                    result.status = RunStatus::InvalidOutput;
                    let stderr = &result.stderr_path;
                    let prior = std::fs::read_to_string(stderr).unwrap_or_default();
                    let _ = std::fs::write(
                        stderr,
                        format!("{prior}\n--- supervisor: output allowlist violation ---\n{e}\n",),
                    );
                    result.output_files.clear();
                }
            }
        }

        // 8. Write manifest.json (best effort; the run still "happened").
        let manifest_path = run_dir.join("manifest.json");
        let manifest = Manifest {
            run_uuid,
            job: ManifestJob::from(&job),
            result: ManifestResult::from(&result),
        };
        let _ = std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap_or_else(|_| "{}".to_string()),
        );

        Ok((result, run_dir))
    }

    fn review_input_source(&self, paper_ref: &str) -> Result<PathBuf> {
        let root = self
            .data_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("grokrxiv-data/papers"));
        let path = root.join(paper_ref).join("review_input.json");
        if !path.exists() {
            anyhow::bail!(
                "supervisor: review_input.json not found at {}. \
                 Run `agenthero grokrxiv ingest <arxiv_id>` first, or scaffold grokrxiv-data/.",
                path.display()
            );
        }
        Ok(path)
    }

    fn validate_verdict(&self, verdict_path: &Path) -> Result<()> {
        let schema_path = self
            .verdict_schema_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("schemas/verdict.schema.json"));
        let schema_bytes = std::fs::read_to_string(&schema_path)
            .map_err(|e| anyhow::anyhow!("read schema {}: {e}", schema_path.display()))?;
        let schema: serde_json::Value = serde_json::from_str(&schema_bytes)
            .map_err(|e| anyhow::anyhow!("parse schema {}: {e}", schema_path.display()))?;
        let body = std::fs::read_to_string(verdict_path)
            .map_err(|e| anyhow::anyhow!("read verdict {}: {e}", verdict_path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("parse verdict {}: {e}", verdict_path.display()))?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|e| anyhow::anyhow!("compile verdict schema: {e}"))?;
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            anyhow::bail!("verdict.json schema errors: {}", errors.join("; "));
        }
        Ok(())
    }
}

/// Partial config — the supervisor fills in `prompt_path` after rendering.
#[derive(Debug, Clone)]
pub struct RunnerConfigPartial {
    /// Model identifier passed through to the runner.
    pub model: String,
    /// Cost ceiling surfaced in the manifest.
    pub max_cost_usd: f32,
    /// Wall-clock timeout for the runner.
    pub timeout_seconds: u64,
}

impl Default for RunnerConfigPartial {
    fn default() -> Self {
        Self {
            model: "opus[1m]".to_string(),
            max_cost_usd: 0.50,
            timeout_seconds: 1800,
        }
    }
}

/// Reason the shared materialiser may reject an agent's stdout. Surfaced
/// to the per-runner `run()` so the runner can flip its `RunStatus`
/// accordingly.
#[derive(Debug)]
pub enum MaterialiseError {
    /// Stdout was unparseable / wrong shape — `RunStatus::Failed`.
    Failed(String),
    /// Stdout parsed but a required field was missing / malformed —
    /// `RunStatus::InvalidOutput`.
    Invalid(String),
}

impl std::fmt::Display for MaterialiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(s) => write!(f, "{s}"),
            Self::Invalid(s) => write!(f, "{s}"),
        }
    }
}

/// Split the agent's three-field wrapper JSON into the three output files.
/// Shared between `LocalCommandRunner` and `ClaudeRunner`.
pub fn materialise_agent_output(
    job: &AgentJob,
    inner_json: &str,
) -> std::result::Result<Vec<PathBuf>, MaterialiseError> {
    let cleaned = strip_code_fences(inner_json.trim());
    let parsed: serde_json::Value = serde_json::from_str(cleaned).map_err(|e| {
        MaterialiseError::Failed(format!(
            "agent stdout was not a JSON object: {e}; raw={inner_json:?}"
        ))
    })?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| MaterialiseError::Invalid("agent stdout was not a JSON object".into()))?;

    let review_md = obj
        .get("review_md")
        .and_then(|v| v.as_str())
        .ok_or_else(|| MaterialiseError::Invalid("missing string field `review_md`".into()))?;
    let verdict_json = obj
        .get("verdict_json")
        .ok_or_else(|| MaterialiseError::Invalid("missing field `verdict_json`".into()))?;
    let audit_json = obj
        .get("audit_json")
        .ok_or_else(|| MaterialiseError::Invalid("missing field `audit_json`".into()))?;

    std::fs::write(job.output_dir.join("review.md"), review_md.as_bytes())
        .map_err(|e| MaterialiseError::Failed(format!("write review.md: {e}")))?;
    std::fs::write(
        job.output_dir.join("verdict.json"),
        serde_json::to_string_pretty(verdict_json)
            .map_err(|e| MaterialiseError::Invalid(format!("serialise verdict_json: {e}")))?,
    )
    .map_err(|e| MaterialiseError::Failed(format!("write verdict.json: {e}")))?;
    std::fs::write(
        job.output_dir.join("audit.json"),
        serde_json::to_string_pretty(audit_json)
            .map_err(|e| MaterialiseError::Invalid(format!("serialise audit_json: {e}")))?,
    )
    .map_err(|e| MaterialiseError::Failed(format!("write audit.json: {e}")))?;

    Ok(vec![
        job.output_dir.join("review.md"),
        job.output_dir.join("verdict.json"),
        job.output_dir.join("audit.json"),
    ])
}

fn strip_code_fences(s: &str) -> &str {
    let t = s.trim();
    let stripped = if let Some(rest) = t.strip_prefix("```json") {
        rest
    } else if let Some(rest) = t.strip_prefix("```") {
        rest
    } else {
        return t;
    };
    stripped
        .trim_start_matches('\n')
        .trim_end_matches("```")
        .trim()
}

fn enforce_output_allowlist(output_dir: &Path, allowed: &[String]) -> Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut extras: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(output_dir)
        .map_err(|e| anyhow::anyhow!("read_dir {}: {e}", output_dir.display()))?
    {
        let entry = entry.map_err(|e| anyhow::anyhow!("read_dir entry: {e}"))?;
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => {
                extras.push(format!("{:?}", entry.file_name()));
                continue;
            }
        };
        if !allowed.iter().any(|a| a == &name) {
            extras.push(name);
            continue;
        }
        found.push(entry.path());
    }
    if !extras.is_empty() {
        anyhow::bail!(
            "unexpected files in output/: [{}]; allowed: [{}]",
            extras.join(", "),
            allowed.join(", "),
        );
    }
    // Ensure all required files are present.
    let missing: Vec<&str> = allowed
        .iter()
        .map(String::as_str)
        .filter(|name| {
            !found
                .iter()
                .any(|p| p.file_name().and_then(|f| f.to_str()) == Some(name))
        })
        .collect();
    if !missing.is_empty() {
        anyhow::bail!("missing required output files: [{}]", missing.join(", "));
    }
    // Deterministic order for downstream consumers / tests.
    found.sort();
    Ok(found)
}

#[cfg(unix)]
fn lock_input_dir(input_dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    walk_apply(input_dir, &|p| {
        let mut perm = std::fs::metadata(p)?.permissions();
        let mode = perm.mode();
        // Strip write bits (0o222) from owner/group/other.
        perm.set_mode(mode & !0o222);
        std::fs::set_permissions(p, perm)
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn lock_input_dir(_input_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn walk_apply(p: &Path, f: &dyn Fn(&Path) -> std::io::Result<()>) -> Result<()> {
    f(p).map_err(|e| anyhow::anyhow!("chmod {}: {e}", p.display()))?;
    if p.is_dir() {
        for entry in std::fs::read_dir(p)? {
            let entry = entry?;
            walk_apply(&entry.path(), f)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Manifest types (serde-only — kept private to this module so we can evolve
// the JSON shape without touching the public AgentJob / AgentRunResult).
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct Manifest {
    run_uuid: Uuid,
    job: ManifestJob,
    result: ManifestResult,
}

#[derive(Debug, Serialize)]
struct ManifestJob {
    paper_id: Uuid,
    stage: Stage,
    input_dir: PathBuf,
    output_dir: PathBuf,
    timeout_seconds: u64,
    model: String,
    max_cost_usd: f32,
    prompt_path: PathBuf,
    allowed_outputs: Vec<String>,
}

impl From<&AgentJob> for ManifestJob {
    fn from(j: &AgentJob) -> Self {
        Self {
            paper_id: j.paper_id,
            stage: j.stage,
            input_dir: j.input_dir.clone(),
            output_dir: j.output_dir.clone(),
            timeout_seconds: j.timeout_seconds,
            model: j.runner_config.model.clone(),
            max_cost_usd: j.runner_config.max_cost_usd,
            prompt_path: j.runner_config.prompt_path.clone(),
            allowed_outputs: j.allowed_outputs.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ManifestResult {
    status: RunStatus,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    output_files: Vec<PathBuf>,
    started_at: chrono::DateTime<chrono::Utc>,
    finished_at: chrono::DateTime<chrono::Utc>,
    exit_code: Option<i32>,
}

impl From<&AgentRunResult> for ManifestResult {
    fn from(r: &AgentRunResult) -> Self {
        Self {
            status: r.status,
            stdout_path: r.stdout_path.clone(),
            stderr_path: r.stderr_path.clone(),
            output_files: r.output_files.clone(),
            started_at: r.started_at,
            finished_at: r.finished_at,
            exit_code: r.exit_code,
        }
    }
}
