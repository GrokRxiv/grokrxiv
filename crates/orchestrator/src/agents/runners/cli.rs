//! `CliRunner` — local CLI subprocess for tool-using agents.
//!
//! Spawns `claude` / `codex` / `gemini` based on `spec.provider`. No runtime
//! `--cli-agent` flag — the YAML's existing `provider:` field is the source
//! of truth.
//!
//! RPT2 Track B: host-only execution. `SandboxPolicy::Container` is explicitly
//! rejected so callers don't silently get "ran on host when you asked for
//! container".

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::agents::extraction::ToolCtx;
use crate::agents::traits::AgentRunner;
use crate::agents::types::{
    AgentInput, AgentRun, AgentRunnerKind, AgentSpec, Message, SandboxPolicy, ToolCompletion,
    ToolSpec,
};

/// FP-RPT3b B5: structured errors returned by `CliRunner::run`. Wrapped into
/// the `anyhow::Error` chain so callers can detect them via
/// `err.downcast_ref::<CliError>()` and decide whether to fall back to a
/// different runner instead of bubbling up as a generic subprocess failure.
#[derive(Debug)]
pub enum CliError {
    /// The CLI subprocess emitted a known quota / rate-limit / billing signal
    /// on stderr. `provider` is the provider tag (`claude`, `openai`,
    /// `gemini`); `message` is the first 200 chars of stderr for forensics.
    QuotaExhausted {
        /// Provider tag from `AgentSpec.provider`.
        provider: String,
        /// First slice of the subprocess stderr that triggered the
        /// classification. Truncated to 200 chars.
        message: String,
    },
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::QuotaExhausted { provider, message } => write!(
                f,
                "{provider} CLI quota exhausted. Set --runner api or wait for reset. \
                 stderr={message}"
            ),
        }
    }
}

impl std::error::Error for CliError {}

/// Match a subprocess stderr buffer against the known quota / rate-limit /
/// billing signatures (case-insensitive substring). Returns `Some(snippet)`
/// when a signature matches, where `snippet` is the first 200 chars of stderr
/// suitable for inclusion in the structured error message.
fn detect_quota_signal(stderr: &str) -> Option<String> {
    let lower = stderr.to_lowercase();
    const SIGNATURES: &[&str] = &[
        "rate limit",
        "rate-limit",
        "rate_limit",
        "quota exceeded",
        "quota exhausted",
        "insufficient_quota",
        "insufficient quota",
        "billing",
        "payment required",
        "resource_exhausted",
        "resource exhausted",
        "429",
    ];
    if SIGNATURES.iter().any(|sig| lower.contains(sig)) {
        let snippet: String = stderr.chars().take(200).collect();
        return Some(snippet);
    }
    None
}

/// Default subprocess timeout (seconds) when `GROKRXIV_CLI_TIMEOUT_SECS` is
/// unset.
const DEFAULT_CLI_TIMEOUT_SECS: u64 = 360;

/// Name of the Claude skill that enforces JSON-only output.
const CLAUDE_SKILL_NAME: &str = "grokrxiv-review";

/// Body of the Claude skill (`SKILL.md`) installed on first invocation.
const CLAUDE_SKILL_BODY: &str = "---
name: grokrxiv-review
description: Specialist reviewer for grokrxiv. Emits JSON-only output strictly matching the role's schema.
---

You are a specialist reviewer for grokrxiv. The user supplies:
- a role tag (one of: summary, technical_correctness, novelty, reproducibility, citation, meta_reviewer)
- a paper extract (or for meta_reviewer, the 5 specialist outputs)
- the JSON schema for that role's output

You MUST output a SINGLE JSON object that validates against the schema. NO prose, NO markdown fences, NO commentary. If you cannot, output `{\"error\":\"<one-line reason>\"}`.
";

/// FP-RPT3b B4: one-shot guard per provider for the auth-path log line.
/// `CliRunner` is constructed once and shared, so we only need a single
/// `OnceLock` per provider tag to make sure the auth surface is logged
/// exactly once per orchestrator process.
static CLAUDE_AUTH_LOGGED: OnceLock<()> = OnceLock::new();
static CODEX_AUTH_LOGGED: OnceLock<()> = OnceLock::new();
static GEMINI_AUTH_LOGGED: OnceLock<()> = OnceLock::new();

/// Spawns local CLI binaries (`claude` / `codex` / `gemini`). The binary path
/// for each is overridable via `GROKRXIV_CLAUDE_BIN` / `GROKRXIV_CODEX_BIN` /
/// `GROKRXIV_GEMINI_BIN`. Default timeout 180s via `GROKRXIV_CLI_TIMEOUT_SECS`.
#[derive(Default)]
pub struct CliRunner;

impl CliRunner {
    /// Construct with defaults.
    pub fn new() -> Self {
        Self
    }

    /// Map a `spec.provider` string to the binary that should be executed.
    /// Reads the per-provider override env var. Returns `Err` for any
    /// unsupported provider.
    pub fn binary_for(&self, provider: &str) -> anyhow::Result<String> {
        match provider {
            "claude" => Ok(std::env::var("GROKRXIV_CLAUDE_BIN")
                .unwrap_or_else(|_| "claude".to_string())),
            "openai" => Ok(std::env::var("GROKRXIV_CODEX_BIN")
                .unwrap_or_else(|_| "codex".to_string())),
            "gemini" => Ok(std::env::var("GROKRXIV_GEMINI_BIN")
                .unwrap_or_else(|_| "gemini".to_string())),
            other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
        }
    }
}

/// Specification for the constructed subprocess. Captured separately from the
/// spawn so unit tests can assert on the shape without actually invoking the
/// binary.
#[derive(Debug, Clone)]
struct BuiltCommand {
    /// The path / name of the binary that will be exec'd.
    program: String,
    /// argv excluding the program itself.
    args: Vec<String>,
    /// The prompt body that gets piped to the child's stdin.
    stdin_payload: String,
    /// For codex: the schema file path written before invocation (so the test
    /// helper can assert it was placed and the runtime helper can clean up).
    /// `None` for claude / gemini.
    schema_path: Option<PathBuf>,
}

#[async_trait]
impl AgentRunner for CliRunner {
    fn name(&self) -> &'static str {
        "cli"
    }

    async fn run(
        &self,
        spec: &AgentSpec,
        input: &AgentInput,
    ) -> anyhow::Result<AgentRun> {
        // 1. Reject Container sandbox explicitly — RPT2 Track B is host-only.
        if matches!(spec.sandbox, SandboxPolicy::Container) {
            anyhow::bail!(
                "SandboxPolicy::Container is not supported in RPT2 — set --sandbox none or update your YAML"
            );
        }

        let started = Instant::now();
        let timeout_dur = cli_timeout();

        // FP-RPT3b B4: surface the per-provider auth path at INFO level once
        // per process so the operator can audit the $0-marginal-cost claim
        // against the actual auth tier.
        log_auth_path_once(&spec.provider);

        // 2. Pre-flight: ensure the Claude skill is installed on disk before
        //    spawning. Idempotent.
        if spec.provider == "claude" {
            if let Err(e) = ensure_grokrxiv_review_skill_installed() {
                tracing::warn!(err = %e, "failed to install grokrxiv-review claude skill");
            }
        }

        let prompt = format!("{}\n\n{}", input.system_prompt, input.user_prompt);

        // 3. First attempt.
        let built = build_command(self, spec, &prompt)?;
        let raw_stdout = match exec_and_capture(&built, timeout_dur, spec.role, &spec.provider).await {
            Ok(s) => s,
            Err(e) => {
                cleanup_schema_path(&built.schema_path);
                return Err(e);
            }
        };
        cleanup_schema_path(&built.schema_path);

        // 4. Extract and validate JSON. On parse OR schema-validation failure,
        //    one-shot corrective retry.
        let extracted = extract_json_text(&spec.provider, &raw_stdout);
        let parsed = match parse_and_validate(&extracted, &spec.schema) {
            Ok(v) => v,
            Err(first_err) => {
                let corrective = format!(
                    "Your previous output did not parse as JSON. Please output JSON only, no prose. Try again:\n{schema}\n{prompt}",
                    schema = serde_json::to_string(&spec.schema).unwrap_or_default(),
                    prompt = prompt,
                );
                let built2 = build_command(self, spec, &corrective)?;
                let raw2 = match exec_and_capture(&built2, timeout_dur, spec.role, &spec.provider).await {
                    Ok(s) => s,
                    Err(e) => {
                        cleanup_schema_path(&built2.schema_path);
                        return Err(e);
                    }
                };
                cleanup_schema_path(&built2.schema_path);
                let extracted2 = extract_json_text(&spec.provider, &raw2);
                parse_and_validate(&extracted2, &spec.schema).map_err(|second_err| {
                    anyhow::anyhow!(
                        "CliRunner parse/validate failure after corrective retry for role {role:?}: first={first_err}; retry={second_err}",
                        role = spec.role,
                    )
                })?
            }
        };

        let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

        Ok(AgentRun {
            role: spec.role,
            runner: AgentRunnerKind::Cli,
            model: spec.model.clone(),
            output: parsed,
            tokens_in: None,
            tokens_out: None,
            latency_ms,
            cache_hit: false,
            sandbox_ref: None,
            verifier_status: None,
            verifier_notes: None,
        })
    }

    async fn complete_with_tools(
        &self,
        spec: &AgentSpec,
        messages: &[Message],
        tools: &[ToolSpec],
        ctx: &ToolCtx<'_>,
    ) -> anyhow::Result<ToolCompletion> {
        // FP6 / RPT3 Track 8 framework: claude and codex CLIs both have
        // tool-call streaming, but their wire formats diverge and the
        // contracts are still moving. For Wave 1 we ship a single explicit
        // escape valve: when `GROKRXIV_EXTRACTION_TOOL_FALLBACK=api` is set,
        // dispatch through an in-process ApiRunner using the same
        // provider name. This keeps the framework end-to-end testable
        // without coupling Wave 1 to two CLIs that are still drifting.
        let fallback = std::env::var("GROKRXIV_EXTRACTION_TOOL_FALLBACK")
            .ok()
            .filter(|s| s == "api");
        if fallback.is_some() {
            let providers = build_api_fallback_providers(spec)?;
            let api = super::api::ApiRunner::new(providers);
            return api.complete_with_tools(spec, messages, tools, ctx).await;
        }
        anyhow::bail!(
            "CliRunner.complete_with_tools: `{}` CLI does not yet support the GrokRxiv \
             tool-call protocol. Either set `GROKRXIV_EXTRACTION_TOOL_FALLBACK=api` to \
             dispatch via the provider API for this stage, or run the extraction agent \
             through the `api` runner (--runner api).",
            spec.provider
        )
    }
}

/// Build a provider registry for the ApiRunner fallback. Pulls keys from the
/// environment so this works in the same shell that invoked the CLI.
fn build_api_fallback_providers(
    spec: &AgentSpec,
) -> anyhow::Result<std::collections::HashMap<String, std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>>
{
    use grokrxiv_llm_adapter::{provider_by_name, ProviderConfig};
    let cfg = ProviderConfig::from_env();
    let providers_iter = [spec.provider.as_str()];
    let mut map: std::collections::HashMap<
        String,
        std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>,
    > = std::collections::HashMap::new();
    for name in providers_iter {
        let p = provider_by_name(name, &cfg)
            .map_err(|e| anyhow::anyhow!("api fallback: cannot build provider {name}: {e}"))?;
        map.insert(name.to_string(), p);
    }
    Ok(map)
}

/// Read `GROKRXIV_CLI_TIMEOUT_SECS` (default 180s).
fn cli_timeout() -> Duration {
    let secs = std::env::var("GROKRXIV_CLI_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CLI_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Compose the per-CLI command. Pure: does not spawn anything. For codex it
/// also materialises the schema JSON to a temp file under
/// `$TMPDIR/grokrxiv-schemas/` so the unit tests can assert the path shape.
fn build_command(
    runner: &CliRunner,
    spec: &AgentSpec,
    prompt: &str,
) -> anyhow::Result<BuiltCommand> {
    let program = runner.binary_for(&spec.provider)?;
    let role_slug = role_slug(spec.role);

    let (args, schema_path) = match spec.provider.as_str() {
        "claude" => {
            // Pass the prompt via stdin (`-p -`) to avoid argv-length limits.
            // NOTE: claude CLI does NOT have a `--skill` flag — skills are
            // invoked via `/skill-name` at the start of the prompt itself
            // (the help text says "Skills still resolve via /skill-name").
            // The prompt prefix is prepended in `build_prompt_for_claude`.
            let args = vec![
                "-p".to_string(),
                "-".to_string(),
                "--model".to_string(),
                spec.model.clone(),
                "--output-format".to_string(),
                "json".to_string(),
            ];
            (args, None)
        }
        "openai" => {
            // codex doesn't read prompts from stdin in `exec`; it takes a
            // positional prompt arg. We still capture it in `stdin_payload`
            // for symmetry with the other branches, but we pass it as the
            // final positional arg. Long prompts: codex handles multi-line
            // strings fine, and we are bounded by the OS argv limit only on
            // truly enormous inputs (>1MB on macOS / >2MB on Linux).
            let path = write_codex_schema(role_slug, &spec.schema)?;
            let args = vec![
                "exec".to_string(),
                "--json".to_string(),
                "--output-schema".to_string(),
                path.to_string_lossy().into_owned(),
                prompt.to_string(),
            ];
            (args, Some(path))
        }
        "gemini" => {
            let args = vec![
                "-p".to_string(),
                prompt.to_string(),
                "--model".to_string(),
                spec.model.clone(),
                "--approval-mode".to_string(),
                "plan".to_string(),
            ];
            (args, None)
        }
        other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
    };

    // For claude, prepend the `/grokrxiv-review` skill invocation to the
    // prompt so claude resolves the skill (the help text confirms skills
    // are invoked via `/skill-name`, not a CLI flag).
    let stdin_payload = if spec.provider == "claude" {
        format!("/{CLAUDE_SKILL_NAME}\n\n{prompt}")
    } else {
        prompt.to_string()
    };

    Ok(BuiltCommand {
        program,
        args,
        stdin_payload,
        schema_path,
    })
}

/// Spawn the built command, pipe the prompt to stdin (claude), enforce the
/// supervisor's timeout, capture stdout/stderr, surface non-zero exit codes.
///
/// FP-RPT3b B5: when the subprocess exits non-zero AND its stderr matches a
/// known quota signature, the returned error wraps `CliError::QuotaExhausted`
/// so the supervisor can detect it via downcast and dispatch a per-stage
/// `cli_quota_fallback` (Team X2's yaml field) without re-parsing log text.
async fn exec_and_capture(
    built: &BuiltCommand,
    timeout_dur: Duration,
    role: grokrxiv_schemas::AgentRole,
    provider: &str,
) -> anyhow::Result<String> {
    let mut cmd = Command::new(&built.program);
    cmd.args(&built.args);

    // Only `claude` reads its prompt from stdin in our wiring. For codex /
    // gemini the prompt is in argv, but we still set stdin to null to avoid
    // the child blocking on an inherited terminal.
    let uses_stdin = built
        .args
        .iter()
        .zip(built.args.iter().skip(1))
        .any(|(a, b)| a == "-p" && b == "-");
    if uses_stdin {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{}`: {e}", built.program))?;

    if uses_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(built.stdin_payload.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("failed to write prompt to stdin: {e}"))?;
            // Drop closes stdin so the child sees EOF and proceeds.
            drop(stdin);
        }
    }

    let wait_fut = child.wait_with_output();
    let output = match timeout(timeout_dur, wait_fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => anyhow::bail!("waiting on `{}` failed: {e}", built.program),
        Err(_) => {
            // Timed out — we already moved `child` into wait_with_output, so
            // we can't call .kill(). wait_with_output drops the child handle
            // which on tokio signals the process and reaps it. To be safe
            // surface a clear error.
            anyhow::bail!(
                "CliRunner timed out after {}s for role {:?}",
                timeout_dur.as_secs(),
                role
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        // FP-RPT3b B5: classify as a structured quota error when stderr
        // matches a known signature. The caller can then fall back to a
        // different runner instead of treating it as a generic subprocess
        // failure.
        if let Some(snippet) = detect_quota_signal(&stderr) {
            return Err(anyhow::Error::new(CliError::QuotaExhausted {
                provider: provider.to_string(),
                message: snippet,
            })
            .context(format!(
                "`{}` exited with {:?} for role {:?}",
                built.program,
                output.status.code(),
                role,
            )));
        }
        anyhow::bail!(
            "`{}` exited with {:?} for role {:?}: {stderr}",
            built.program,
            output.status.code(),
            role,
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(stdout)
}

/// Extract the JSON payload from a CLI's raw stdout. Claude wraps the
/// model's reply in `{"type":"result","subtype":"success","result":"<json>",
/// ...}`; codex emits JSON directly with `--json`; gemini emits the raw
/// completion. For claude we walk the wrapper; for the others we return the
/// stdout unchanged.
/// Strip leading ```json / ``` and trailing ``` from a JSON payload. Returns
/// the input unchanged if no fences are present. Mirrors `strip_fences` in
/// `agents/runners/api.rs`.
pub fn strip_code_fences(s: &str) -> &str {
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

fn extract_json_text(provider: &str, raw_stdout: &str) -> String {
    let trimmed = raw_stdout.trim();
    match provider {
        "claude" => {
            // `claude -p --output-format json` returns
            // {"type":"result","subtype":"success","result":"<json-string>", ...}
            let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                return trimmed.to_string();
            };
            match wrapper.get("result") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => trimmed.to_string(),
            }
        }
        "openai" => {
            // `codex exec --json` streams JSONL events:
            //   {"type":"thread.started",...}
            //   {"type":"turn.started"}
            //   {"type":"item.started", "item": {...web_search, etc.}}
            //   ...
            //   {"type":"item.completed", "item": {"type":"agent_message", "text":"<json>"}}
            //   {"type":"turn.completed",...}
            // The real output is the LAST item.completed with type=agent_message;
            // its `text` field is the JSON we want.
            let mut last_agent_text: Option<String> = None;
            for line in trimmed.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(evt) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if evt.get("type") == Some(&serde_json::Value::String("item.completed".into())) {
                    if let Some(item) = evt.get("item") {
                        if item.get("type")
                            == Some(&serde_json::Value::String("agent_message".into()))
                        {
                            if let Some(serde_json::Value::String(t)) = item.get("text") {
                                last_agent_text = Some(t.clone());
                            }
                        }
                    }
                }
            }
            last_agent_text.unwrap_or_else(|| trimmed.to_string())
        }
        _ => trimmed.to_string(),
    }
}

/// Parse the extracted text as JSON and validate against the role schema. The
/// validation matches what `JsonSchemaVerifier` does in the verifier crate.
fn parse_and_validate(
    extracted: &str,
    schema: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    // Strip ```json / ``` code fences before parsing — claude's
    // /grokrxiv-review skill output is sometimes wrapped in a fenced code
    // block even when --output-format=json is set. Mirrors the helper in
    // ApiRunner.
    let cleaned = strip_code_fences(extracted.trim());
    let parsed: serde_json::Value = serde_json::from_str(cleaned)
        .map_err(|e| anyhow::anyhow!("not valid JSON: {e}; raw={extracted:?}"))?;

    // Empty schema {} = no constraint. Skip validation in that case so unit
    // tests with stub specs keep working.
    if schema.is_null()
        || (schema.is_object() && schema.as_object().map(|m| m.is_empty()).unwrap_or(false))
    {
        return Ok(parsed);
    }

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| anyhow::anyhow!("invalid role schema: {e}"))?;
    let errors: Vec<String> = validator.iter_errors(&parsed).map(|e| e.to_string()).collect();
    if !errors.is_empty() {
        anyhow::bail!("schema validation failed: {}", errors.join("; "));
    }
    Ok(parsed)
}

/// Render `AgentRole` to a stable snake-case slug (used to name the codex
/// schema temp file).
fn role_slug(role: grokrxiv_schemas::AgentRole) -> &'static str {
    use grokrxiv_schemas::AgentRole;
    match role {
        AgentRole::Summary => "summary",
        AgentRole::TechnicalCorrectness => "technical_correctness",
        AgentRole::Novelty => "novelty",
        AgentRole::Reproducibility => "reproducibility",
        AgentRole::Citation => "citation",
        AgentRole::MetaReviewer => "meta_reviewer",
    }
}

/// Persist the role's JSON schema to `$TMPDIR/grokrxiv-schemas/<role>.schema.json`
/// for codex's `--output-schema` flag. The directory is created if needed.
fn write_codex_schema(
    role_slug: &str,
    schema: &serde_json::Value,
) -> anyhow::Result<PathBuf> {
    let mut dir = std::env::temp_dir();
    dir.push("grokrxiv-schemas");
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dir.display()))?;
    let path = dir.join(format!("{role_slug}.schema.json"));
    let body = serde_json::to_vec_pretty(schema)
        .map_err(|e| anyhow::anyhow!("failed to serialise schema: {e}"))?;
    std::fs::write(&path, body)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(path)
}

/// Best-effort cleanup of a codex schema temp file. Errors are swallowed —
/// the file is in `$TMPDIR` and the OS reclaims it.
fn cleanup_schema_path(path: &Option<PathBuf>) {
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
}

/// FP-RPT3b B4: log the per-provider auth surface exactly once per process.
/// Best-effort: any I/O failure becomes `auth_method=unknown` rather than a
/// hard error. Reads config files but never writes to them.
fn log_auth_path_once(provider: &str) {
    match provider {
        "claude" => {
            if CLAUDE_AUTH_LOGGED.set(()).is_ok() {
                let (auth_method, account_type, billing_type) = inspect_claude_auth();
                tracing::info!(
                    event = "cli_auth_path",
                    provider = "claude",
                    auth_method = %auth_method,
                    account_type = %account_type,
                    billing_type = %billing_type,
                    "claude CLI auth path"
                );
            }
        }
        "openai" => {
            if CODEX_AUTH_LOGGED.set(()).is_ok() {
                let (auth_method, plan_type) = inspect_codex_auth();
                tracing::info!(
                    event = "cli_auth_path",
                    provider = "openai",
                    auth_method = %auth_method,
                    plan_type = %plan_type,
                    "codex CLI auth path"
                );
                // Per the FP-RPT3b audit: codex's `chatgpt_plus` / `chatgpt_pro`
                // tiers include a metered API budget but are NOT a flat $0
                // subscription. Warn unless the auth tier is explicitly
                // recognised as a personal subscription with included usage.
                if auth_method != "chatgpt_subscription" {
                    tracing::warn!(
                        provider = "openai",
                        auth_method = %auth_method,
                        "codex CLI runs against OpenAI API — will incur per-token billing"
                    );
                }
            }
        }
        "gemini" => {
            if GEMINI_AUTH_LOGGED.set(()).is_ok() {
                let (auth_method, account, quota_project) = inspect_gemini_auth();
                tracing::info!(
                    event = "cli_auth_path",
                    provider = "gemini",
                    auth_method = %auth_method,
                    account = %account,
                    quota_project = %quota_project,
                    "gemini CLI auth path"
                );
                // gemini CLI on `cloud-platform` scope routes through the
                // gcloud quota project, which is metered. Same warning pattern
                // as codex.
                if auth_method != "personal_subscription" {
                    tracing::warn!(
                        provider = "gemini",
                        auth_method = %auth_method,
                        "gemini CLI runs against Google AI / Vertex API — will incur per-token billing"
                    );
                }
            }
        }
        _ => {}
    }
}

/// Best-effort read of `~/.claude.json` to surface `oauthAccount.billingType`
/// and `oauthAccount.organizationType`. Returns a `(auth_method, account_type,
/// billing_type)` triple where each field falls back to `"unknown"` on any
/// I/O / parse failure.
fn inspect_claude_auth() -> (String, String, String) {
    let Ok(home) = std::env::var("HOME") else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let path = PathBuf::from(home).join(".claude.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let oauth = val.get("oauthAccount").cloned().unwrap_or(serde_json::Value::Null);
    let billing_type = oauth
        .get("billingType")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let account_type = oauth
        .get("organizationType")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    // Derive an auth_method tag from the observed fields. `stripe_subscription`
    // + `claude_max` (or `claude_pro`) on the org indicates the operator's
    // CLI is backed by their personal Anthropic subscription. Anything else
    // we tag as `api_key` so the operator can spot the cost path immediately.
    let auth_method = match (billing_type.as_str(), account_type.as_str()) {
        ("stripe_subscription", t) if t.starts_with("claude_") => "personal_subscription",
        ("stripe_subscription", _) => "stripe_subscription",
        (_, "unknown") => "unknown",
        _ => "api_key",
    }
    .to_string();
    (auth_method, account_type, billing_type)
}

/// Best-effort read of `~/.codex/auth.json` to surface whether the codex CLI
/// is authenticated against a ChatGPT subscription (token JWT carries
/// `chatgpt_plan_type`) or a raw `OPENAI_API_KEY`. Returns `(auth_method,
/// plan_type)`.
fn inspect_codex_auth() -> (String, String) {
    let Ok(home) = std::env::var("HOME") else {
        return ("unknown".into(), "unknown".into());
    };
    let path = PathBuf::from(home).join(".codex").join("auth.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return ("unknown".into(), "unknown".into());
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ("unknown".into(), "unknown".into());
    };

    // If `OPENAI_API_KEY` is set on the file, codex routes through the API
    // path and bills per token.
    if let Some(serde_json::Value::String(_)) = val.get("OPENAI_API_KEY") {
        return ("api_key".into(), "n/a".into());
    }

    // Otherwise look for a ChatGPT-tier JWT under `tokens.id_token`. We do
    // NOT validate or contact a server — just decode the middle base64
    // segment to read the `chatgpt_plan_type` claim if it exists.
    let id_token = val
        .get("tokens")
        .and_then(|t| t.get("id_token"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let plan_type = decode_jwt_claim(id_token, "chatgpt_plan_type").unwrap_or_else(|| "unknown".into());
    let auth_method = if plan_type != "unknown" {
        "chatgpt_subscription"
    } else if id_token.is_empty() {
        "unknown"
    } else {
        "oauth"
    };
    (auth_method.into(), plan_type)
}

/// Best-effort read of `~/.config/gcloud/application_default_credentials.json`
/// (used by the `gemini` CLI when invoked under `gcloud` auth). Returns
/// `(auth_method, account, quota_project)`.
fn inspect_gemini_auth() -> (String, String, String) {
    let Ok(home) = std::env::var("HOME") else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let path = PathBuf::from(home)
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");
    let Ok(bytes) = std::fs::read(&path) else {
        // Fall back to `~/.gemini/oauth_creds.json` (the standalone gemini CLI
        // path). We just record that we found an OAuth credential blob — the
        // scope tells us this is the gcloud metered path.
        let alt = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".gemini")
            .join("oauth_creds.json");
        if alt.exists() {
            return ("gcloud_oauth".into(), "unknown".into(), "unknown".into());
        }
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let typ = val.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
    let account = val
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let quota_project = val
        .get("quota_project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let auth_method = match typ {
        "authorized_user" => "gcloud_oauth",
        "service_account" => "service_account",
        _ => "unknown",
    }
    .to_string();
    (auth_method, account, quota_project)
}

/// Minimal JWT claim decoder. Splits on `.`, base64url-decodes the payload,
/// and returns the string-valued claim if found. Returns `None` on any
/// decode failure; we never want auth-logging to crash a run.
fn decode_jwt_claim(jwt: &str, claim: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    // base64url decode (no padding). Reuse the URL-safe alphabet via a
    // tiny manual implementation so we don't pull in a new dep.
    let bytes = base64url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get(claim).and_then(|x| x.as_str()).map(String::from)
}

/// Decode a base64url string (no padding). Returns `None` on any failure.
fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    // Convert URL-safe alphabet back to standard.
    let mut std_b64: String = s
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    // Add the padding base64 requires.
    while std_b64.len() % 4 != 0 {
        std_b64.push('=');
    }
    // Use a tiny inline base64 decoder against the standard alphabet.
    decode_b64_standard(&std_b64)
}

fn decode_b64_standard(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                pad += 1;
                buf[i] = 0;
            } else {
                let v = lookup[c as usize];
                if v == 255 {
                    return None;
                }
                buf[i] = v;
            }
        }
        let n = (buf[0] as u32) << 18
            | (buf[1] as u32) << 12
            | (buf[2] as u32) << 6
            | (buf[3] as u32);
        // First byte is always present unless the entire chunk is padding.
        out.push((n >> 16) as u8);
        // Second byte is missing only if 2+ trailing `=` were present.
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        // Third byte is missing if any trailing `=` was present.
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Install `~/.claude/skills/grokrxiv-review/SKILL.md` if it isn't already
/// there. Idempotent: a no-op when the file exists.
pub fn ensure_grokrxiv_review_skill_installed() -> anyhow::Result<()> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("$HOME not set; cannot install claude skill"))?;
    let mut dir = PathBuf::from(home);
    dir.push(".claude");
    dir.push("skills");
    dir.push(CLAUDE_SKILL_NAME);
    let skill_path = dir.join("SKILL.md");
    if skill_path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", dir.display()))?;
    std::fs::write(&skill_path, CLAUDE_SKILL_BODY)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", skill_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::types::{AgentSpec, SandboxPolicy};
    use grokrxiv_schemas::AgentRole;

    fn stub_spec(provider: &str, model: &str) -> AgentSpec {
        let mut s = AgentSpec::api_default(
            AgentRole::Summary,
            provider.to_string(),
            model.to_string(),
        );
        s.runner = AgentRunnerKind::Cli;
        s.schema = serde_json::json!({});
        s
    }

    #[test]
    fn test_provider_to_binary_mapping_claude_openai_gemini() {
        // Clear env vars so we exercise the default-name branch.
        std::env::remove_var("GROKRXIV_CLAUDE_BIN");
        std::env::remove_var("GROKRXIV_CODEX_BIN");
        std::env::remove_var("GROKRXIV_GEMINI_BIN");

        let r = CliRunner::new();
        assert_eq!(r.binary_for("claude").unwrap(), "claude");
        assert_eq!(r.binary_for("openai").unwrap(), "codex");
        assert_eq!(r.binary_for("gemini").unwrap(), "gemini");

        // Now exercise the env override path.
        std::env::set_var("GROKRXIV_CLAUDE_BIN", "/opt/bin/claude-test");
        assert_eq!(r.binary_for("claude").unwrap(), "/opt/bin/claude-test");
        std::env::remove_var("GROKRXIV_CLAUDE_BIN");
    }

    #[test]
    fn test_unsupported_provider_errors_clearly() {
        let r = CliRunner::new();
        let err = r.binary_for("foo").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported provider for CliRunner"),
            "unexpected error message: {msg}"
        );
        assert!(msg.contains("foo"), "error should name the bad provider: {msg}");
    }

    #[test]
    fn test_command_construction_claude() {
        let r = CliRunner::new();
        let spec = stub_spec("claude", "claude-opus-4-7");
        let built = build_command(&r, &spec, "hello prompt").unwrap();

        // Binary
        assert!(
            built.program.ends_with("claude"),
            "program should be claude binary, got {}",
            built.program
        );

        // Args: -p - --model <m> --output-format json
        // (Skill is invoked via the `/grokrxiv-review` prompt prefix piped to
        // stdin, NOT via a `--skill` CLI flag — claude CLI has no such flag.)
        let args = &built.args;
        assert!(args.contains(&"-p".to_string()), "missing -p in {args:?}");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "claude-opus-4-7"),
            "missing --model <model> pair in {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "json"),
            "missing --output-format json pair in {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--skill"),
            "claude CLI does not accept --skill; it must be absent ({args:?})"
        );

        // Prompt is piped to stdin with `/grokrxiv-review` prefix
        assert!(
            built.stdin_payload.starts_with("/grokrxiv-review"),
            "stdin payload should be prefixed with /grokrxiv-review, got {:?}",
            built.stdin_payload
        );
        assert!(
            built.stdin_payload.contains("hello prompt"),
            "stdin payload should contain the original prompt, got {:?}",
            built.stdin_payload
        );
        // No schema file for claude
        assert!(built.schema_path.is_none());
    }

    #[test]
    fn test_command_construction_codex() {
        let r = CliRunner::new();
        let spec = stub_spec("openai", "gpt-5-codex");
        let built = build_command(&r, &spec, "do a review").unwrap();

        assert!(
            built.program.ends_with("codex"),
            "program should be codex binary, got {}",
            built.program
        );

        let args = &built.args;
        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert!(args.contains(&"--json".to_string()), "missing --json in {args:?}");
        // --output-schema <path>
        let schema_idx = args
            .iter()
            .position(|a| a == "--output-schema")
            .expect("--output-schema flag missing");
        let schema_path = args
            .get(schema_idx + 1)
            .expect("--output-schema needs a value");
        assert!(
            schema_path.ends_with("summary.schema.json"),
            "expected role-named schema path, got {schema_path}"
        );

        // The prompt is the last positional argument.
        assert_eq!(args.last().map(String::as_str), Some("do a review"));

        // Schema file should have been materialised on disk.
        let path = built.schema_path.as_ref().expect("codex needs schema path");
        assert!(path.exists(), "schema file not written at {}", path.display());

        // Clean up.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_command_construction_gemini() {
        let r = CliRunner::new();
        let spec = stub_spec("gemini", "gemini-2.5-pro");
        let built = build_command(&r, &spec, "the prompt body").unwrap();

        assert!(
            built.program.ends_with("gemini"),
            "program should be gemini binary, got {}",
            built.program
        );

        let args = &built.args;
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-p" && w[1] == "the prompt body"),
            "missing -p <prompt> pair in {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "gemini-2.5-pro"),
            "missing --model <model> pair in {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--approval-mode" && w[1] == "plan"),
            "missing --approval-mode plan pair in {args:?}"
        );
        assert!(built.schema_path.is_none());
    }

    #[tokio::test]
    async fn test_container_sandbox_is_rejected() {
        let r = CliRunner::new();
        let mut spec = stub_spec("claude", "claude-opus-4-7");
        spec.sandbox = SandboxPolicy::Container;

        let input = AgentInput {
            paper_id: uuid::Uuid::nil(),
            review_id: uuid::Uuid::nil(),
            role: AgentRole::Summary,
            content_hash_material: serde_json::json!({}),
            artifact: serde_json::json!({}),
            system_prompt: "sys".to_string(),
            user_prompt: "usr".to_string(),
            source_bundle_path: None,
        };

        let err = r.run(&spec, &input).await.unwrap_err();
        assert!(
            err.to_string().contains("SandboxPolicy::Container"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_extract_json_text_unwraps_claude_wrapper() {
        let wrapped = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "result": "{\"foo\":\"bar\"}"
        })
        .to_string();
        let got = extract_json_text("claude", &wrapped);
        assert_eq!(got, "{\"foo\":\"bar\"}");
    }

    #[test]
    fn test_extract_json_text_passes_codex_through() {
        let raw = "{\"foo\": \"bar\"}";
        let got = extract_json_text("openai", raw);
        assert_eq!(got, raw);
    }

    #[test]
    fn test_parse_and_validate_empty_schema_skips() {
        let v =
            parse_and_validate("{\"a\":1}", &serde_json::json!({})).expect("empty schema passes");
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn test_parse_and_validate_schema_rejects_bad_shape() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        });
        let err = parse_and_validate("{\"foo\": 7}", &schema).unwrap_err();
        assert!(
            err.to_string().contains("schema validation failed"),
            "unexpected error: {err}"
        );
    }

    /// Integration test gated on `which claude`. Skips silently if claude
    /// isn't installed in PATH.
    #[tokio::test]
    async fn integration_claude_skipped_if_missing() {
        let have = std::process::Command::new("which")
            .arg("claude")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: `claude` not on PATH");
            return;
        }
        // Smoke: just ensure binary_for resolves and we don't panic.
        let r = CliRunner::new();
        assert_eq!(r.binary_for("claude").unwrap(), "claude");
    }

    // -----------------------------------------------------------------
    // FP-RPT3b B5: quota signal detection
    // -----------------------------------------------------------------

    #[test]
    fn quota_signal_matches_rate_limit_variants() {
        assert!(detect_quota_signal("Error: rate limit exceeded for user").is_some());
        assert!(detect_quota_signal("ERROR: Rate-Limit reached").is_some());
        assert!(detect_quota_signal("rate_limit hit").is_some());
    }

    #[test]
    fn quota_signal_matches_quota_variants() {
        assert!(detect_quota_signal("quota exceeded for project foo").is_some());
        assert!(detect_quota_signal("Quota Exhausted").is_some());
        assert!(detect_quota_signal("insufficient_quota").is_some());
        assert!(detect_quota_signal("insufficient quota in account").is_some());
    }

    #[test]
    fn quota_signal_matches_billing_and_429() {
        assert!(detect_quota_signal("billing: please add a payment method").is_some());
        assert!(detect_quota_signal("Payment Required").is_some());
        assert!(detect_quota_signal("HTTP 429 Too Many Requests").is_some());
        assert!(detect_quota_signal("resource_exhausted").is_some());
        assert!(detect_quota_signal("Resource Exhausted").is_some());
    }

    #[test]
    fn quota_signal_ignores_generic_errors() {
        assert!(detect_quota_signal("connection refused").is_none());
        assert!(detect_quota_signal("invalid JSON").is_none());
        assert!(detect_quota_signal("").is_none());
    }

    #[test]
    fn quota_signal_truncates_to_200_chars() {
        let huge = "rate limit exceeded ".repeat(50);
        let snippet = detect_quota_signal(&huge).expect("matches");
        assert!(snippet.chars().count() <= 200);
    }

    #[test]
    fn cli_error_display_includes_provider_and_fallback_hint() {
        let err = CliError::QuotaExhausted {
            provider: "openai".into(),
            message: "Error: rate limit exceeded".into(),
        };
        let s = err.to_string();
        assert!(s.contains("openai"), "display missing provider tag: {s}");
        assert!(s.contains("--runner api"), "display missing fallback hint: {s}");
    }

    /// FP-RPT3b B5: end-to-end quota detection via a fake subprocess. Writes
    /// a tiny shell script that emits a known quota signature on stderr and
    /// exits 1, then asserts `exec_and_capture` returns an `anyhow::Error`
    /// whose chain carries `CliError::QuotaExhausted`.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_classifies_quota_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("grokrxiv-cli-quota-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fake-cli.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'Error: rate limit exceeded for user' >&2\nexit 1\n",
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            schema_path: None,
        };

        let err = exec_and_capture(
            &built,
            Duration::from_secs(5),
            grokrxiv_schemas::AgentRole::Summary,
            "openai",
        )
        .await
        .expect_err("subprocess should exit non-zero");

        let downcast = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>())
            .expect("error chain should carry CliError");
        match downcast {
            CliError::QuotaExhausted { provider, message } => {
                assert_eq!(provider, "openai");
                assert!(
                    message.to_lowercase().contains("rate limit"),
                    "stderr snippet missing rate-limit signal: {message}"
                );
            }
        }
    }

    /// FP-RPT3b B5: non-quota subprocess failures must NOT be classified as
    /// QuotaExhausted; they should keep bubbling up as generic anyhow errors.
    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_does_not_misclassify_generic_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("grokrxiv-cli-generic-fail-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fake-cli.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'segfault in libfoo.so' >&2\nexit 139\n",
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            schema_path: None,
        };

        let err = exec_and_capture(
            &built,
            Duration::from_secs(5),
            grokrxiv_schemas::AgentRole::Summary,
            "claude",
        )
        .await
        .expect_err("subprocess should exit non-zero");

        let downcast = err.chain().find_map(|cause| cause.downcast_ref::<CliError>());
        assert!(
            downcast.is_none(),
            "non-quota failures must not be tagged as QuotaExhausted"
        );
    }

    // -----------------------------------------------------------------
    // FP-RPT3b B4: JWT-claim helper used by codex auth inspection
    // -----------------------------------------------------------------

    #[test]
    fn jwt_claim_decoder_extracts_string_claim() {
        // Build a JWT-shaped string: header.payload.sig where the payload is
        // base64url-encoded JSON.
        let payload = serde_json::json!({"chatgpt_plan_type": "plus", "sub": "x"});
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let payload_b64 = b64url_encode(&payload_bytes);
        let jwt = format!("HEADER.{payload_b64}.SIG");
        assert_eq!(
            decode_jwt_claim(&jwt, "chatgpt_plan_type"),
            Some("plus".into())
        );
        assert_eq!(decode_jwt_claim(&jwt, "missing"), None);
        assert_eq!(decode_jwt_claim("notajwt", "x"), None);
    }

    /// Reference base64url encoder for the JWT decoder unit test.
    fn b64url_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = (b0 as u32) << 16 | (b1 as u32) << 8 | (b2 as u32);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[((n >> 6) & 0x3f) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(n & 0x3f) as usize] as char);
            }
        }
        out
    }
}
