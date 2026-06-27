//! `CliRunner` — local CLI subprocess for tool-using agents.
//!
//! Spawns `claude` / `codex` / `agy` based on `spec.provider`. No runtime
//! `--cli-agent` flag — the YAML's existing `provider:` field is the source
//! of truth.
//!
//! RPT2 Track B: host-only execution. `SandboxPolicy::Container` is explicitly
//! rejected so callers don't silently get "ran on host when you asked for
//! container".

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::agents::types::{
    AgentInput, AgentRun, AgentRunnerKind, AgentSpec, Message, SandboxPolicy, ToolCompletion,
    ToolSpec,
};
use crate::agents::AgentRunner;
use crate::runtime_config::{role_env_suffix, ALLOW_PROVIDER_API_ENV};
use agenthero_agent_runtime::ToolCtx;
use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, Usage};

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
    /// The CLI subprocess exceeded the role timeout and was killed by the
    /// supervisor. This is structured so the caller can record benchmark and
    /// audit evidence without scraping error text.
    TimedOut {
        /// Provider tag from `AgentSpec.provider`.
        provider: String,
        /// DAG role/node label being executed.
        role: String,
        /// Timeout enforced by the supervisor.
        timeout_secs: u64,
        /// Whether process reaping confirmed that the subprocess exited.
        subprocess_status: String,
    },
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::QuotaExhausted { provider, message } => write!(
                f,
                "{provider} CLI quota exhausted. Set --runner api or wait for reset. \
                 message={message}"
            ),
            CliError::TimedOut {
                role,
                timeout_secs,
                subprocess_status,
                ..
            } => write!(
                f,
                "CliRunner timed out after {timeout_secs}s for role {role} \
                 (subprocess {subprocess_status})"
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

/// Default subprocess timeout (seconds) when `AGENTHERO_CLI_TIMEOUT_SECS` is
/// unset.
const DEFAULT_CLI_TIMEOUT_SECS: u64 = 360;
const AGENT_BENCHMARK_PATH_ENV: &str = "GROKRXIV_AGENT_BENCHMARK_PATH";
const CITATION_TIMEOUT_MAX_ENV: &str = "GROKRXIV_CITATION_TIMEOUT_MAX_SECS";
const DEFAULT_CITATION_TIMEOUT_MAX_SECS: u64 = 1_800;
const FORMALIZE_TYPED_IR_TIMEOUT_MAX_ENV: &str = "GROKRXIV_FORMALIZE_TYPED_IR_TIMEOUT_MAX_SECS";
const DEFAULT_FORMALIZE_TYPED_IR_TIMEOUT_MAX_SECS: u64 = 1_800;
const LEAN_CLAUDE_TIMEOUT_ENV: &str = "GROKRXIV_LEAN_CLAUDE_TIMEOUT_SECS";
const LEAN_AUTHOR_TIMEOUT_MAX_ENV: &str = "GROKRXIV_LEAN_PROOF_AUTHOR_TIMEOUT_MAX_SECS";
const DEFAULT_LEAN_AUTHOR_TIMEOUT_MAX_SECS: u64 = 1_800;
const ADAPTIVE_TIMEOUT_MIN_SAMPLES: usize = 3;
const ADAPTIVE_TIMEOUT_SAMPLE_LIMIT: usize = 20;

/// Provider API credentials that must not leak into local CLI children.
///
/// The CLIs should use their own logged-in account state for subscription
/// billing. Keeping API keys in the parent process is fine for explicit
/// `--runner api` / `--extractor api`, but a CLI child must not silently switch
/// to direct provider API billing because it inherited one of these vars.
const PROVIDER_API_ENV_VARS_TO_SCRUB: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENROUTER_API_KEY",
    "VLLM_API_KEY",
];

/// Name of the Claude skill that enforces JSON-only output.
const CLAUDE_SKILL_NAME: &str = "grokrxiv-review";

/// Body of the Claude skill (`SKILL.md`) installed on first invocation.
const CLAUDE_SKILL_BODY: &str = "---
name: grokrxiv-review
description: Specialist reviewer for grokrxiv. Emits JSON-only output strictly matching the role's schema.
---

You are a specialist reviewer for grokrxiv. The user supplies:
- a DAG-scoped role tag
- the input artifact for that DAG node
- the JSON schema for that role's output

You MUST output a SINGLE JSON object that validates against the schema. NO prose, NO markdown fences, NO commentary. If you cannot, output `{\"error\":\"<one-line reason>\"}`.
";

/// FP-RPT3b B4: one-shot guard per provider for the auth-path log line.
/// `CliRunner` is constructed once and shared, so we only need a single
/// `OnceLock` per provider tag to make sure the auth surface is logged
/// exactly once per orchestrator process.
static CLAUDE_AUTH_LOGGED: OnceLock<()> = OnceLock::new();
static CODEX_AUTH_LOGGED: OnceLock<()> = OnceLock::new();
static ANTIGRAVITY_AUTH_LOGGED: OnceLock<()> = OnceLock::new();

/// Spawns local CLI binaries (`claude` / `codex` / `agy`) based on
/// `spec.provider`. The binary path for each is overridable via
/// `AGENTHERO_CLAUDE_BIN` / `AGENTHERO_CODEX_BIN` /
/// `AGENTHERO_ANTIGRAVITY_BIN`.
/// Default timeout 360s via `AGENTHERO_CLI_TIMEOUT_SECS`.
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
            "claude" => {
                Ok(std::env::var("AGENTHERO_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()))
            }
            "openai" => {
                Ok(std::env::var("AGENTHERO_CODEX_BIN").unwrap_or_else(|_| "codex".to_string()))
            }
            "gemini" => Ok(antigravity_binary()),
            other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
        }
    }
}

fn antigravity_binary() -> String {
    std::env::var("AGENTHERO_ANTIGRAVITY_BIN")
        .or_else(|_| std::env::var("AGENTHERO_AGY_BIN"))
        .unwrap_or_else(|_| "agy".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliProviderBackend {
    Claude,
    Codex,
    Antigravity,
}

fn cli_provider_backend(provider: &str, _program: &str) -> anyhow::Result<CliProviderBackend> {
    match provider {
        "claude" => Ok(CliProviderBackend::Claude),
        "openai" => Ok(CliProviderBackend::Codex),
        "gemini" => Ok(CliProviderBackend::Antigravity),
        other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
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
    /// Whether `stdin_payload` should be piped to the child. This is explicit
    /// so Claude's documented `cat file | claude -p "query"` path does not
    /// depend on an undocumented `-p -` sentinel.
    pipe_stdin: bool,
    /// For codex: the schema file path written before invocation (so the test
    /// helper can assert it was placed and the runtime helper can clean up).
    /// `None` for claude / gemini.
    schema_path: Option<PathBuf>,
    /// Working directory for the child process. Keeping CLI children out of
    /// the repo root prevents provider CLIs from scanning the whole checkout
    /// when they fall back to their own local tools.
    cwd: Option<PathBuf>,
}

impl CliRunner {
    async fn run_once(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
        // 1. Reject Container sandbox explicitly — RPT2 Track B is host-only.
        if matches!(spec.sandbox, SandboxPolicy::Container) {
            anyhow::bail!(
                "SandboxPolicy::Container is not supported in RPT2 — set --sandbox none or update your YAML"
            );
        }

        let started = Instant::now();
        let timeout_dur = cli_timeout_for(spec);

        // FP-RPT3b B4: surface the per-provider auth path at INFO level once
        // per process so the operator can audit the $0-marginal-cost claim
        // against the actual auth tier.
        let auth_program = self.binary_for(&spec.provider)?;
        let auth_backend = cli_provider_backend(&spec.provider, &auth_program)?;
        log_auth_path_once(&spec.provider, auth_backend);

        // 2. Pre-flight: ensure the Claude skill is installed on disk before
        //    spawning. Idempotent.
        if spec.provider == "claude" {
            if let Err(e) = ensure_grokrxiv_review_skill_installed() {
                tracing::warn!(err = %e, "failed to install grokrxiv-review claude skill");
            }
        }

        let review_workdir = prepare_review_workdir(spec, input)?;
        let prompt = render_review_prompt_with_files(input);
        emit_cli_input_contract(spec, &review_workdir);

        // 3. First attempt.
        let mut built = build_command(self, spec, &prompt)?;
        built.cwd = Some(review_workdir.path().to_path_buf());
        emit_cli_command_contract(spec, &built, auth_backend, timeout_dur);
        let raw_stdout =
            match exec_and_capture(&built, timeout_dur, &spec.role, &spec.provider).await {
                Ok(s) => s,
                Err(e) => {
                    record_cli_latency_error_sample(
                        spec,
                        started.elapsed(),
                        timeout_dur,
                        &review_workdir,
                        &e,
                    );
                    cleanup_schema_path(&built.schema_path);
                    return Err(e);
                }
            };
        cleanup_schema_path(&built.schema_path);

        // 4. Extract and validate JSON. On parse OR schema-validation failure,
        //    one-shot corrective retry.
        let mut raw_output_for_audit = raw_stdout.clone();
        let extracted = extract_json_text(&spec.provider, &raw_stdout);
        let parsed = match parse_and_validate(&extracted, spec.schema.as_ref()) {
            Ok(v) => v,
            Err(first_err) => {
                let corrective = format!(
                    "Your previous output failed JSON/schema validation.\n\
                     Validation error:\n{first_err}\n\n\
                     Return exactly one JSON object and make it validate against this schema. \
                     Do not include prose, markdown fences, or extra properties.\n\n\
                     Schema:\n{schema}\n\n\
                     Original task:\n{prompt}",
                    schema = serde_json::to_string(spec.schema.as_ref()).unwrap_or_default(),
                    prompt = prompt,
                );
                let mut built2 = build_command(self, spec, &corrective)?;
                built2.cwd = Some(review_workdir.path().to_path_buf());
                emit_cli_command_contract(spec, &built2, auth_backend, timeout_dur);
                let raw2 = match exec_and_capture(&built2, timeout_dur, &spec.role, &spec.provider)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        record_cli_latency_error_sample(
                            spec,
                            started.elapsed(),
                            timeout_dur,
                            &review_workdir,
                            &e,
                        );
                        cleanup_schema_path(&built2.schema_path);
                        return Err(e);
                    }
                };
                cleanup_schema_path(&built2.schema_path);
                let extracted2 = extract_json_text(&spec.provider, &raw2);
                match parse_and_validate(&extracted2, spec.schema.as_ref()) {
                    Ok(v) => {
                        raw_output_for_audit = raw2;
                        v
                    }
                    Err(second_err) => {
                        return Err(anyhow::anyhow!(
                            "CliRunner parse/validate failure after corrective retry for role {role}: first={first_err}; retry={second_err}",
                            role = spec.role,
                        ));
                    }
                }
            }
        };

        let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
        record_cli_latency_sample(
            spec,
            latency_ms,
            timeout_dur,
            &review_workdir,
            CliLatencySampleStatus::Success,
            None,
        );

        Ok(AgentRun {
            role: spec.role.clone(),
            runner: AgentRunnerKind::Cli,
            model: spec.model.clone(),
            output: parsed,
            raw_output: Some(raw_output_for_audit),
            tokens_in: None,
            tokens_out: None,
            latency_ms,
            cache_hit: false,
            sandbox_ref: None,
            verifier_status: None,
            verifier_notes: None,
        })
    }
}

#[async_trait]
impl AgentRunner for CliRunner {
    fn name(&self) -> &'static str {
        "cli"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
        match self.run_once(spec, input).await {
            Ok(run) => Ok(run),
            Err(err) => {
                let Some(fallback_spec) = cli_quota_fallback_spec(spec, &err) else {
                    return Err(err);
                };
                tracing::warn!(
                    role = %spec.role,
                    provider = %spec.provider,
                    fallback_provider = %fallback_spec.provider,
                    fallback_model = %fallback_spec.model,
                    error = %err,
                    "CLI provider quota exhausted; retrying role with fallback CLI provider"
                );
                self.run_once(&fallback_spec, input).await
            }
        }
    }

    async fn complete_with_tools(
        &self,
        spec: &AgentSpec,
        messages: &[Message],
        tools: &[ToolSpec],
        ctx: &ToolCtx<'_>,
    ) -> anyhow::Result<ToolCompletion> {
        // Legacy escape valve kept for old smoke scripts. The canonical
        // operator surface is now `AGENTHERO_EXTRACTOR=api`, which selects the
        // ApiRunner before this method is called.
        let fallback = std::env::var("AGENTHERO_EXTRACTION_TOOL_FALLBACK")
            .ok()
            .filter(|s| s == "api");
        if fallback.is_some() {
            if !extractor_api_selected() || !direct_provider_api_allowed() {
                anyhow::bail!(
                    "AGENTHERO_EXTRACTION_TOOL_FALLBACK=api refused because direct provider API \
                     is disabled for this CLI run; use --extractor api to allow API billing, \
                     or --extractor cli to use local logged-in CLIs"
                );
            }
            let providers = build_api_fallback_providers(spec)?;
            let api = super::api::ApiRunner::new(providers);
            return api.complete_with_tools(spec, messages, tools, ctx).await;
        }

        let program = self.binary_for(&spec.provider)?;
        let backend = cli_provider_backend(&spec.provider, &program)?;
        if !matches!(
            backend,
            CliProviderBackend::Claude | CliProviderBackend::Antigravity
        ) {
            anyhow::bail!(
                "CliRunner.complete_with_tools: provider `{}` is not supported for native \
                 CLI extraction; set AGENTHERO_EXTRACTOR=api or --extractor api for this run",
                spec.provider
            );
        }

        let started = Instant::now();
        let timeout_dur = cli_timeout_for(spec);
        log_auth_path_once(&spec.provider, backend);

        let prompt = render_tool_prompt(spec, messages, tools, ctx)?;
        let built = build_tool_command(self, spec, &prompt, ctx.workdir)?;
        let raw_stdout =
            match exec_and_capture(&built, timeout_dur, &spec.role, &spec.provider).await {
                Ok(s) => s,
                Err(e) => return Err(e),
            };

        match parse_tool_completion(&spec.provider, &raw_stdout, tools) {
            Ok(mut completion) => {
                completion.raw = enrich_cli_tool_raw(completion.raw, started.elapsed());
                Ok(completion)
            }
            Err(first_err) => {
                let extracted = extract_json_text(&spec.provider, &raw_stdout);

                // (1) Deterministic salvage — recover the payload locally ($0, no model call) by
                // fixing common malformations (raw control chars in strings, trailing commas).
                // Accepted only when the repaired text parses AND the envelope is well-formed, so
                // it never silently truncates or drops nodes.
                if let Some(repaired) = repair_malformed_json(&extracted) {
                    if let Ok(envelope) = parse_tool_envelope(&repaired, tools) {
                        tracing::warn!(
                            role = %spec.role,
                            provider = %spec.provider,
                            "CliRunner recovered a malformed tool-envelope via deterministic JSON salvage"
                        );
                        let mut completion = tool_completion_from_envelope(
                            envelope,
                            &spec.provider,
                            &raw_stdout,
                            &repaired,
                        );
                        completion.raw = enrich_cli_tool_raw(completion.raw, started.elapsed());
                        return Ok(completion);
                    }
                }

                // (2) LLM JSON-repair call — feed the malformed payload + the exact parse error
                // back to the model, ask it to fix ONLY the syntax (preserving all content), and
                // feed the corrected envelope back into the run. A large extraction must never be
                // discarded over one bad character.
                let repair_prompt = format!(
                    "The text between <payload> and </payload> was meant to be a SINGLE JSON \
                     tool-call envelope but failed to parse.\n\
                     Parse error: {first_err}\n\n\
                     Fix ONLY the JSON syntax so it parses. Preserve ALL content exactly — every \
                     `tool_calls` entry and every field, especially the complete `arguments` (e.g. \
                     the full `theorem_graph` array). Properly escape characters inside string \
                     values (backslashes, double quotes, and newlines). Do not drop, summarize, or \
                     add nodes. Return ONLY the corrected JSON object — no markdown, no code \
                     fences, no commentary.\n\n\
                     <payload>\n{extracted}\n</payload>"
                );
                let built_repair = build_tool_command(self, spec, &repair_prompt, ctx.workdir)?;
                if let Ok(repaired_raw) =
                    exec_and_capture(&built_repair, timeout_dur, &spec.role, &spec.provider).await
                {
                    if let Ok(mut completion) =
                        parse_tool_completion(&spec.provider, &repaired_raw, tools)
                    {
                        tracing::warn!(
                            role = %spec.role,
                            provider = %spec.provider,
                            "CliRunner recovered a malformed tool-envelope via LLM JSON-repair call"
                        );
                        completion.raw = enrich_cli_tool_raw(completion.raw, started.elapsed());
                        return Ok(completion);
                    }
                }

                // (3) Last resort — re-emit corrective retry (regenerate from the original prompt).
                let corrective = format!(
                    "{prompt}\n\n\
                     Your previous response could not be parsed as a GrokRxiv tool-call \
                     envelope.\n\
                     Error: {first_err}\n\n\
                     Return exactly one JSON object with this shape and no markdown fences:\n\
                     {{\"text\":\"optional short note\",\"tool_calls\":[{{\"id\":\"call_1\",\
                     \"name\":\"tool_name\",\"arguments\":{{}}}}]}}\n\
                     Use only the available tool names. If the extraction is complete, call \
                     submit with the final payload."
                );
                let built2 = build_tool_command(self, spec, &corrective, ctx.workdir)?;
                let raw2 = match exec_and_capture(&built2, timeout_dur, &spec.role, &spec.provider)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => return Err(e),
                };
                parse_tool_completion(&spec.provider, &raw2, tools)
                    .map(|mut completion| {
                        completion.raw = enrich_cli_tool_raw(completion.raw, started.elapsed());
                        completion
                    })
                    .map_err(|second_err| {
                        anyhow::anyhow!(
                            "CliRunner tool-envelope parse failure after deterministic salvage, LLM \
                             repair, and corrective retry for provider={} model={}: \
                             first={first_err}; retry={second_err}",
                            spec.provider,
                            spec.model,
                        )
                    })
            }
        }
    }
}

fn enrich_cli_tool_raw(mut raw: serde_json::Value, elapsed: Duration) -> serde_json::Value {
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "cli_latency_ms".to_string(),
            serde_json::Value::Number((elapsed.as_millis() as u64).into()),
        );
        return raw;
    }
    serde_json::json!({
        "raw": raw,
        "cli_latency_ms": elapsed.as_millis() as u64,
    })
}

struct PreparedReviewWorkdir {
    path: PathBuf,
    _tempdir: Option<tempfile::TempDir>,
}

impl PreparedReviewWorkdir {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn prepare_review_workdir(
    spec: &AgentSpec,
    input: &AgentInput,
) -> anyhow::Result<PreparedReviewWorkdir> {
    let (path, tempdir) = if let Some(source_bundle_path) = input.source_bundle_path.as_deref() {
        let path = PathBuf::from(source_bundle_path);
        std::fs::create_dir_all(&path)
            .map_err(|e| anyhow::anyhow!("create review CLI harness {}: {e}", path.display()))?;
        (path, None)
    } else {
        let dir = tempfile::Builder::new()
            .prefix("grokrxiv-review-")
            .tempdir()
            .map_err(|e| anyhow::anyhow!("create review CLI workdir: {e}"))?;
        (dir.path().to_path_buf(), Some(dir))
    };
    write_json_file(&path.join("review_input.json"), &input.artifact)?;
    write_json_file(&path.join("schema.json"), spec.schema.as_ref())?;
    std::fs::write(path.join("prompt.md"), &input.user_prompt)
        .map_err(|e| anyhow::anyhow!("write prompt.md: {e}"))?;
    std::fs::write(path.join("system.md"), &input.system_prompt)
        .map_err(|e| anyhow::anyhow!("write system.md: {e}"))?;
    std::fs::write(
        path.join("README.md"),
        "GrokRxiv prepared this directory for one review role.\n\
         Use system.md as the role instruction and prompt.md as the task. \
         review_input.json is the canonical audit artifact and backup source; \
         do not read it wholesale unless prompt.md explicitly needs a field not already rendered there. \
         Use schema.json as the required output schema.\n\
         Do not search parent directories or the GrokRxiv repository.\n",
    )
    .map_err(|e| anyhow::anyhow!("write README.md: {e}"))?;
    Ok(PreparedReviewWorkdir {
        path,
        _tempdir: tempdir,
    })
}

fn write_json_file(path: &std::path::Path, value: &serde_json::Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| anyhow::anyhow!("serialise {}: {e}", path.display()))?;
    std::fs::write(path, body).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))
}

fn render_review_prompt_with_files(input: &AgentInput) -> String {
    let lean_loop = if lean_inventory_loop_role(&input.role) {
        "\n\
         This role is allowed to use the local files and tools in the current directory. \
         Read GOAL.md and PLAN.md first. Then edit GrokRxiv/Proofs.lean, run \
         `lake env lean GrokRxiv/Proofs.lean`, fix from compiler output, and repeat until \
         the goal is satisfied or the real blocker is exposed. Do not inspect parent \
         directories.\n"
    } else {
        ""
    };
    format!(
	        "GrokRxiv has prepared the exact review inputs for role `{role}` in your current working directory.\n\
	         Read and follow these files only:\n\
	         - system.md: role instruction\n\
	         - prompt.md: role-specific task with the bounded paper context already rendered\n\
	         - review_input.json: canonical audit artifact and backup source; do not read it wholesale unless prompt.md explicitly requires a missing field\n\
	         - schema.json: required output schema\n\
             - GOAL.md and PLAN.md, when present: the required agent loop contract\n\n\
	         Return exactly one JSON object that validates against schema.json. \
	         The first byte of stdout must be `{{` and the last byte must be `}}`. \
	         Do not include prose, markdown fences, a plan, a strategy, a confirmation question, \
	         or extra properties.\n\n\
	         Do not search parent directories. Do not inspect the GrokRxiv repository checkout. \
	         If you use local file tools, restrict them to the current directory and these files.{lean_loop}",
            role = role_slug(&input.role),
            lean_loop = lean_loop,
	    )
}

fn lean_inventory_loop_role(role: &str) -> bool {
    matches!(role, "lean_inventory_author" | "lean_inventory_fixer")
}

fn render_tool_prompt(
    spec: &AgentSpec,
    messages: &[Message],
    tools: &[ToolSpec],
    ctx: &ToolCtx<'_>,
) -> anyhow::Result<String> {
    let tools_json = serde_json::to_string_pretty(tools)
        .map_err(|e| anyhow::anyhow!("serialise extraction tools: {e}"))?;
    let messages_json = serde_json::to_string_pretty(messages)
        .map_err(|e| anyhow::anyhow!("serialise extraction messages: {e}"))?;
    Ok(format!(
        "You are GrokRxiv's staged extraction tool-call planner.\n\
         Provider: {provider}\n\
         Model: {model}\n\
         Paper arXiv id: {arxiv_id}\n\
         Workdir: {workdir}\n\n\
         You do not execute tools directly. GrokRxiv executes them after you propose calls.\n\
         Return ONLY one JSON object, with no prose and no markdown fences:\n\
         {{\"text\":\"optional short note\",\"tool_calls\":[{{\"id\":\"call_1\",\
         \"name\":\"tool_name\",\"arguments\":{{}}}}]}}\n\n\
         Rules:\n\
         - `tool_calls` must be an array containing at least one call.\n\
         - `name` must exactly match one available tool.\n\
         - `arguments` must be a JSON object matching that tool schema.\n\
         - To finish the extraction, call `submit` with the final schema payload.\n\
         - Do not claim a tool result until GrokRxiv sends it back in the conversation.\n\n\
         Available tools:\n{tools_json}\n\n\
         Conversation so far:\n{messages_json}\n",
        provider = spec.provider,
        model = spec.model,
        arxiv_id = ctx.source_id,
        workdir = ctx.workdir.display(),
    ))
}

fn build_tool_command(
    runner: &CliRunner,
    spec: &AgentSpec,
    prompt: &str,
    workdir: &Path,
) -> anyhow::Result<BuiltCommand> {
    let program = runner.binary_for(&spec.provider)?;
    let backend = cli_provider_backend(&spec.provider, &program)?;
    let args = match backend {
        CliProviderBackend::Claude => vec![
            "-p".to_string(),
            "Use the complete GrokRxiv extraction tool-call prompt supplied on stdin. Return only the requested JSON object.".to_string(),
            "--model".to_string(),
            spec.model.clone(),
            "--output-format".to_string(),
            "json".to_string(),
        ],
        CliProviderBackend::Antigravity => vec![
            "-p".to_string(),
            prompt.to_string(),
            "--model".to_string(),
            spec.model.clone(),
        ],
        CliProviderBackend::Codex => {
            anyhow::bail!("unsupported provider for CLI tool loop: {}", spec.provider)
        }
    };
    Ok(BuiltCommand {
        program,
        args,
        stdin_payload: prompt.to_string(),
        pipe_stdin: matches!(backend, CliProviderBackend::Claude),
        schema_path: None,
        cwd: Some(workdir.to_path_buf()),
    })
}

fn parse_tool_completion(
    provider: &str,
    raw_stdout: &str,
    tools: &[ToolSpec],
) -> anyhow::Result<ToolCompletion> {
    let extracted = extract_json_text(provider, raw_stdout);
    let envelope = parse_tool_envelope(&extracted, tools)?;
    Ok(tool_completion_from_envelope(
        envelope, provider, raw_stdout, &extracted,
    ))
}

fn tool_completion_from_envelope(
    envelope: ParsedToolEnvelope,
    provider: &str,
    raw_stdout: &str,
    extracted: &str,
) -> ToolCompletion {
    ToolCompletion {
        finish_reason: if envelope.tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolUse
        },
        text: envelope.text,
        tool_calls: envelope.tool_calls,
        usage: usage_from_cli_wrapper(provider, raw_stdout),
        raw: raw_cli_payload(provider, raw_stdout, extracted),
    }
}

/// Best-effort DETERMINISTIC repair of a malformed JSON tool-envelope so a large extraction
/// payload is recovered locally ($0, no model round-trip) instead of being discarded. Conservative:
/// returns `Some(repaired)` only when a repair pass yields parseable JSON (no silent truncation /
/// data loss); otherwise `None`, and the caller escalates to an LLM JSON-repair call. Targets the
/// common LLM mistakes on big LaTeX-dense payloads: raw control characters inside string values and
/// trailing commas before `}`/`]`.
fn repair_malformed_json(extracted: &str) -> Option<String> {
    let base = strip_code_fences(extracted.trim()).to_string();
    let escaped = escape_unescaped_control_chars(&base);
    let candidates = [
        escaped.clone(),
        strip_trailing_commas(&escaped),
        strip_trailing_commas(&base),
    ];
    candidates
        .into_iter()
        .find(|cand| serde_json::from_str::<serde_json::Value>(cand).is_ok())
}

/// Escape raw control characters (newlines/tabs/etc.) that appear INSIDE JSON string values — a
/// frequent LLM mistake when emitting multi-line LaTeX/proof text. Characters outside strings are
/// left untouched.
fn escape_unescaped_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_string {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                out.push(c);
                escaped = true;
            } else if c == '"' {
                out.push(c);
                in_string = false;
            } else if c == '\n' {
                out.push_str("\\n");
            } else if c == '\r' {
                out.push_str("\\r");
            } else if c == '\t' {
                out.push_str("\\t");
            } else if (c as u32) < 0x20 {
                out.push_str(&format!("\\u{:04x}", c as u32));
            } else {
                out.push(c);
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    out
}

/// Remove trailing commas before `}` or `]` (e.g. `{"a":1,}` -> `{"a":1}`), ignoring commas inside
/// string values.
fn strip_trailing_commas(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c as char);
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b',' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1; // drop the trailing comma
                continue;
            }
        }
        out.push(c as char);
        i += 1;
    }
    out
}

#[derive(Debug)]
struct ParsedToolEnvelope {
    text: String,
    tool_calls: Vec<ProviderToolCall>,
}

fn parse_tool_envelope(extracted: &str, tools: &[ToolSpec]) -> anyhow::Result<ParsedToolEnvelope> {
    let cleaned = strip_code_fences(extracted.trim());
    let value: serde_json::Value = serde_json::from_str(cleaned)
        .map_err(|e| anyhow::anyhow!("not valid tool-envelope JSON: {e}; raw={extracted:?}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("tool envelope must be a JSON object"))?;
    let text = object
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let calls_value = object
        .get("tool_calls")
        .ok_or_else(|| anyhow::anyhow!("tool envelope missing `tool_calls` array"))?;
    let calls = calls_value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("`tool_calls` must be an array"))?;

    let allowed: HashSet<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    let mut out = Vec::with_capacity(calls.len());
    for (idx, call) in calls.iter().enumerate() {
        let call_obj = call
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool_calls[{idx}] must be an object"))?;
        let name = call_obj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("tool_calls[{idx}].name must be a string"))?;
        if !allowed.contains(name) {
            anyhow::bail!("tool_calls[{idx}] used unknown tool `{name}`");
        }
        let arguments = call_obj
            .get("arguments")
            .or_else(|| call_obj.get("input"))
            .or_else(|| call_obj.get("parameters"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !arguments.is_object() {
            anyhow::bail!("tool_calls[{idx}].arguments must be a JSON object");
        }
        let id = call_obj
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("cli_call_{}", idx + 1));
        out.push(ProviderToolCall {
            id,
            name: name.to_string(),
            arguments,
        });
    }

    Ok(ParsedToolEnvelope {
        text,
        tool_calls: out,
    })
}

fn raw_cli_payload(provider: &str, raw_stdout: &str, extracted: &str) -> serde_json::Value {
    let wrapper = serde_json::from_str::<serde_json::Value>(raw_stdout.trim())
        .unwrap_or_else(|_| serde_json::Value::String(raw_stdout.to_string()));
    serde_json::json!({
        "provider": provider,
        "wrapper": wrapper,
        "extracted": extracted,
    })
}

fn usage_from_cli_wrapper(provider: &str, raw_stdout: &str) -> Usage {
    let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(raw_stdout.trim()) else {
        return Usage::default();
    };
    match provider {
        "claude" => {
            let usage = wrapper.get("usage").unwrap_or(&serde_json::Value::Null);
            Usage {
                tokens_in: u32_field(usage, &["input_tokens", "tokens_in", "prompt_tokens"]),
                tokens_out: u32_field(usage, &["output_tokens", "tokens_out", "completion_tokens"]),
                cache_hits: u32_field(
                    usage,
                    &[
                        "cache_read_input_tokens",
                        "cache_creation_input_tokens",
                        "cache_hits",
                    ],
                ),
            }
        }
        "gemini" => {
            let stats = wrapper.get("stats").unwrap_or(&serde_json::Value::Null);
            Usage {
                tokens_in: find_nested_token_count(
                    stats,
                    &[
                        "prompt_tokens",
                        "input_tokens",
                        "tokens_in",
                        "promptTokenCount",
                    ],
                ),
                tokens_out: find_nested_token_count(
                    stats,
                    &[
                        "completion_tokens",
                        "output_tokens",
                        "tokens_out",
                        "candidatesTokenCount",
                    ],
                ),
                cache_hits: find_nested_token_count(
                    stats,
                    &["cached_tokens", "cache_hits", "cachedContentTokenCount"],
                ),
            }
        }
        _ => Usage::default(),
    }
}

fn u32_field(value: &serde_json::Value, keys: &[&str]) -> u32 {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_u64()))
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn find_nested_token_count(value: &serde_json::Value, keys: &[&str]) -> u32 {
    if let Some(n) = keys
        .iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_u64()))
    {
        return n.min(u32::MAX as u64) as u32;
    }
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| find_nested_token_count(item, keys))
            .sum(),
        serde_json::Value::Object(map) => map
            .values()
            .map(|item| find_nested_token_count(item, keys))
            .sum(),
        _ => 0,
    }
}

/// Build a provider registry for the ApiRunner fallback. Pulls keys from the
/// environment so this works in the same shell that invoked the CLI.
fn build_api_fallback_providers(
    spec: &AgentSpec,
) -> anyhow::Result<
    std::collections::HashMap<String, std::sync::Arc<dyn grokrxiv_llm_adapter::LLMProvider>>,
> {
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

/// Resolve the subprocess timeout. Priority order:
///   1. `GROKRXIV_<ROLE>_TIMEOUT_SECS` env var as an explicit floor.
///   2. `spec.timeout_secs` from `agents/<role>.yaml`.
///   3. `AGENTHERO_CLI_TIMEOUT_SECS` as a global default for roles with no YAML timeout.
///   4. `DEFAULT_CLI_TIMEOUT_SECS`.
/// The global env var stays available for unconfigured roles, but it must not
/// inflate short typed-IR/citation roles or mask explicit app policy.
fn cli_timeout_for(spec: &AgentSpec) -> Option<Duration> {
    // 1. Operator per-role override (`GROKRXIV_<ROLE>_TIMEOUT_SECS`) wins as the configured
    //    floor. Adaptive roles can still lift it from recent successful samples.
    if let Some(secs) = timeout_env_secs(&role_timeout_env_var(&spec.role)) {
        let chosen = adaptive_cli_timeout_for(spec, secs).unwrap_or(secs);
        return Some(Duration::from_secs(chosen));
    }
    if lean_claude_role(spec) {
        if let Some(secs) = timeout_env_secs(LEAN_CLAUDE_TIMEOUT_ENV) {
            let chosen = adaptive_cli_timeout_for(spec, secs).unwrap_or(secs);
            return Some(Duration::from_secs(chosen));
        }
        return None;
    }
    // 2. Explicit app/YAML policy wins over global defaults.
    let configured = if spec.timeout_secs > 0 {
        u64::from(spec.timeout_secs)
    } else {
        timeout_env_secs("AGENTHERO_CLI_TIMEOUT_SECS").unwrap_or(DEFAULT_CLI_TIMEOUT_SECS)
    };
    let chosen = adaptive_cli_timeout_for(spec, configured).unwrap_or(configured);
    Some(Duration::from_secs(chosen))
}

pub(crate) fn cli_timeout_for_spec(spec: &AgentSpec) -> Option<Duration> {
    cli_timeout_for(spec)
}

fn lean_claude_role(spec: &AgentSpec) -> bool {
    spec.provider == "claude" && spec.role.starts_with("lean_")
}

fn timeout_env_secs(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
}

fn adaptive_cli_timeout_for(spec: &AgentSpec, configured_secs: u64) -> Option<u64> {
    let (max_env, default_max_secs) = adaptive_timeout_max_contract(spec)?;
    let samples = read_success_latency_samples(spec);
    let sample_count = samples.len();
    if sample_count < ADAPTIVE_TIMEOUT_MIN_SAMPLES {
        emit_adaptive_timeout_contract(
            spec,
            configured_secs,
            sample_count,
            None,
            configured_secs,
            "insufficient_samples",
        );
        return None;
    }
    let sum_ms: u128 = samples.iter().map(|value| u128::from(*value)).sum();
    let mean_ms = (sum_ms / sample_count as u128).min(u128::from(u64::MAX)) as u64;
    let mean_secs = mean_ms.saturating_add(999) / 1_000;
    let proposed_secs = mean_secs.saturating_mul(2).saturating_add(60);
    let max_secs = timeout_env_secs(max_env).unwrap_or(default_max_secs);
    let chosen = configured_secs
        .max(proposed_secs)
        .min(max_secs.max(configured_secs));
    emit_adaptive_timeout_contract(
        spec,
        configured_secs,
        sample_count,
        Some(mean_ms),
        chosen,
        if chosen > configured_secs {
            "benchmark"
        } else {
            "config_floor"
        },
    );
    Some(chosen)
}

fn adaptive_timeout_max_contract(spec: &AgentSpec) -> Option<(&'static str, u64)> {
    if spec.role == "citation" && spec.provider == "claude" && spec.model.contains("sonnet") {
        return Some((CITATION_TIMEOUT_MAX_ENV, DEFAULT_CITATION_TIMEOUT_MAX_SECS));
    }
    if spec.role == "formalize_source_inventory_typed_transcriber"
        && spec.provider == "claude"
        && spec.model.contains("sonnet")
    {
        return Some((
            FORMALIZE_TYPED_IR_TIMEOUT_MAX_ENV,
            DEFAULT_FORMALIZE_TYPED_IR_TIMEOUT_MAX_SECS,
        ));
    }
    if spec.role == "lean_proof_author" && spec.provider == "claude" && spec.model.contains("opus")
    {
        return Some((
            LEAN_AUTHOR_TIMEOUT_MAX_ENV,
            DEFAULT_LEAN_AUTHOR_TIMEOUT_MAX_SECS,
        ));
    }
    None
}

fn emit_adaptive_timeout_contract(
    spec: &AgentSpec,
    configured_secs: u64,
    samples: usize,
    mean_ms: Option<u64>,
    chosen_secs: u64,
    source: &str,
) {
    tracing::info!(
        role = %spec.role,
        provider = %spec.provider,
        model = %spec.model,
        configured_secs,
        samples,
        mean_ms,
        chosen_secs,
        source,
        "CLI adaptive timeout contract"
    );
    eprintln!(
        "agent_timeout_benchmark role={} provider={} model={} configured_secs={} samples={} mean_ms={} chosen_secs={} source={}",
        spec.role,
        spec.provider,
        spec.model,
        configured_secs,
        samples,
        mean_ms.map(|value| value.to_string()).unwrap_or_else(|| "-".to_string()),
        chosen_secs,
        source
    );
}

fn read_success_latency_samples(spec: &AgentSpec) -> Vec<u64> {
    let path = agent_benchmark_path();
    let _guard = agent_benchmark_io_lock();
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    body.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value.get("status").and_then(|v| v.as_str()) == Some("success")
                && value.get("role").and_then(|v| v.as_str()) == Some(spec.role.as_str())
                && value.get("provider").and_then(|v| v.as_str()) == Some(spec.provider.as_str())
                && value.get("model").and_then(|v| v.as_str()) == Some(spec.model.as_str())
        })
        .filter_map(|value| value.get("latency_ms").and_then(|v| v.as_u64()))
        .filter(|latency_ms| *latency_ms > 0)
        .take(ADAPTIVE_TIMEOUT_SAMPLE_LIMIT)
        .collect()
}

fn agent_benchmark_path() -> PathBuf {
    std::env::var(AGENT_BENCHMARK_PATH_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(".agenthero")
                .join("benchmarks")
                .join("grokrxiv")
                .join("agent_latencies.jsonl")
        })
}

fn agent_benchmark_io_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("agent benchmark lock poisoned")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliLatencySampleStatus {
    Success,
    Timeout,
    Error,
}

impl CliLatencySampleStatus {
    fn as_str(self) -> &'static str {
        match self {
            CliLatencySampleStatus::Success => "success",
            CliLatencySampleStatus::Timeout => "timeout",
            CliLatencySampleStatus::Error => "error",
        }
    }
}

fn cli_quota_fallback_spec(spec: &AgentSpec, err: &anyhow::Error) -> Option<AgentSpec> {
    let quota_error = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())?;
    match quota_error {
        CliError::QuotaExhausted { .. } => {}
        CliError::TimedOut { .. } => return None,
    }
    let provider = std::env::var("AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    if matches!(provider.as_str(), "0" | "off" | "none" | "disabled") || provider == spec.provider {
        return None;
    }
    let model = std::env::var("AGENTHERO_CLI_QUOTA_FALLBACK_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_cli_quota_fallback_model(&provider).to_string());
    let mut fallback = spec.clone();
    fallback.provider = provider;
    fallback.model = model;
    Some(fallback)
}

fn default_cli_quota_fallback_model(provider: &str) -> &'static str {
    match provider {
        "claude" => "sonnet[1m]",
        "gemini" => "Gemini 3.5 Flash (Medium)",
        _ => "gpt-5.5",
    }
}

fn role_timeout_env_var(role: &str) -> String {
    format!("GROKRXIV_{}_TIMEOUT_SECS", role_env_suffix(role))
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
    let backend = cli_provider_backend(&spec.provider, &program)?;
    let role_slug = role_slug(&spec.role);

    // A7: paper-review roles can use the `/grokrxiv-review` skill. Other app
    // roles, including formalize typed-IR, must use their own strict prompt so
    // they do not inherit stale review-role scratch output.
    let uses_review_skill = grokrxiv_review_skill_role(&role_slug);
    let provider_prompt = if matches!(
        backend,
        CliProviderBackend::Claude | CliProviderBackend::Antigravity
    ) && uses_review_skill
    {
        format!("/{CLAUDE_SKILL_NAME}\n\n{prompt}")
    } else {
        prompt.to_string()
    };

    let mut stdin_payload = provider_prompt.clone();
    let (args, schema_path, pipe_stdin) = match backend {
        CliProviderBackend::Claude => {
            // Pass the large role prompt via stdin to avoid argv-length
            // limits, but keep the actual `-p` query explicit. Claude's docs
            // document `cat file | claude -p "query"` for piped content; they
            // do not document `-p -` as a stdin sentinel.
            stdin_payload = prompt.to_string();
            let schema_for_claude = claude_json_schema(spec.schema.as_ref());
            let schema_json = serde_json::to_string(&schema_for_claude)
                .map_err(|e| anyhow::anyhow!("failed to serialise claude json schema: {e}"))?;
            // NOTE: claude CLI does NOT have a `--skill` flag — skills are
            // invoked via `/skill-name` at the start of the prompt body
            // (help text: "Skills still resolve via /skill-name").
            let query = if lean_inventory_loop_role(&spec.role) {
                "/goal Read GOAL.md and PLAN.md. Complete the Lean loop in this directory: \
                 edit GrokRxiv/Proofs.lean, run `lake env lean GrokRxiv/Proofs.lean`, \
                 fix compiler errors, repeat until the file typechecks or until the real \
                 source-faithfulness blocker is exposed in Lean code and notes. Then print \
                 exactly one JSON object matching schema.json, with code equal to the final \
                 GrokRxiv/Proofs.lean contents."
                    .to_string()
            } else if uses_review_skill {
                format!(
                    "/{CLAUDE_SKILL_NAME}\n\n\
                     Read the complete GrokRxiv review prompt supplied on stdin. \
                     Follow it exactly and return only the requested schema-valid JSON object."
                )
            } else {
                "Read the complete GrokRxiv prompt supplied on stdin. \
                 Follow it exactly and return only the requested schema-valid JSON object."
                    .to_string()
            };
            let args = vec![
                "-p".to_string(),
                query,
                "--model".to_string(),
                spec.model.clone(),
                "--output-format".to_string(),
                "json".to_string(),
                "--json-schema".to_string(),
                schema_json,
            ];
            let args = if lean_inventory_loop_role(&spec.role) {
                let mut args = args;
                args.extend([
                    "--permission-mode".to_string(),
                    "acceptEdits".to_string(),
                    "--tools".to_string(),
                    "Read,Write,Edit,Bash".to_string(),
                    "--allowedTools".to_string(),
                    "Read,Write,Edit,Bash(lake env lean *)".to_string(),
                ]);
                args
            } else {
                args
            };
            (args, None, true)
        }
        CliProviderBackend::Codex => {
            // codex doesn't read prompts from stdin in `exec`; it takes a
            // positional prompt arg. We still capture it in `stdin_payload`
            // for symmetry with the other branches, but we pass it as the
            // final positional arg. Long prompts: codex handles multi-line
            // strings fine, and we are bounded by the OS argv limit only on
            // truly enormous inputs (>1MB on macOS / >2MB on Linux).
            let path = write_codex_schema(&role_slug, spec.schema.as_ref())?;
            let args = vec![
                "exec".to_string(),
                "--skip-git-repo-check".to_string(),
                "--json".to_string(),
                "--output-schema".to_string(),
                path.to_string_lossy().into_owned(),
                provider_prompt.clone(),
            ];
            (args, Some(path), false)
        }
        CliProviderBackend::Antigravity => {
            // Antigravity (`agy`) print mode emits the model response directly.
            // Keep the JSON contract in the prompt and avoid the deprecated
            // Gemini CLI `-o json` wrapper flag.
            let args = vec![
                "-p".to_string(),
                provider_prompt.clone(),
                "--model".to_string(),
                spec.model.clone(),
            ];
            (args, None, false)
        }
    };

    Ok(BuiltCommand {
        program,
        args,
        stdin_payload,
        pipe_stdin,
        schema_path,
        cwd: None,
    })
}

fn emit_cli_input_contract(spec: &AgentSpec, review_workdir: &PreparedReviewWorkdir) {
    let path = review_workdir.path();
    let prompt_chars = std::fs::read_to_string(path.join("prompt.md"))
        .map(|body| body.chars().count())
        .unwrap_or(0);
    let review_input_bytes = std::fs::metadata(path.join("review_input.json"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let schema_bytes = std::fs::metadata(path.join("schema.json"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    tracing::info!(
        role = %spec.role,
        provider = %spec.provider,
        model = %spec.model,
        workdir = %path.display(),
        prompt_chars,
        review_input_bytes,
        schema_bytes,
        "CLI agent input contract"
    );
    eprintln!(
        "agent_input role={} provider={} model={} prompt_chars={} review_input_bytes={} schema_bytes={} workdir={}",
        spec.role,
        spec.provider,
        spec.model,
        prompt_chars,
        review_input_bytes,
        schema_bytes,
        path.display()
    );
}

fn timeout_secs_value(timeout_dur: Option<Duration>) -> serde_json::Value {
    timeout_dur
        .map(|duration| serde_json::json!(duration.as_secs()))
        .unwrap_or(serde_json::Value::Null)
}

fn timeout_policy(timeout_dur: Option<Duration>) -> &'static str {
    if timeout_dur.is_some() {
        "bounded"
    } else {
        "unbounded"
    }
}

fn timeout_display(timeout_dur: Option<Duration>) -> String {
    timeout_dur
        .map(|duration| format!("{}s", duration.as_secs()))
        .unwrap_or_else(|| "unbounded".to_string())
}

fn record_cli_latency_sample(
    spec: &AgentSpec,
    latency_ms: i32,
    timeout_dur: Option<Duration>,
    review_workdir: &PreparedReviewWorkdir,
    status: CliLatencySampleStatus,
    error: Option<&str>,
) {
    if latency_ms <= 0 {
        return;
    }
    let path = agent_benchmark_path();
    let parent = path.parent().map(Path::to_path_buf);
    let prompt_chars = std::fs::read_to_string(review_workdir.path().join("prompt.md"))
        .map(|body| body.chars().count())
        .unwrap_or(0);
    let schema_bytes = std::fs::metadata(review_workdir.path().join("schema.json"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let review_input_bytes = std::fs::metadata(review_workdir.path().join("review_input.json"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let mut sample = serde_json::json!({
        "schema_version": 1,
        "ts_ms": ts_ms,
        "role": spec.role,
        "provider": spec.provider,
        "model": spec.model,
        "runner": "cli",
        "status": status.as_str(),
        "latency_ms": latency_ms,
        "timeout_secs": timeout_secs_value(timeout_dur),
        "timeout_policy": timeout_policy(timeout_dur),
        "prompt_chars": prompt_chars,
        "schema_bytes": schema_bytes,
        "review_input_bytes": review_input_bytes,
    });
    if let Some(error) = error {
        if let Some(obj) = sample.as_object_mut() {
            obj.insert(
                "error".to_string(),
                serde_json::Value::String(error.chars().take(500).collect()),
            );
        }
    }
    let _guard = agent_benchmark_io_lock();
    let result = (|| -> anyhow::Result<()> {
        if let Some(parent) = parent {
            std::fs::create_dir_all(&parent)
                .map_err(|e| anyhow::anyhow!("create benchmark dir {}: {e}", parent.display()))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| anyhow::anyhow!("open benchmark {}: {e}", path.display()))?;
        writeln!(file, "{}", sample)
            .map_err(|e| anyhow::anyhow!("write benchmark {}: {e}", path.display()))?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            tracing::info!(
                role = %spec.role,
                provider = %spec.provider,
                model = %spec.model,
                status = status.as_str(),
                latency_ms,
                timeout = %timeout_display(timeout_dur),
                prompt_chars,
                benchmark_path = %path.display(),
                "CLI agent latency benchmark recorded"
            );
            eprintln!(
                "agent_benchmark role={} provider={} model={} status={} latency_ms={} timeout={} prompt_chars={} path={}",
                spec.role,
                spec.provider,
                spec.model,
                status.as_str(),
                latency_ms,
                timeout_display(timeout_dur),
                prompt_chars,
                path.display()
            );
        }
        Err(err) => {
            tracing::warn!(
                role = %spec.role,
                provider = %spec.provider,
                model = %spec.model,
                err = %err,
                "failed to record CLI latency benchmark"
            );
        }
    }
}

fn record_cli_latency_error_sample(
    spec: &AgentSpec,
    elapsed: Duration,
    timeout_dur: Option<Duration>,
    review_workdir: &PreparedReviewWorkdir,
    err: &anyhow::Error,
) {
    let latency_ms = elapsed.as_millis().min(i32::MAX as u128) as i32;
    let status = match err
        .chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())
    {
        Some(CliError::TimedOut { .. }) => CliLatencySampleStatus::Timeout,
        _ => CliLatencySampleStatus::Error,
    };
    record_cli_latency_sample(
        spec,
        latency_ms,
        timeout_dur,
        review_workdir,
        status,
        Some(&err.to_string()),
    );
}

fn emit_cli_command_contract(
    spec: &AgentSpec,
    built: &BuiltCommand,
    backend: CliProviderBackend,
    timeout_dur: Option<Duration>,
) {
    let command = safe_cli_command_display(built);
    tracing::info!(
        role = %spec.role,
        provider = %spec.provider,
        model = %spec.model,
        backend = ?backend,
        timeout = %timeout_display(timeout_dur),
        program = %built.program,
        pipe_stdin = built.pipe_stdin,
        stdin_chars = built.stdin_payload.chars().count(),
        command = %command,
        "CLI agent command contract"
    );
    crate::cli_status::emit(format!(
        "agent role={} provider={} model={} backend={backend:?} timeout={} command={command}",
        spec.role,
        spec.provider,
        spec.model,
        timeout_display(timeout_dur)
    ));
    eprintln!(
        "agent_command role={} provider={} model={} backend={backend:?} timeout={} command={command}",
        spec.role,
        spec.provider,
        spec.model,
        timeout_display(timeout_dur)
    );
}

fn safe_cli_command_display(built: &BuiltCommand) -> String {
    let mut parts = vec![built.program.clone()];
    let mut args = built.args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-p" => {
                parts.push(arg.clone());
                let chars = args.next().map(|value| value.chars().count()).unwrap_or(0);
                parts.push(format!("<prompt-query:{chars} chars>"));
            }
            "--json-schema" => {
                parts.push(arg.clone());
                let bytes = args.next().map(|value| value.len()).unwrap_or(0);
                parts.push(format!("<inline-json-schema:{bytes} bytes>"));
            }
            "--output-schema" => {
                parts.push(arg.clone());
                let path = args.next().cloned().unwrap_or_default();
                parts.push(path);
            }
            "--model" => {
                parts.push(arg.clone());
                let model = args.next().cloned().unwrap_or_default();
                parts.push(model);
            }
            _ if arg.chars().count() > 120 => {
                parts.push(format!("<arg:{} chars>", arg.chars().count()));
            }
            _ => parts.push(arg.clone()),
        }
    }
    if built.pipe_stdin {
        parts.push(format!(
            "<stdin:{} chars>",
            built.stdin_payload.chars().count()
        ));
    }
    parts.join(" ")
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
    timeout_dur: Option<Duration>,
    role: &str,
    provider: &str,
) -> anyhow::Result<String> {
    write_cli_command_audit(built, timeout_dur, role, provider);
    let mut cmd = Command::new(&built.program);
    cmd.args(&built.args);
    if let Some(cwd) = &built.cwd {
        cmd.current_dir(cwd);
    }
    cmd.kill_on_drop(true);
    configure_process_group(&mut cmd);
    scrub_provider_api_env(&mut cmd);
    tracing::info!(
        provider = %provider,
        program = %built.program,
        api_env_scrubbed = true,
        "CLI subprocess API env scrubbed"
    );

    // Only commands with `pipe_stdin` read stdin in our wiring. Others get
    // null stdin so children cannot block on an inherited terminal.
    if built.pipe_stdin {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `{}`: {e}", built.program))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture stdout for `{}`", built.program))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture stderr for `{}`", built.program))?;
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await.map(|_| buf)
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await.map(|_| buf)
    });

    if built.pipe_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(built.stdin_payload.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("failed to write prompt to stdin: {e}"))?;
            // Drop closes stdin so the child sees EOF and proceeds.
            drop(stdin);
        }
    }

    let status = if let Some(timeout_dur) = timeout_dur {
        match timeout(timeout_dur, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => anyhow::bail!("waiting on `{}` failed: {e}", built.program),
            Err(_) => {
                // Timed out. Kill the whole process group first (reaps CLI sub-children), then the
                // direct child, then VERIFY death by reaping it — if `wait()` doesn't return after
                // the kill the process is wedged un-killably, which we surface in the error.
                kill_process_group(&child);
                let _ = timeout(Duration::from_secs(5), child.kill()).await;
                stdout_task.abort();
                stderr_task.abort();
                let reaped = matches!(
                    timeout(Duration::from_secs(5), child.wait()).await,
                    Ok(Ok(_))
                );
                if !reaped {
                    tracing::warn!(
                        role,
                        pid = child.id(),
                        "CliRunner subprocess did not exit after SIGKILL; possible orphaned process tree"
                    );
                }
                write_cli_timeout_audit(built, timeout_dur, role, provider, reaped);
                return Err(anyhow::Error::new(CliError::TimedOut {
                    provider: provider.to_string(),
                    role: role.to_string(),
                    timeout_secs: timeout_dur.as_secs(),
                    subprocess_status: if reaped {
                        "killed".to_string()
                    } else {
                        "kill_unconfirmed".to_string()
                    },
                }));
            }
        }
    } else {
        child
            .wait()
            .await
            .map_err(|e| anyhow::anyhow!("waiting on `{}` failed: {e}", built.program))?
    };
    let stdout = stdout_task
        .await
        .map_err(|e| anyhow::anyhow!("stdout task failed for `{}`: {e}", built.program))?
        .map_err(|e| anyhow::anyhow!("read stdout for `{}` failed: {e}", built.program))?;
    let stderr = stderr_task
        .await
        .map_err(|e| anyhow::anyhow!("stderr task failed for `{}`: {e}", built.program))?
        .map_err(|e| anyhow::anyhow!("read stderr for `{}` failed: {e}", built.program))?;

    if !status.success() {
        let stdout = String::from_utf8_lossy(&stdout).to_string();
        let stderr = String::from_utf8_lossy(&stderr).to_string();
        write_cli_raw_io_audit(built, &stdout, &stderr);
        // FP-RPT3b B5: classify as a structured quota error when stderr/stdout
        // matches a known signature. The caller can then fall back to a
        // different runner instead of treating it as a generic subprocess
        // failure.
        let combined_error_log = combine_subprocess_error_log(&stderr, &stdout, built);
        if let Some(snippet) = detect_quota_signal(&combined_error_log) {
            return Err(anyhow::Error::new(CliError::QuotaExhausted {
                provider: provider.to_string(),
                message: snippet,
            })
            .context(format!(
                "`{}` exited with {:?} for role {}",
                built.program,
                status.code(),
                role,
            )));
        }
        let detail = subprocess_failure_detail(&stderr, &stdout, built);
        anyhow::bail!(
            "`{}` exited with {:?} for role {}: {detail}",
            built.program,
            status.code(),
            role,
        );
    }

    let stdout = String::from_utf8_lossy(&stdout).to_string();
    write_cli_raw_io_audit(built, &stdout, "");
    if stdout.trim().is_empty() {
        let cli_log = cli_log_from_args(built).unwrap_or_default();
        if let Some(snippet) = detect_quota_signal(&cli_log) {
            return Err(anyhow::Error::new(CliError::QuotaExhausted {
                provider: provider.to_string(),
                message: snippet,
            })
            .context(format!(
                "`{}` exited successfully with empty stdout for role {} but its log contains a quota signal",
                built.program, role,
            )));
        }
        if provider == "gemini" {
            let log_hint = if cli_log.trim().is_empty() {
                String::new()
            } else {
                let snippet: String = cli_log.chars().take(200).collect();
                format!("; log={snippet:?}")
            };
            anyhow::bail!(
                "`{}` exited successfully with empty stdout for role {}{}",
                built.program,
                role,
                log_hint
            );
        }
    }
    Ok(stdout)
}

fn write_cli_command_audit(
    built: &BuiltCommand,
    timeout_dur: Option<Duration>,
    role: &str,
    provider: &str,
) {
    let Some(cwd) = built.cwd.as_ref() else {
        return;
    };
    let payload = serde_json::json!({
        "role": role,
        "provider": provider,
        "program": &built.program,
        "args": &built.args,
        "timeout_secs": timeout_secs_value(timeout_dur),
        "timeout_policy": timeout_policy(timeout_dur),
        "pipe_stdin": built.pipe_stdin,
        "cwd": cwd.display().to_string(),
    });
    let _ = std::fs::write(
        cwd.join("command.json"),
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

fn write_cli_raw_io_audit(built: &BuiltCommand, stdout: &str, stderr: &str) {
    let Some(cwd) = built.cwd.as_ref() else {
        return;
    };
    let _ = std::fs::write(cwd.join("raw_stdout.txt"), stdout);
    let _ = std::fs::write(cwd.join("raw_stderr.txt"), stderr);
}

fn write_cli_timeout_audit(
    built: &BuiltCommand,
    timeout_dur: Duration,
    role: &str,
    provider: &str,
    reaped: bool,
) {
    let Some(cwd) = built.cwd.as_ref() else {
        return;
    };
    let payload = serde_json::json!({
        "status": "runner_timeout",
        "role": role,
        "provider": provider,
        "program": &built.program,
        "args": &built.args,
        "timeout_secs": timeout_dur.as_secs(),
        "subprocess_status": if reaped { "killed" } else { "kill_unconfirmed" },
    });
    let _ = std::fs::write(
        cwd.join("timeout_result.json"),
        serde_json::to_vec_pretty(&payload).unwrap_or_default(),
    );
}

fn combine_subprocess_error_log(stderr: &str, stdout: &str, built: &BuiltCommand) -> String {
    let cli_log = cli_log_from_args(built).unwrap_or_default();
    [stderr, stdout, cli_log.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn subprocess_failure_detail(stderr: &str, stdout: &str, built: &BuiltCommand) -> String {
    let cli_log = cli_log_from_args(built).unwrap_or_default();
    for (label, body) in [
        ("stderr", stderr),
        ("stdout", stdout),
        ("log", cli_log.as_str()),
    ] {
        let trimmed = body.trim();
        if !trimmed.is_empty() {
            let snippet: String = trimmed.chars().take(600).collect();
            return format!("{label}={snippet}");
        }
    }
    "stderr/stdout empty".to_string()
}

fn cli_log_from_args(built: &BuiltCommand) -> Option<String> {
    let path = arg_value(&built.args, "--log-file")?;
    std::fs::read_to_string(path).ok()
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

#[cfg(unix)]
fn configure_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(child: &tokio::process::Child) {
    let Some(pid) = child.id() else {
        return;
    };
    let target = format!("-{pid}");
    // Group cleanup. The child is spawned with `process_group(0)` so `-KILL -<pid>` reaps the
    // whole tree (the CLI's own sub-children — e.g. codex/claude helpers — that the direct
    // `child.kill()` alone would orphan). Log failures instead of silently swallowing them so
    // a survived process tree is visible in the hang diagnostics.
    match std::process::Command::new("kill")
        .args(["-KILL", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => {
            tracing::warn!(pid, %status, "process-group SIGKILL returned non-success; sub-children may have escaped the group");
        }
        Err(err) => {
            tracing::warn!(pid, err = %err, "failed to SIGKILL process group; sub-children may be orphaned");
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_child: &tokio::process::Child) {}

fn direct_provider_api_allowed() -> bool {
    matches!(
        std::env::var(ALLOW_PROVIDER_API_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn extractor_api_selected() -> bool {
    matches!(std::env::var("AGENTHERO_EXTRACTOR").as_deref(), Ok("api"))
}

fn scrub_provider_api_env(cmd: &mut Command) {
    for key in PROVIDER_API_ENV_VARS_TO_SCRUB {
        cmd.env_remove(key);
    }
    cmd.env("GROKRXIV_CLI_API_ENV_SCRUBBED", "1");
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
            if let Some(structured_output) = wrapper.get("structured_output") {
                return structured_output.to_string();
            }
            match wrapper.get("result") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => trimmed.to_string(),
            }
        }
        "gemini" => {
            // A7: `gemini -o json` returns
            // {"session_id": "...", "response": "<inner>", "stats": {...}}.
            // Unwrap the `response` field; fall back to the raw body if the
            // shape isn't what we expect (e.g. gemini emitted an error blob
            // we'd rather surface verbatim than swallow).
            let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                return trimmed.to_string();
            };
            match wrapper.get("response") {
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
    let parsed: serde_json::Value = match serde_json::from_str(cleaned) {
        Ok(parsed) => parsed,
        Err(first_err) => {
            let mut candidate_errors: Vec<String> = Vec::new();
            for candidate in json_object_candidates(cleaned).into_iter().rev() {
                match serde_json::from_str::<serde_json::Value>(&candidate) {
                    Ok(parsed) => match validate_parsed(parsed, schema) {
                        Ok(validated) => return Ok(validated),
                        Err(err) => candidate_errors.push(err.to_string()),
                    },
                    Err(err) => candidate_errors.push(err.to_string()),
                }
            }
            let raw_excerpt = diagnostic_excerpt(extracted, 1_200);
            if candidate_errors.is_empty() {
                return Err(anyhow::anyhow!(
                    "not valid JSON: {first_err}; raw_excerpt={raw_excerpt:?}"
                ));
            }
            return Err(anyhow::anyhow!(
                "not valid JSON: {first_err}; candidate errors={}; raw_excerpt={raw_excerpt:?}",
                candidate_errors.join(" | ")
            ));
        }
    };

    validate_parsed(parsed, schema)
}

fn json_object_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start_idx) = start.take() {
                        out.push(text[start_idx..=idx].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn validate_parsed(
    parsed: serde_json::Value,
    schema: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    // Empty schema {} = no constraint. Skip validation in that case so unit
    // tests with stub specs keep working.
    if schema.is_null()
        || (schema.is_object() && schema.as_object().map(|m| m.is_empty()).unwrap_or(false))
    {
        return Ok(parsed);
    }

    let validator = jsonschema::validator_for(schema)
        .map_err(|e| anyhow::anyhow!("invalid role schema: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(&parsed)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        anyhow::bail!("schema validation failed: {}", errors.join("; "));
    }
    Ok(parsed)
}

fn diagnostic_excerpt(raw: &str, max_chars: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut excerpt = trimmed.chars().take(max_chars).collect::<String>();
    excerpt.push_str("...");
    excerpt
}

/// Normalize a manifest role id to a filesystem-safe schema filename stem.
fn role_slug(role: &str) -> String {
    role.trim()
        .replace('.', "_")
        .replace('-', "_")
        .to_ascii_lowercase()
}

fn claude_json_schema(schema: &serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(map) => {
            let union_types = map
                .get("type")
                .and_then(|value| value.as_array())
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| item.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                })
                .filter(|items| items.len() > 1);

            let mut rewritten = serde_json::Map::new();
            for (key, value) in map {
                if union_types.is_some() && key == "type" {
                    continue;
                }
                rewritten.insert(key.clone(), claude_json_schema(value));
            }

            if let Some(types) = union_types {
                serde_json::json!({
                    "anyOf": types
                        .into_iter()
                        .map(|type_name| {
                            let mut branch = rewritten.clone();
                            branch.insert("type".to_string(), serde_json::json!(type_name));
                            serde_json::Value::Object(branch)
                        })
                        .collect::<Vec<_>>()
                })
            } else {
                serde_json::Value::Object(rewritten)
            }
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(claude_json_schema).collect())
        }
        _ => schema.clone(),
    }
}

fn grokrxiv_review_skill_role(role_slug: &str) -> bool {
    matches!(
        role_slug,
        "summary"
            | "technical_correctness"
            | "novelty"
            | "reproducibility"
            | "citation"
            | "meta_reviewer"
    )
}

/// Persist the role's JSON schema to `$TMPDIR/grokrxiv-schemas/<role>.schema.json`
/// for codex's `--output-schema` flag. The directory is created if needed.
fn write_codex_schema(role_slug: &str, schema: &serde_json::Value) -> anyhow::Result<PathBuf> {
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
fn log_auth_path_once(_provider: &str, backend: CliProviderBackend) {
    match backend {
        CliProviderBackend::Claude => {
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
        CliProviderBackend::Codex => {
            if CODEX_AUTH_LOGGED.set(()).is_ok() {
                let (auth_method, plan_type) = inspect_codex_auth();
                tracing::info!(
                    event = "cli_auth_path",
                    provider = "openai",
                    auth_method = %auth_method,
                    plan_type = %plan_type,
                    "codex CLI auth path"
                );
                tracing::info!(
                    provider = "openai",
                    auth_method = %auth_method,
                    "codex CLI uses local CLI auth; provider API key env is scrubbed"
                );
            }
        }
        CliProviderBackend::Antigravity => {
            if ANTIGRAVITY_AUTH_LOGGED.set(()).is_ok() {
                let (auth_method, state) = inspect_antigravity_auth();
                tracing::info!(
                    event = "cli_auth_path",
                    provider = "gemini",
                    auth_method = %auth_method,
                    state = %state,
                    "Antigravity CLI auth path"
                );
                tracing::info!(
                    provider = "gemini",
                    auth_method = %auth_method,
                    "Antigravity CLI uses local CLI/keyring auth; provider API key env is scrubbed"
                );
            }
        }
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
    let oauth = val
        .get("oauthAccount")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
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
    let plan_type =
        decode_jwt_claim(id_token, "chatgpt_plan_type").unwrap_or_else(|| "unknown".into());
    let auth_method = if plan_type != "unknown" {
        "chatgpt_subscription"
    } else if id_token.is_empty() {
        "unknown"
    } else {
        "oauth"
    };
    (auth_method.into(), plan_type)
}

/// Best-effort check for Antigravity CLI state. Antigravity migrates session
/// tokens into native keyring storage, so this reports only local non-secret
/// state markers and never tries to read tokens.
fn inspect_antigravity_auth() -> (String, String) {
    let Ok(home) = std::env::var("HOME") else {
        return ("unknown".into(), "unknown".into());
    };
    let home = PathBuf::from(home);
    let markers = [
        home.join(".gemini").join("antigravity"),
        home.join(".gemini").join("antigravity-cli"),
        home.join("Library")
            .join("Application Support")
            .join("Antigravity"),
    ];
    if let Some(marker) = markers.iter().find(|path| path.exists()) {
        return (
            "antigravity_keyring".into(),
            format!("present ({})", marker.display()),
        );
    }
    ("unknown".into(), "unknown".into())
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
        let n =
            (buf[0] as u32) << 18 | (buf[1] as u32) << 12 | (buf[2] as u32) << 6 | (buf[3] as u32);
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
    use std::sync::MutexGuard;

    fn stub_spec(provider: &str, model: &str) -> AgentSpec {
        let mut s = AgentSpec::api_default("summary", provider.to_string(), model.to_string());
        s.runner = AgentRunnerKind::Cli;
        s.schema = std::sync::Arc::new(serde_json::json!({}));
        s
    }

    struct EnvVarGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvVarGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let lock = crate::test_env_lock();
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { _lock: lock, saved }
        }

        fn set_owned(vars: &[(&'static str, String)]) -> Self {
            let lock = crate::test_env_lock();
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { _lock: lock, saved }
        }

        fn clear(vars: &[&'static str]) -> Self {
            let lock = crate::test_env_lock();
            let saved = vars
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in vars {
                std::env::remove_var(key);
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn test_provider_to_binary_mapping_claude_openai_google_cli_backends() {
        // Clear env vars so we exercise the default-name branch.
        let _env = EnvVarGuard::clear(&[
            "AGENTHERO_CLAUDE_BIN",
            "AGENTHERO_CODEX_BIN",
            "AGENTHERO_ANTIGRAVITY_BIN",
            "AGENTHERO_AGY_BIN",
        ]);

        let r = CliRunner::new();
        assert_eq!(r.binary_for("claude").unwrap(), "claude");
        assert_eq!(r.binary_for("openai").unwrap(), "codex");
        assert_eq!(r.binary_for("gemini").unwrap(), "agy");

        // Now exercise the env override path.
        std::env::set_var("AGENTHERO_CLAUDE_BIN", "/opt/bin/claude-test");
        assert_eq!(r.binary_for("claude").unwrap(), "/opt/bin/claude-test");
        std::env::remove_var("AGENTHERO_CLAUDE_BIN");

        std::env::set_var("AGENTHERO_ANTIGRAVITY_BIN", "/opt/bin/agy");
        assert_eq!(r.binary_for("gemini").unwrap(), "/opt/bin/agy");
        std::env::remove_var("AGENTHERO_ANTIGRAVITY_BIN");
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
        assert!(
            msg.contains("foo"),
            "error should name the bad provider: {msg}"
        );
    }

    /// Phase: spec.timeout_secs plumbing. Run as a single test to keep env-var
    /// state changes serial (parallel test threads share process env).
    #[test]
    fn cli_timeout_for_resolution_order() {
        // 1. per-role env var wins over everything.
        {
            let _guard = EnvVarGuard::set(&[
                ("GROKRXIV_CUSTOM_VALIDATOR_TIMEOUT_SECS", "77"),
                ("AGENTHERO_CLI_TIMEOUT_SECS", "42"),
            ]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.role = "custom_validator".to_string();
            spec.timeout_secs = 999;
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(77)));
        }
        // 2. invalid/blank per-role env var is ignored; with no YAML timeout the global applies.
        {
            let _guard = EnvVarGuard::set(&[
                ("GROKRXIV_CUSTOM_VALIDATOR_TIMEOUT_SECS", ""),
                ("AGENTHERO_CLI_TIMEOUT_SECS", "42"),
            ]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.role = "custom_validator".to_string();
            spec.timeout_secs = 0;
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(42)));
        }
        // 3. Explicit YAML beats the global default in both directions. A generic
        //    `AGENTHERO_CLI_TIMEOUT_SECS` must not silently inflate short typed-IR/citation
        //    roles or cap deliberately-long roles.
        {
            // long YAML beats a small global (the theorem-extraction bug).
            let _guard = EnvVarGuard::set(&[("AGENTHERO_CLI_TIMEOUT_SECS", "360")]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 2400;
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(2400)));
        }
        {
            // short YAML still beats a large global default (the typed-IR/citation leak).
            let _guard = EnvVarGuard::set(&[("AGENTHERO_CLI_TIMEOUT_SECS", "999")]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 42;
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(42)));
        }
        // 4. no env var → spec wins over default.
        {
            let _guard = EnvVarGuard::clear(&[
                "AGENTHERO_CLI_TIMEOUT_SECS",
                "GROKRXIV_SUMMARY_TIMEOUT_SECS",
            ]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 120;
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(120)));
        }
        // 5. no env var, spec=0 → falls back to default.
        {
            let _guard = EnvVarGuard::clear(&[
                "AGENTHERO_CLI_TIMEOUT_SECS",
                "GROKRXIV_SUMMARY_TIMEOUT_SECS",
            ]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 0;
            assert_eq!(
                cli_timeout_for(&spec),
                Some(Duration::from_secs(DEFAULT_CLI_TIMEOUT_SECS))
            );
        }
    }

    #[test]
    fn lean_claude_roles_are_unbounded_by_default_but_can_be_bounded_by_env() {
        let mut spec = stub_spec("claude", "opus[1m]");
        spec.role = "lean_inventory_author".to_string();
        spec.timeout_secs = 600;
        {
            let _guard = EnvVarGuard::clear(&[
                "GROKRXIV_LEAN_INVENTORY_AUTHOR_TIMEOUT_SECS",
                LEAN_CLAUDE_TIMEOUT_ENV,
                "AGENTHERO_CLI_TIMEOUT_SECS",
            ]);
            assert_eq!(cli_timeout_for(&spec), None);
        }

        {
            let _guard = EnvVarGuard::set_owned(&[(LEAN_CLAUDE_TIMEOUT_ENV, "2400".to_string())]);
            assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(2400)));
        }
    }

    #[test]
    fn citation_sonnet_timeout_uses_recent_benchmark_average() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        for latency_ms in [700_000u64, 720_000, 740_000] {
            let sample = serde_json::json!({
                "schema_version": 1,
                "role": "citation",
                "provider": "claude",
                "model": "sonnet[1m]",
                "runner": "cli",
                "status": "success",
                "latency_ms": latency_ms
            });
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| writeln!(file, "{}", sample))
                .expect("write benchmark");
        }
        let _guard = EnvVarGuard::set_owned(&[
            (
                AGENT_BENCHMARK_PATH_ENV,
                path.to_string_lossy().into_owned(),
            ),
            ("GROKRXIV_CITATION_TIMEOUT_SECS", "".to_string()),
            ("AGENTHERO_CLI_TIMEOUT_SECS", "".to_string()),
            (CITATION_TIMEOUT_MAX_ENV, "1800".to_string()),
        ]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "citation".to_string();
        spec.timeout_secs = 900;

        assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(1_500)));
    }

    #[test]
    fn citation_sonnet_timeout_benchmark_does_not_shrink_configured_floor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        let sample = serde_json::json!({
            "schema_version": 1,
            "role": "citation",
            "provider": "claude",
            "model": "sonnet[1m]",
            "runner": "cli",
            "status": "success",
            "latency_ms": 100_000
        });
        std::fs::write(&path, format!("{sample}\n")).expect("write benchmark");
        let _guard = EnvVarGuard::set_owned(&[
            (
                AGENT_BENCHMARK_PATH_ENV,
                path.to_string_lossy().into_owned(),
            ),
            ("GROKRXIV_CITATION_TIMEOUT_SECS", "".to_string()),
            ("AGENTHERO_CLI_TIMEOUT_SECS", "".to_string()),
            (CITATION_TIMEOUT_MAX_ENV, "1800".to_string()),
        ]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "citation".to_string();
        spec.timeout_secs = 900;

        assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(900)));
    }

    #[test]
    fn formalize_typed_ir_sonnet_timeout_uses_recent_benchmark_average() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        for latency_ms in [210_000u64, 240_000, 270_000] {
            let sample = serde_json::json!({
                "schema_version": 1,
                "role": "formalize_source_inventory_typed_transcriber",
                "provider": "claude",
                "model": "sonnet[1m]",
                "runner": "cli",
                "status": "success",
                "latency_ms": latency_ms
            });
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| writeln!(file, "{}", sample))
                .expect("write benchmark");
        }
        let _guard = EnvVarGuard::set_owned(&[
            (
                AGENT_BENCHMARK_PATH_ENV,
                path.to_string_lossy().into_owned(),
            ),
            ("GROKRXIV_FORMALIZE_TYPED_IR_TIMEOUT_SECS", "".to_string()),
            ("AGENTHERO_CLI_TIMEOUT_SECS", "".to_string()),
            (FORMALIZE_TYPED_IR_TIMEOUT_MAX_ENV, "1800".to_string()),
        ]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        spec.timeout_secs = 300;

        assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(540)));
    }

    #[test]
    fn formalize_typed_ir_timeout_ignores_single_slow_sample() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        let sample = serde_json::json!({
            "schema_version": 1,
            "role": "formalize_source_inventory_typed_transcriber",
            "provider": "claude",
            "model": "sonnet[1m]",
            "runner": "cli",
            "status": "success",
            "latency_ms": 672_933
        });
        std::fs::write(&path, format!("{sample}\n")).expect("write benchmark");
        let _guard = EnvVarGuard::set_owned(&[
            (
                AGENT_BENCHMARK_PATH_ENV,
                path.to_string_lossy().into_owned(),
            ),
            ("GROKRXIV_FORMALIZE_TYPED_IR_TIMEOUT_SECS", "".to_string()),
            ("AGENTHERO_CLI_TIMEOUT_SECS", "".to_string()),
            (FORMALIZE_TYPED_IR_TIMEOUT_MAX_ENV, "1800".to_string()),
        ]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        spec.timeout_secs = 300;

        assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(300)));
    }

    #[test]
    fn timeout_benchmark_samples_are_audited_but_not_used_for_adaptive_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        let workdir = dir.path().join("review-workdir");
        std::fs::create_dir_all(&workdir).expect("create workdir");
        std::fs::write(workdir.join("prompt.md"), "typed IR prompt").expect("write prompt");
        std::fs::write(workdir.join("schema.json"), "{}").expect("write schema");
        std::fs::write(workdir.join("review_input.json"), "{\"paper\":true}")
            .expect("write review input");
        let prepared = PreparedReviewWorkdir {
            path: workdir,
            _tempdir: None,
        };
        let _guard = EnvVarGuard::set_owned(&[(
            AGENT_BENCHMARK_PATH_ENV,
            path.to_string_lossy().into_owned(),
        )]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        record_cli_latency_sample(
            &spec,
            300_064,
            Some(Duration::from_secs(300)),
            &prepared,
            CliLatencySampleStatus::Timeout,
            Some("CliRunner timed out after 300s for role formalize_source_inventory_typed_transcriber (subprocess killed)"),
        );

        let body = std::fs::read_to_string(&path).expect("read benchmark");
        let sample: serde_json::Value = serde_json::from_str(body.trim()).expect("json sample");
        assert_eq!(sample["status"], "timeout");
        assert_eq!(sample["latency_ms"], 300_064);
        assert_eq!(sample["timeout_secs"], 300);
        assert_eq!(sample["prompt_chars"], "typed IR prompt".chars().count());
        assert!(sample["error"]
            .as_str()
            .expect("error text")
            .contains("timed out after 300s"));
        assert!(
            read_success_latency_samples(&spec).is_empty(),
            "timeout samples are audit evidence, not adaptive timeout inputs"
        );
    }

    #[test]
    fn lean_opus_author_timeout_uses_recent_benchmark_average_above_role_floor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent_latencies.jsonl");
        for latency_ms in [450_000u64, 480_000, 510_000] {
            let sample = serde_json::json!({
                "schema_version": 1,
                "role": "lean_proof_author",
                "provider": "claude",
                "model": "opus[1m]",
                "runner": "cli",
                "status": "success",
                "latency_ms": latency_ms
            });
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut file| writeln!(file, "{}", sample))
                .expect("write benchmark");
        }
        let _guard = EnvVarGuard::set_owned(&[
            (
                AGENT_BENCHMARK_PATH_ENV,
                path.to_string_lossy().into_owned(),
            ),
            ("GROKRXIV_LEAN_PROOF_AUTHOR_TIMEOUT_SECS", "600".to_string()),
            (
                "GROKRXIV_LEAN_PROOF_AUTHOR_TIMEOUT_MAX_SECS",
                "1800".to_string(),
            ),
            ("AGENTHERO_CLI_TIMEOUT_SECS", "1800".to_string()),
        ]);
        let mut spec = stub_spec("claude", "opus[1m]");
        spec.role = "lean_proof_author".to_string();
        spec.timeout_secs = 600;

        assert_eq!(cli_timeout_for(&spec), Some(Duration::from_secs(1_020)));
    }

    #[test]
    fn extraction_tool_command_runs_inside_workdir() {
        let r = CliRunner::new();
        let spec = stub_spec("gemini", "gemini-test");
        let workdir = std::env::temp_dir().join("grokrxiv-cli-tool-cwd-test");

        let built = build_tool_command(&r, &spec, "prompt", &workdir).expect("build command");

        assert_eq!(built.cwd.as_deref(), Some(workdir.as_path()));
        assert!(
            !built
                .args
                .windows(2)
                .any(|w| w[0] == "--approval-mode" && w[1] == "plan"),
            "gemini extraction tool loop must not use plan mode; it must return tool-call JSON"
        );
    }

    #[test]
    fn review_workdir_materializes_explicit_input_files() {
        let spec = stub_spec("gemini", "gemini-test");
        let input = AgentInput {
            context: Default::default(),
            role: "summary".to_string(),
            content_hash_material: serde_json::json!({"ignored": true}),
            artifact: serde_json::json!({"title": "Paper", "sections": []}),
            system_prompt: "system instructions".to_string(),
            user_prompt: "review this paper".to_string(),
            source_bundle_path: None,
        };

        let dir = prepare_review_workdir(&spec, &input).expect("prepare workdir");
        let root = dir.path();

        assert!(root.join("review_input.json").exists());
        assert!(root.join("prompt.md").exists());
        assert!(root.join("system.md").exists());
        assert!(root.join("schema.json").exists());
        assert!(root.join("README.md").exists());
        let prompt = std::fs::read_to_string(root.join("prompt.md")).unwrap();
        assert_eq!(prompt, "review this paper");

        let rendered = render_review_prompt_with_files(&input);
        assert!(rendered.contains("review_input.json"));
        assert!(
            !rendered.contains("review this paper"),
            "review prompt should reference prompt.md instead of duplicating the role task"
        );
        assert!(rendered.contains("Do not search parent directories"));
    }

    #[test]
    fn review_workdir_uses_source_bundle_path_as_persistent_harness() {
        let spec = stub_spec("gemini", "gemini-test");
        let harness = tempfile::Builder::new()
            .prefix("grokrxiv-review-harness-")
            .tempdir()
            .expect("harness tempdir");
        let harness_path = harness.path().to_path_buf();
        let input = AgentInput {
            context: Default::default(),
            role: "haskell_semantic_author".to_string(),
            content_hash_material: serde_json::json!({"ignored": true}),
            artifact: serde_json::json!({"target": "haskell"}),
            system_prompt: "system instructions".to_string(),
            user_prompt: "write code".to_string(),
            source_bundle_path: Some(harness_path.display().to_string()),
        };

        let dir = prepare_review_workdir(&spec, &input).expect("prepare harness workdir");
        assert_eq!(dir.path(), harness_path.as_path());
        assert!(harness_path.join("review_input.json").exists());
        assert!(harness_path.join("prompt.md").exists());

        drop(dir);
        assert!(
            harness_path.join("review_input.json").exists(),
            "source bundle harness must not be deleted when the run completes"
        );
    }

    #[test]
    fn claude_schema_rewrites_union_type_arrays_to_anyof() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": ["string", "null"],
                    "enum": [null, "no_theorems_in_paper"]
                },
                "value": {
                    "type": ["number", "integer", "string"]
                },
                "nested": {
                    "items": {
                        "type": ["string", "null"]
                    }
                }
            }
        });

        let rewritten = claude_json_schema(&schema);
        let body = serde_json::to_string(&rewritten).unwrap();

        assert!(
            !body.contains("\"type\":["),
            "Claude CLI rejects union type arrays in strict schema mode: {body}"
        );
        assert_eq!(rewritten["type"], "object");
        assert!(rewritten["properties"]["reason"]["anyOf"].is_array());
        assert_eq!(
            rewritten["properties"]["value"]["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            rewritten["properties"]["nested"]["items"]["anyOf"][1]["type"],
            "null"
        );
    }

    #[test]
    fn test_command_construction_claude() {
        let r = CliRunner::new();
        let spec = stub_spec("claude", "opus[1m]");
        let built = build_command(&r, &spec, "hello prompt").unwrap();

        // Binary
        assert!(
            built.program.ends_with("claude"),
            "program should be claude binary, got {}",
            built.program
        );

        // Args: -p <query> --model <m> --output-format json --json-schema <schema>
        // (Skill is invoked via the `/grokrxiv-review` query prefix, and the
        // large prompt is piped to stdin. Claude CLI has no `--skill` flag.)
        let args = &built.args;
        assert!(args.contains(&"-p".to_string()), "missing -p in {args:?}");
        let prompt_idx = args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in claude args");
        let prompt_value = args
            .get(prompt_idx + 1)
            .expect("missing -p value in claude args");
        assert!(
            prompt_value.starts_with("/grokrxiv-review"),
            "claude query should invoke the grokrxiv skill, got {prompt_value:?}"
        );
        assert_ne!(
            prompt_value, "-",
            "claude invocation should use documented piped-content shape, not `-p -`"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "opus[1m]"),
            "missing --model <model> pair in {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--output-format" && w[1] == "json"),
            "missing --output-format json pair in {args:?}"
        );
        assert!(
            args.windows(2).any(|w| w[0] == "--json-schema"),
            "missing --json-schema pair in {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--skill"),
            "claude CLI does not accept --skill; it must be absent ({args:?})"
        );

        assert!(
            built.pipe_stdin,
            "claude should receive large prompts on stdin"
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
    fn citation_claude_command_uses_sonnet_review_skill_and_piped_prompt() {
        let r = CliRunner::new();
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "citation".to_string();
        let built = build_command(&r, &spec, "citation prompt").unwrap();

        let prompt_idx = built
            .args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in claude args");
        let prompt_value = built
            .args
            .get(prompt_idx + 1)
            .expect("missing -p value in claude args");
        assert!(
            prompt_value.starts_with("/grokrxiv-review"),
            "citation is a review role and should invoke the review skill"
        );
        assert!(
            built
                .args
                .windows(2)
                .any(|w| w[0] == "--model" && w[1] == "sonnet[1m]"),
            "citation command must use Sonnet by default: {:?}",
            built.args
        );
        assert_eq!(built.stdin_payload, "citation prompt");

        let display = safe_cli_command_display(&built);
        assert!(display.contains("sonnet[1m]"));
        assert!(!display.contains("citation prompt"));
    }

    #[test]
    fn test_command_construction_codex() {
        let _env = EnvVarGuard::clear(&["AGENTHERO_CODEX_BIN"]);
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
        assert!(
            args.contains(&"--skip-git-repo-check".to_string()),
            "missing --skip-git-repo-check in {args:?}"
        );
        assert!(
            args.contains(&"--json".to_string()),
            "missing --json in {args:?}"
        );
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
        assert!(
            path.exists(),
            "schema file not written at {}",
            path.display()
        );

        // Clean up.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_command_construction_gemini() {
        let r = CliRunner::new();
        let _env = EnvVarGuard::clear(&["AGENTHERO_ANTIGRAVITY_BIN", "AGENTHERO_AGY_BIN"]);
        let spec = stub_spec("gemini", "gemini-3-flash-preview");
        let built = build_command(&r, &spec, "the prompt body").unwrap();

        assert!(
            built.program.ends_with("agy"),
            "program should be Antigravity agy binary, got {}",
            built.program
        );

        let args = &built.args;

        let prompt_idx = args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in gemini args");
        let prompt_value = args
            .get(prompt_idx + 1)
            .expect("missing -p value in gemini args");
        assert!(
            prompt_value == "/grokrxiv-review\n\nthe prompt body",
            "expected skill-prefixed prompt for gemini, got {prompt_value:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--model" && w[1] == "gemini-3-flash-preview"),
            "gemini args should include --model gemini-3-flash-preview: {args:?}"
        );
        assert!(
            !args.windows(2).any(|w| w[0] == "-o" && w[1] == "json"),
            "agy prints the model response directly and should not request the legacy Gemini JSON wrapper: {args:?}"
        );

        assert!(
            built.stdin_payload == "/grokrxiv-review\n\nthe prompt body",
            "stdin_payload should mirror the gemini prompt, got {:?}",
            built.stdin_payload
        );

        assert!(built.schema_path.is_none());
    }

    #[test]
    fn antigravity_formalize_typed_ir_avoids_review_skill() {
        let r = CliRunner::new();
        let _env = EnvVarGuard::clear(&["AGENTHERO_ANTIGRAVITY_BIN", "AGENTHERO_AGY_BIN"]);
        let mut spec = stub_spec("gemini", "Gemini 3.5 Flash (Medium)");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        let built = build_command(&r, &spec, "the prompt body").unwrap();

        let prompt_idx = built
            .args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in gemini args");
        let prompt_value = built
            .args
            .get(prompt_idx + 1)
            .expect("missing -p value in gemini args");
        assert_eq!(
            prompt_value, "the prompt body",
            "formalize typed-IR must not route through the review-only skill"
        );
        assert_eq!(built.stdin_payload, "the prompt body");
    }

    #[test]
    fn claude_formalize_typed_ir_avoids_review_skill() {
        let r = CliRunner::new();
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        let built = build_command(&r, &spec, "typed ir prompt").unwrap();

        let prompt_idx = built
            .args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in claude args");
        let prompt_value = built
            .args
            .get(prompt_idx + 1)
            .expect("missing -p value in claude args");
        assert!(
            !prompt_value.starts_with("/grokrxiv-review"),
            "formalize typed-IR must not route through the review-only skill"
        );
        assert!(
            built
                .args
                .windows(2)
                .any(|w| w[0] == "--model" && w[1] == "sonnet[1m]"),
            "typed-IR command must use configured Sonnet model: {:?}",
            built.args
        );
        assert_eq!(built.stdin_payload, "typed ir prompt");
    }

    #[test]
    fn test_command_construction_antigravity_override() {
        let _env = EnvVarGuard::set(&[("AGENTHERO_ANTIGRAVITY_BIN", "/opt/bin/agy")]);
        let r = CliRunner::new();
        let spec = stub_spec("gemini", "gemini-3-flash-preview");
        let built = build_command(&r, &spec, "the prompt body").unwrap();

        assert_eq!(built.program, "/opt/bin/agy");
        assert!(built.args.windows(2).any(|w| w[0] == "-p"));
        assert!(built
            .args
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "gemini-3-flash-preview"));
        assert!(built
            .args
            .windows(2)
            .all(|w| !(w[0] == "-o" && w[1] == "json")));

        assert!(built.schema_path.is_none());
    }

    #[tokio::test]
    async fn test_container_sandbox_is_rejected() {
        let r = CliRunner::new();
        let mut spec = stub_spec("claude", "opus[1m]");
        spec.sandbox = SandboxPolicy::Container;

        let input = AgentInput {
            context: Default::default(),
            role: "summary".to_string(),
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
    fn test_extract_json_text_prefers_claude_structured_output() {
        let wrapped = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "structured_output": {"foo": "bar"},
            "result": "ignored text"
        })
        .to_string();
        let got = extract_json_text("claude", &wrapped);
        assert_eq!(got, "{\"foo\":\"bar\"}");
    }

    /// A7: Gemini's `-o json` mode wraps the model output in a
    /// `{"session_id": "...", "response": "<inner>", "stats": {...}}`
    /// envelope. `extract_json_text("gemini", ...)` must return only the
    /// inner string so downstream JSON-schema validation runs against the
    /// model's actual reply, not the envelope. Sample shape captured live
    /// from `gemini -o json -p '...'`.
    #[test]
    fn test_extract_json_text_unwraps_gemini_wrapper() {
        let wrapped = serde_json::json!({
            "session_id": "abc-123",
            "response": "{\"summary\":\"ok\"}",
            "stats": {
                "models": {"Gemini 3.5 Flash (Medium)": {"tokens": {"total": 99}}}
            }
        })
        .to_string();
        let got = extract_json_text("gemini", &wrapped);
        assert_eq!(got, "{\"summary\":\"ok\"}");
    }

    /// When the gemini wrapper is missing (e.g. an error blob slipped
    /// through), fall back to returning the raw stdout so the operator can
    /// see whatever gemini actually emitted.
    #[test]
    fn test_extract_json_text_gemini_falls_back_when_no_wrapper() {
        let raw = "not a json wrapper";
        let got = extract_json_text("gemini", raw);
        assert_eq!(got, raw);
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
    fn test_parse_and_validate_extracts_fenced_json_after_prose() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        });
        let raw = "Here is the JSON:\n\n```json\n{\"foo\":\"bar\"}\n```";

        let v = parse_and_validate(raw, &schema).expect("fenced object extracted");

        assert_eq!(v["foo"], "bar");
    }

    #[test]
    fn test_parse_and_validate_prefers_last_valid_json_object() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        });
        let raw = "First attempt:\n```json\n{\"foo\":7}\n```\n\n{\"foo\":\"bar\"}";

        let v = parse_and_validate(raw, &schema).expect("last valid object extracted");

        assert_eq!(v["foo"], "bar");
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

    #[test]
    fn test_parse_and_validate_truncates_raw_output_in_errors() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["foo"],
            "properties": { "foo": { "type": "string" } }
        });
        let raw = format!("not json {}", "x".repeat(4_000));

        let err = parse_and_validate(&raw, &schema).unwrap_err().to_string();

        assert!(
            err.contains("raw_excerpt"),
            "expected a bounded raw excerpt in parse errors: {err}"
        );
        assert!(
            err.len() < 1_800,
            "parse error should stay bounded for observability logs, got {} chars",
            err.len()
        );
    }

    #[test]
    fn repair_malformed_json_escapes_raw_newline_in_string() {
        // Model emitted a literal newline inside a `source_tex` string (common on multi-line
        // LaTeX/proof text) — invalid JSON. Deterministic salvage should escape it and parse.
        let bad = "{\"tool_calls\":[{\"id\":\"c\",\"name\":\"submit\",\"arguments\":{\"theorem_graph\":[{\"id\":\"p\",\"source_tex\":\"::: proof\n*Proof.* x ◻\n:::\"}]}}]}";
        assert!(
            serde_json::from_str::<serde_json::Value>(bad).is_err(),
            "fixture must be invalid JSON as-is"
        );
        let repaired = repair_malformed_json(bad).expect("salvage should recover the payload");
        let v: serde_json::Value = serde_json::from_str(&repaired).expect("repaired parses");
        assert_eq!(
            v["tool_calls"][0]["arguments"]["theorem_graph"][0]["source_tex"],
            "::: proof\n*Proof.* x ◻\n:::"
        );
    }

    #[test]
    fn repair_malformed_json_strips_trailing_commas() {
        let bad = "{\"a\":[1,2,],\"b\":{\"x\":1,},}";
        assert!(serde_json::from_str::<serde_json::Value>(bad).is_err());
        let repaired = repair_malformed_json(bad).expect("salvage trailing commas");
        let v: serde_json::Value = serde_json::from_str(&repaired).expect("parses");
        assert_eq!(v["a"], serde_json::json!([1, 2]));
        assert_eq!(v["b"]["x"], 1);
    }

    #[test]
    fn repair_malformed_json_returns_none_when_unrepairable() {
        // An unescaped quote mid-string is not in scope for deterministic salvage; caller
        // escalates to the LLM repair call rather than guessing.
        let bad = "{\"a\":\"he said \"hi\" loudly\"}";
        assert!(repair_malformed_json(bad).is_none());
    }

    #[test]
    fn escape_unescaped_control_chars_leaves_valid_json_untouched() {
        let good = "{\"a\":\"line1\\nline2\",\"b\":[1,2]}";
        assert_eq!(escape_unescaped_control_chars(good), good);
    }

    #[test]
    fn parse_tool_envelope_accepts_valid_tool_call() {
        let tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }];
        let raw = serde_json::json!({
            "text": "Need the body.",
            "tool_calls": [{
                "id": "call_1",
                "name": "read_file",
                "arguments": {"path": "body.md"}
            }]
        })
        .to_string();

        let parsed = parse_tool_envelope(&raw, &tools).expect("valid envelope");
        assert_eq!(parsed.text, "Need the body.");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].name, "read_file");
        assert_eq!(parsed.tool_calls[0].arguments["path"], "body.md");
    }

    #[test]
    fn parse_tool_envelope_rejects_unknown_tool() {
        let tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let raw = serde_json::json!({
            "tool_calls": [{
                "name": "shell_out",
                "arguments": {}
            }]
        })
        .to_string();

        let err = parse_tool_envelope(&raw, &tools).unwrap_err();
        assert!(
            err.to_string().contains("unknown tool"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_tool_completion_unwraps_claude_result_envelope() {
        let tools = vec![ToolSpec {
            name: "submit".into(),
            description: "Submit final payload".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let inner = serde_json::json!({
            "tool_calls": [{
                "name": "submit",
                "arguments": {"ok": true}
            }]
        })
        .to_string();
        let wrapped = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "result": inner,
            "usage": {"input_tokens": 12, "output_tokens": 8}
        })
        .to_string();

        let completion =
            parse_tool_completion("claude", &wrapped, &tools).expect("valid completion");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].id, "cli_call_1");
        assert_eq!(completion.tool_calls[0].name, "submit");
        assert_eq!(completion.usage.tokens_in, 12);
        assert_eq!(completion.usage.tokens_out, 8);
        assert_eq!(completion.finish_reason, FinishReason::ToolUse);
    }

    #[test]
    fn parse_tool_completion_unwraps_gemini_response_envelope() {
        let tools = vec![ToolSpec {
            name: "list_files".into(),
            description: "List files".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let inner = serde_json::json!({
            "tool_calls": [{
                "id": "gem_1",
                "name": "list_files",
                "arguments": {}
            }]
        })
        .to_string();
        let wrapped = serde_json::json!({
            "session_id": "abc",
            "response": inner,
            "stats": {
                "models": {
                    "Gemini 3.5 Flash (Medium)": {
                        "tokens": {"prompt_tokens": 5, "completion_tokens": 3}
                    }
                }
            }
        })
        .to_string();

        let completion =
            parse_tool_completion("gemini", &wrapped, &tools).expect("valid completion");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].id, "gem_1");
        assert_eq!(completion.tool_calls[0].name, "list_files");
        assert_eq!(completion.usage.tokens_in, 5);
        assert_eq!(completion.usage.tokens_out, 3);
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
        assert!(
            s.contains("--runner api"),
            "display missing fallback hint: {s}"
        );
    }

    #[test]
    fn cli_quota_fallback_requires_explicit_provider() {
        let _env = EnvVarGuard::clear(&[
            "AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER",
            "AGENTHERO_CLI_QUOTA_FALLBACK_MODEL",
        ]);
        let spec = stub_spec("gemini", "gemini-3-flash-preview");
        let err = anyhow::Error::new(CliError::QuotaExhausted {
            provider: "gemini".to_string(),
            message: "quota exceeded".to_string(),
        });

        assert!(cli_quota_fallback_spec(&spec, &err).is_none());
    }

    #[test]
    fn cli_quota_fallback_defaults_gemini_to_current_agy_model() {
        let _env = EnvVarGuard::set(&[
            ("AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER", "gemini"),
            ("AGENTHERO_CLI_QUOTA_FALLBACK_MODEL", ""),
        ]);
        let spec = stub_spec("claude", "sonnet[1m]");
        let err = anyhow::Error::new(CliError::QuotaExhausted {
            provider: "claude".to_string(),
            message: "session limit reached".to_string(),
        });

        let fallback = cli_quota_fallback_spec(&spec, &err)
            .expect("explicit gemini fallback should be configured");
        assert_eq!(fallback.provider, "gemini");
        assert_eq!(fallback.model, "Gemini 3.5 Flash (Medium)");
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
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let err = exec_and_capture(&built, Some(Duration::from_secs(5)), "summary", "openai")
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
            other => panic!("unexpected CliError: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_classifies_claude_session_limit_on_stdout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"You'\''ve hit your session limit · resets 6:10am (America/Costa_Rica)"}'
exit 1
"#,
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let err = exec_and_capture(&built, Some(Duration::from_secs(5)), "summary", "claude")
            .await
            .expect_err("subprocess should exit non-zero");

        let downcast = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>())
            .expect("error chain should carry CliError for stdout session limits");
        match downcast {
            CliError::QuotaExhausted { provider, message } => {
                assert_eq!(provider, "claude");
                assert!(
                    message.to_lowercase().contains("session limit"),
                    "stdout snippet missing session-limit signal: {message}"
                );
            }
            other => panic!("unexpected CliError: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_uses_explicit_openai_cli_fallback_when_gemini_quota_is_exhausted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let agy = dir.path().join("fake-agy.sh");
        let codex = dir.path().join("fake-codex.sh");
        std::fs::write(
            &agy,
            "#!/bin/sh\nprintf 'RESOURCE_EXHAUSTED (code 429): Individual quota reached' >&2\nexit 1\n",
        )
        .expect("write fake agy");
        std::fs::write(
            &codex,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"ok\\\":true}\"}}'\n",
        )
        .expect("write fake codex");
        for path in [&agy, &codex] {
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }

        let _env = EnvVarGuard::set_owned(&[
            (
                "AGENTHERO_ANTIGRAVITY_BIN",
                agy.to_string_lossy().to_string(),
            ),
            ("AGENTHERO_CODEX_BIN", codex.to_string_lossy().to_string()),
            (
                "AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER",
                "openai".to_string(),
            ),
            (
                "AGENTHERO_CLI_QUOTA_FALLBACK_MODEL",
                "gpt-fallback".to_string(),
            ),
        ]);
        let mut spec = stub_spec("gemini", "gemini-3-flash-preview");
        spec.role = "citation".to_string();
        let input = AgentInput {
            context: Default::default(),
            role: "citation".to_string(),
            content_hash_material: serde_json::json!({"paper": "x"}),
            artifact: serde_json::json!({"title": "Paper", "sections": []}),
            system_prompt: "system".to_string(),
            user_prompt: "return json".to_string(),
            source_bundle_path: None,
        };

        let run = CliRunner::new()
            .run(&spec, &input)
            .await
            .expect("quota-exhausted gemini should fall back to codex CLI");
        assert_eq!(run.model, "gpt-fallback");
        assert_eq!(run.output, serde_json::json!({"ok": true}));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_uses_explicit_agy_fallback_when_claude_quota_is_exhausted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let claude = dir.path().join("fake-claude.sh");
        let agy = dir.path().join("fake-agy.sh");
        std::fs::write(
            &claude,
            "#!/bin/sh\necho 'Error: session limit reached; rate limit exceeded' >&2\nexit 1\n",
        )
        .expect("write fake claude");
        std::fs::write(
            &agy,
            "#!/bin/sh\nprintf '%s\\n' '{\"ok\":true,\"provider\":\"agy\"}'\n",
        )
        .expect("write fake agy");
        for path in [&claude, &agy] {
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }

        let _env = EnvVarGuard::set_owned(&[
            ("AGENTHERO_CLAUDE_BIN", claude.to_string_lossy().to_string()),
            (
                "AGENTHERO_ANTIGRAVITY_BIN",
                agy.to_string_lossy().to_string(),
            ),
            (
                "AGENTHERO_CLI_QUOTA_FALLBACK_PROVIDER",
                "gemini".to_string(),
            ),
            (
                "AGENTHERO_CLI_QUOTA_FALLBACK_MODEL",
                "Gemini 3.5 Flash (Medium)".to_string(),
            ),
        ]);
        let mut spec = stub_spec("claude", "sonnet[1m]");
        spec.role = "formalize_source_inventory_typed_transcriber".to_string();
        let input = AgentInput {
            context: Default::default(),
            role: "formalize_source_inventory_typed_transcriber".to_string(),
            content_hash_material: serde_json::json!({"paper": "x"}),
            artifact: serde_json::json!({"title": "Paper", "sections": []}),
            system_prompt: "system".to_string(),
            user_prompt: "return json".to_string(),
            source_bundle_path: None,
        };

        let run = CliRunner::new()
            .run(&spec, &input)
            .await
            .expect("quota-exhausted claude should fall back to agy when explicitly configured");
        assert_eq!(run.model, "Gemini 3.5 Flash (Medium)");
        assert_eq!(
            run.output,
            serde_json::json!({"ok": true, "provider": "agy"})
        );
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
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let err = exec_and_capture(&built, Some(Duration::from_secs(5)), "summary", "claude")
            .await
            .expect_err("subprocess should exit non-zero");

        let downcast = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>());
        assert!(
            downcast.is_none(),
            "non-quota failures must not be tagged as QuotaExhausted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_rejects_empty_gemini_stdout_without_quota() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-gemini.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
exit 0
"#,
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let err = exec_and_capture(&built, Some(Duration::from_secs(5)), "citation", "gemini")
            .await
            .expect_err("empty gemini stdout should fail before schema parsing");

        assert!(
            err.to_string().contains("empty stdout"),
            "unexpected error: {err:#}"
        );
        let downcast = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>());
        assert!(
            downcast.is_none(),
            "non-quota empty stdout must not be tagged as QuotaExhausted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_kills_child_on_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-cli.sh");
        let pid_file = dir.path().join("pid");
        let child_pid_file = dir.path().join("child-pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho $$ > '{}'\nsleep 30 &\necho $! > '{}'\nwait\n",
                pid_file.to_string_lossy(),
                child_pid_file.to_string_lossy()
            ),
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let err = exec_and_capture(
            &built,
            Some(Duration::from_secs(3)),
            "custom_validator",
            "gemini",
        )
        .await
        .expect_err("subprocess should time out");
        assert!(err.to_string().contains("timed out"), "{err:#}");
        let timeout_error = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>())
            .expect("timeout should be structured");
        match timeout_error {
            CliError::TimedOut {
                provider,
                role,
                timeout_secs,
                ..
            } => {
                assert_eq!(provider, "gemini");
                assert_eq!(role, "custom_validator");
                assert_eq!(*timeout_secs, 3);
            }
            other => panic!("unexpected CliError: {other:?}"),
        }

        let pid = read_pid_file(&pid_file).await.expect("pid file");
        let child_pid = read_pid_file(&child_pid_file)
            .await
            .expect("child pid file");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let parent_alive = process_is_alive(&pid);
        let child_alive = process_is_alive(&child_pid);
        if parent_alive || child_alive {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid, &child_pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        assert!(
            !parent_alive,
            "timed-out child process {pid} should have been killed"
        );
        assert!(
            !child_alive,
            "timed-out grandchild process {child_pid} should have been killed"
        );
    }

    #[cfg(unix)]
    async fn read_pid_file(path: &std::path::Path) -> Option<String> {
        for _ in 0..20 {
            match std::fs::read_to_string(path) {
                Ok(value) => return Some(value.trim().to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        None
    }

    #[cfg(unix)]
    fn process_is_alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_and_capture_scrubs_provider_api_env_for_gemini_child() {
        use std::os::unix::fs::PermissionsExt;
        let _env = EnvVarGuard::set(&[
            ("ANTHROPIC_API_KEY", "parent-anthropic-key"),
            ("OPENAI_API_KEY", "parent-openai-key"),
            (
                "GOOGLE_GENERATIVE_AI_API_KEY",
                "parent-google-generative-key",
            ),
            ("GOOGLE_API_KEY", "parent-google-key"),
            ("GEMINI_API_KEY", "parent-gemini-key"),
        ]);

        let dir = std::env::temp_dir().join("grokrxiv-cli-env-scrub-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("fake-cli.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
printf '{"anthropic":"%s","openai":"%s","google_genai":"%s","google":"%s","gemini":"%s","marker":"%s"}\n' "${ANTHROPIC_API_KEY+x}" "${OPENAI_API_KEY+x}" "${GOOGLE_GENERATIVE_AI_API_KEY+x}" "${GOOGLE_API_KEY+x}" "${GEMINI_API_KEY+x}" "${GROKRXIV_CLI_API_ENV_SCRUBBED:-}"
"#,
        )
        .expect("write fake script");
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let built = BuiltCommand {
            program: script.to_string_lossy().to_string(),
            args: vec![],
            stdin_payload: String::new(),
            pipe_stdin: false,
            schema_path: None,
            cwd: None,
        };

        let stdout = exec_and_capture(&built, Some(Duration::from_secs(5)), "summary", "gemini")
            .await
            .expect("subprocess should succeed");
        let observed: serde_json::Value =
            serde_json::from_str(&stdout).expect("fake script should emit JSON");

        assert_eq!(observed["anthropic"], "");
        assert_eq!(observed["openai"], "");
        assert_eq!(observed["google_genai"], "");
        assert_eq!(observed["google"], "");
        assert_eq!(observed["gemini"], "");
        assert_eq!(observed["marker"], "1");
    }

    #[tokio::test]
    async fn extraction_api_fallback_is_refused_when_direct_api_disabled() {
        let _env = EnvVarGuard::set(&[
            ("AGENTHERO_EXTRACTION_TOOL_FALLBACK", "api"),
            ("AGENTHERO_EXTRACTOR", "cli"),
        ]);
        let r = CliRunner::new();
        let spec = stub_spec("claude", "claude-test");
        let workdir = std::env::temp_dir().join("grokrxiv-cli-fallback-refused-test");
        let _ = std::fs::create_dir_all(&workdir);
        let ctx = ToolCtx {
            workdir: &workdir,
            semantic_ast: None,
            source_id: "2401.00001",
            http: std::sync::Arc::new(reqwest::Client::new()),
        };

        let err = r
            .complete_with_tools(&spec, &[], &[], &ctx)
            .await
            .expect_err("legacy API fallback must be fail-closed in CLI mode");
        let msg = err.to_string();
        assert!(
            msg.contains("AGENTHERO_EXTRACTION_TOOL_FALLBACK=api refused"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("--extractor api"),
            "error should name the explicit API opt-in: {msg}"
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

    #[test]
    fn antigravity_auth_reports_local_state_marker() {
        let home = tempfile::tempdir().unwrap();
        let marker = home.path().join(".gemini").join("antigravity");
        std::fs::create_dir_all(&marker).unwrap();

        let _home = EnvVarGuard::set_owned(&[("HOME", home.path().to_string_lossy().into_owned())]);
        let (auth_method, state) = inspect_antigravity_auth();
        assert_eq!(auth_method, "antigravity_keyring");
        assert!(state.contains(".gemini/antigravity"), "{state}");
    }

    /// Reference base64url encoder for the JWT decoder unit test.
    fn b64url_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
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
