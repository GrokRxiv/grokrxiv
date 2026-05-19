//! `CliRunner` — local CLI subprocess for tool-using agents.
//!
//! Spawns `claude` / `codex` / `gemini` based on `spec.provider`. No runtime
//! `--cli-agent` flag — the YAML's existing `provider:` field is the source
//! of truth.
//!
//! RPT2 Track B: host-only execution. `SandboxPolicy::Container` is explicitly
//! rejected so callers don't silently get "ran on host when you asked for
//! container".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
use crate::runtime_config::ALLOW_PROVIDER_API_ENV;
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
            "claude" => {
                Ok(std::env::var("GROKRXIV_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()))
            }
            "openai" => {
                Ok(std::env::var("GROKRXIV_CODEX_BIN").unwrap_or_else(|_| "codex".to_string()))
            }
            "gemini" => {
                Ok(std::env::var("GROKRXIV_GEMINI_BIN").unwrap_or_else(|_| "gemini".to_string()))
            }
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
    /// Working directory for the child process. Keeping CLI children out of
    /// the repo root prevents provider CLIs from scanning the whole checkout
    /// when they fall back to their own local tools.
    cwd: Option<PathBuf>,
}

#[async_trait]
impl AgentRunner for CliRunner {
    fn name(&self) -> &'static str {
        "cli"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
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
        log_auth_path_once(&spec.provider);

        // 2. Pre-flight: ensure the Claude skill is installed on disk before
        //    spawning. Idempotent.
        if spec.provider == "claude" {
            if let Err(e) = ensure_grokrxiv_review_skill_installed() {
                tracing::warn!(err = %e, "failed to install grokrxiv-review claude skill");
            }
        }

        let review_workdir = prepare_review_workdir(spec, input)?;
        let prompt = render_review_prompt_with_files(input);

        // 3. First attempt.
        let mut built = build_command(self, spec, &prompt)?;
        built.cwd = Some(review_workdir.path().to_path_buf());
        let raw_stdout =
            match exec_and_capture(&built, timeout_dur, spec.role, &spec.provider).await {
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
                    "Your previous output failed JSON/schema validation.\n\
                     Validation error:\n{first_err}\n\n\
                     Return exactly one JSON object and make it validate against this schema. \
                     Do not include prose, markdown fences, or extra properties.\n\n\
                     Schema:\n{schema}\n\n\
                     Original task:\n{prompt}",
                    schema = serde_json::to_string(&spec.schema).unwrap_or_default(),
                    prompt = prompt,
                );
                let mut built2 = build_command(self, spec, &corrective)?;
                built2.cwd = Some(review_workdir.path().to_path_buf());
                let raw2 =
                    match exec_and_capture(&built2, timeout_dur, spec.role, &spec.provider).await {
                        Ok(s) => s,
                        Err(e) => {
                            cleanup_schema_path(&built2.schema_path);
                            return Err(e);
                        }
                    };
                cleanup_schema_path(&built2.schema_path);
                let extracted2 = extract_json_text(&spec.provider, &raw2);
                match parse_and_validate(&extracted2, &spec.schema) {
                    Ok(v) => v,
                    Err(second_err) => {
                        if let Some(repaired) = repair_review_output(
                            spec.role,
                            &extracted2,
                            &spec.schema,
                            &input.artifact,
                        )
                        .or_else(|| {
                            repair_review_output(
                                spec.role,
                                &extracted,
                                &spec.schema,
                                &input.artifact,
                            )
                        }) {
                            repaired
                        } else {
                            return Err(anyhow::anyhow!(
                                "CliRunner parse/validate failure after corrective retry for role {role:?}: first={first_err}; retry={second_err}",
                                role = spec.role,
                            ));
                        }
                    }
                }
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
        // Legacy escape valve kept for old smoke scripts. The canonical
        // operator surface is now `GROKRXIV_EXTRACTOR=api`, which selects the
        // ApiRunner before this method is called.
        let fallback = std::env::var("GROKRXIV_EXTRACTION_TOOL_FALLBACK")
            .ok()
            .filter(|s| s == "api");
        if fallback.is_some() {
            if !extractor_api_selected() || !direct_provider_api_allowed() {
                anyhow::bail!(
                    "GROKRXIV_EXTRACTION_TOOL_FALLBACK=api refused because direct provider API \
                     is disabled for this CLI run; use --extractor api to allow API billing, \
                     or --extractor cli to use local logged-in CLIs"
                );
            }
            let providers = build_api_fallback_providers(spec)?;
            let api = super::api::ApiRunner::new(providers);
            return api.complete_with_tools(spec, messages, tools, ctx).await;
        }

        if !matches!(spec.provider.as_str(), "claude" | "gemini") {
            anyhow::bail!(
                "CliRunner.complete_with_tools: provider `{}` is not supported for native \
                 CLI extraction; set GROKRXIV_EXTRACTOR=api or --extractor api for this run",
                spec.provider
            );
        }

        let started = Instant::now();
        let timeout_dur = cli_timeout_for(spec);
        log_auth_path_once(&spec.provider);

        let prompt = render_tool_prompt(spec, messages, tools, ctx)?;
        let built = build_tool_command(self, spec, &prompt, ctx.workdir)?;
        let raw_stdout =
            match exec_and_capture(&built, timeout_dur, spec.role, &spec.provider).await {
                Ok(s) => s,
                Err(e) => return Err(e),
            };

        match parse_tool_completion(&spec.provider, &raw_stdout, tools) {
            Ok(mut completion) => {
                completion.raw = enrich_cli_tool_raw(completion.raw, started.elapsed());
                Ok(completion)
            }
            Err(first_err) => {
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
                let raw2 =
                    match exec_and_capture(&built2, timeout_dur, spec.role, &spec.provider).await {
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
                            "CliRunner tool-envelope parse failure after corrective retry for \
                             provider={} model={}: first={first_err}; retry={second_err}",
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

fn prepare_review_workdir(
    spec: &AgentSpec,
    input: &AgentInput,
) -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("grokrxiv-review-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("create review CLI workdir: {e}"))?;
    write_json_file(&dir.path().join("review_input.json"), &input.artifact)?;
    write_json_file(&dir.path().join("schema.json"), &spec.schema)?;
    std::fs::write(dir.path().join("prompt.md"), &input.user_prompt)
        .map_err(|e| anyhow::anyhow!("write prompt.md: {e}"))?;
    std::fs::write(dir.path().join("system.md"), &input.system_prompt)
        .map_err(|e| anyhow::anyhow!("write system.md: {e}"))?;
    std::fs::write(
        dir.path().join("README.md"),
        "GrokRxiv prepared this directory for one review role.\n\
         Use review_input.json as the paper/review artifact, prompt.md as the task, \
         system.md as the role instruction, and schema.json as the required output schema.\n\
         Do not search parent directories or the GrokRxiv repository.\n",
    )
    .map_err(|e| anyhow::anyhow!("write README.md: {e}"))?;
    Ok(dir)
}

fn write_json_file(path: &std::path::Path, value: &serde_json::Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| anyhow::anyhow!("serialise {}: {e}", path.display()))?;
    std::fs::write(path, body).map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))
}

fn render_review_prompt_with_files(input: &AgentInput) -> String {
    format!(
        "{system}\n\n\
         GrokRxiv has prepared the exact review inputs in your current working directory.\n\
         Use these files only:\n\
         - review_input.json: canonical JSON artifact to review\n\
         - prompt.md: role-specific task\n\
         - system.md: role instruction\n\
         - schema.json: required output schema\n\n\
         Do not search parent directories. Do not inspect the GrokRxiv repository checkout. \
         If you use local file tools, restrict them to the current directory and these files.\n\n\
         Role task:\n{user}",
        system = input.system_prompt,
        user = input.user_prompt,
    )
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
        arxiv_id = ctx.arxiv_id,
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
    let args = match spec.provider.as_str() {
        "claude" => vec![
            "-p".to_string(),
            "-".to_string(),
            "--model".to_string(),
            spec.model.clone(),
            "--output-format".to_string(),
            "json".to_string(),
        ],
        "gemini" => vec![
            "-p".to_string(),
            prompt.to_string(),
            "--model".to_string(),
            spec.model.clone(),
            "--approval-mode".to_string(),
            "plan".to_string(),
            "-o".to_string(),
            "json".to_string(),
        ],
        other => anyhow::bail!("unsupported provider for CLI tool loop: {other}"),
    };
    Ok(BuiltCommand {
        program,
        args,
        stdin_payload: prompt.to_string(),
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
    Ok(ToolCompletion {
        finish_reason: if envelope.tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolUse
        },
        text: envelope.text,
        tool_calls: envelope.tool_calls,
        usage: usage_from_cli_wrapper(provider, raw_stdout),
        raw: raw_cli_payload(provider, raw_stdout, &extracted),
    })
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
///   1. `GROKRXIV_CLI_TIMEOUT_SECS` env var (operator override).
///   2. `spec.timeout_secs` from `agents/<role>.yaml`.
///   3. `DEFAULT_CLI_TIMEOUT_SECS`.
/// The env var stays a global escape hatch but no longer silently overrides
/// the YAML's per-role budget.
fn cli_timeout_for(spec: &AgentSpec) -> Duration {
    if let Some(secs) = std::env::var("GROKRXIV_CLI_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Duration::from_secs(secs);
    }
    if spec.timeout_secs > 0 {
        return Duration::from_secs(spec.timeout_secs.into());
    }
    Duration::from_secs(DEFAULT_CLI_TIMEOUT_SECS)
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

    // A7: for claude AND gemini, the JSON-only output contract is enforced
    // via the `/grokrxiv-review` skill which both CLIs resolve from a
    // `/skill-name` prefix on the prompt body (neither CLI has a `--skill`
    // flag). codex uses `--output-schema` instead, so no prefix there.
    let provider_prompt = if spec.provider == "claude" || spec.provider == "gemini" {
        format!("/{CLAUDE_SKILL_NAME}\n\n{prompt}")
    } else {
        prompt.to_string()
    };

    let (args, schema_path) = match spec.provider.as_str() {
        "claude" => {
            // Pass the prompt via stdin (`-p -`) to avoid argv-length limits.
            // NOTE: claude CLI does NOT have a `--skill` flag — skills are
            // invoked via `/skill-name` at the start of the prompt body
            // (help text: "Skills still resolve via /skill-name").
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
                "--skip-git-repo-check".to_string(),
                "--json".to_string(),
                "--output-schema".to_string(),
                path.to_string_lossy().into_owned(),
                provider_prompt.clone(),
            ];
            (args, Some(path))
        }
        "gemini" => {
            // A7: emit JSON via `-o json`. Gemini's headless mode wraps the
            // model output in `{"session_id": ..., "response": "<inner>",
            // "stats": {...}}` — `extract_json_text` unwraps that wrapper
            // before schema validation. Without `-o json` gemini emits prose,
            // which forces every CLI Gemini call into the corrective-retry
            // path (B3). The prompt itself carries the `/grokrxiv-review`
            // prefix so the model knows to emit only the role-schema JSON.
            let args = vec![
                "-p".to_string(),
                provider_prompt.clone(),
                "--model".to_string(),
                spec.model.clone(),
                "--approval-mode".to_string(),
                "plan".to_string(),
                "-o".to_string(),
                "json".to_string(),
            ];
            (args, None)
        }
        other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
    };

    // `stdin_payload` is only consumed by the claude branch (which reads
    // `-p -`); for codex and gemini the prompt is already in argv. We still
    // populate the field for symmetry / debugging so `BuiltCommand` always
    // captures the prompt the model will actually see.
    let stdin_payload = provider_prompt;

    Ok(BuiltCommand {
        program,
        args,
        stdin_payload,
        schema_path,
        cwd: None,
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
    if let Some(cwd) = &built.cwd {
        cmd.current_dir(cwd);
    }
    cmd.kill_on_drop(true);
    scrub_provider_api_env(&mut cmd);
    if provider == "gemini" {
        // Extraction and review subprocesses run in isolated temp workdirs so
        // CLI tools cannot scan the repo root. Gemini requires an explicit
        // trust signal for headless automation in such directories.
        cmd.env("GEMINI_CLI_TRUST_WORKSPACE", "true");
    }
    tracing::info!(
        provider = %provider,
        program = %built.program,
        api_env_scrubbed = true,
        "CLI subprocess API env scrubbed"
    );

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

fn direct_provider_api_allowed() -> bool {
    matches!(
        std::env::var(ALLOW_PROVIDER_API_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn extractor_api_selected() -> bool {
    matches!(std::env::var("GROKRXIV_EXTRACTOR").as_deref(), Ok("api"))
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
    let parsed: serde_json::Value = serde_json::from_str(cleaned)
        .map_err(|e| anyhow::anyhow!("not valid JSON: {e}; raw={extracted:?}"))?;

    validate_parsed(parsed, schema)
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

fn repair_review_output(
    role: grokrxiv_schemas::AgentRole,
    extracted: &str,
    schema: &serde_json::Value,
    artifact: &serde_json::Value,
) -> Option<serde_json::Value> {
    use grokrxiv_schemas::AgentRole;

    let repaired = match role {
        AgentRole::Novelty => {
            let parsed = parse_json_lenient(extracted)?;
            repair_novelty_review(parsed)?
        }
        AgentRole::Reproducibility => parse_json_lenient(extracted)
            .and_then(repair_reproducibility_review)
            .or_else(|| Some(fallback_reproducibility_review(artifact)))?,
        AgentRole::Citation => parse_json_lenient(extracted)
            .and_then(repair_citation_review)
            .or_else(|| Some(fallback_citation_review(artifact)))?,
        _ => return None,
    };

    validate_parsed(repaired, schema).ok()
}

fn parse_json_lenient(extracted: &str) -> Option<serde_json::Value> {
    let cleaned = strip_code_fences(extracted.trim());
    if cleaned.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(cleaned) {
        return Some(v);
    }
    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    if start >= end {
        return None;
    }
    serde_json::from_str(&cleaned[start..=end]).ok()
}

fn repair_novelty_review(parsed: serde_json::Value) -> Option<serde_json::Value> {
    let obj = parsed.as_object()?;
    let score = obj
        .get("novelty_score")
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| score_from_verdict(obj.get("verdict").and_then(|v| v.as_str())));
    let verdict = normalize_novelty_verdict(obj.get("verdict").and_then(|v| v.as_str()), score);
    let confidence = obj
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    let related_work = obj
        .get("related_work")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(normalize_related_work_item)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let missing_prior_art = obj
        .get("missing_prior_art")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let title = text_field(item, &["title", "citation", "work"])?;
                    let reason = text_field(item, &["reason", "comment", "delta", "notes"])
                        .unwrap_or_else(|| {
                            "Mentioned by CLI reviewer as potentially relevant.".to_string()
                        });
                    Some(serde_json::json!({
                        "title": title,
                        "reason": reason,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "novelty_score": score.clamp(0.0, 1.0),
        "related_work": related_work,
        "missing_prior_art": missing_prior_art,
        "verdict": verdict,
        "confidence": confidence,
    }))
}

fn normalize_related_work_item(item: &serde_json::Value) -> Option<serde_json::Value> {
    let citation = item.get("citation");
    let citation_key = item
        .get("citation_key")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            citation?
                .get("key")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let title = text_field(item, &["title", "work"])
        .or_else(|| citation.and_then(|c| text_field(c, &["title", "raw", "key"])))?;
    let relation = normalize_relation(item.get("relation").and_then(|v| v.as_str()));
    let delta = text_field(
        item,
        &["delta", "comment", "explanation", "notes", "reason"],
    )
    .unwrap_or_else(|| {
        "CLI reviewer identified this as related work but did not provide a schema-compliant delta."
            .to_string()
    });

    Some(serde_json::json!({
        "citation_key": citation_key,
        "title": title,
        "relation": relation,
        "delta": delta,
    }))
}

fn normalize_relation(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "builds_on" | "builds on" | "extends" | "extension" | "supporting" => "builds_on",
        "competing" | "contrasts" | "contrast" | "alternative" => "competing",
        "prior_art" | "prior art" | "background" | "baseline" => "prior_art",
        "orthogonal" | "complementary" | "related" | "adjacent" => "orthogonal",
        _ => "prior_art",
    }
}

fn normalize_novelty_verdict(raw: Option<&str>, score: f64) -> &'static str {
    let lower = raw.unwrap_or("").to_ascii_lowercase();
    for verdict in ["significant", "incremental", "marginal", "duplicative"] {
        if lower.contains(verdict) {
            return verdict;
        }
    }
    if score >= 0.75 {
        "significant"
    } else if score >= 0.45 {
        "incremental"
    } else if score >= 0.2 {
        "marginal"
    } else {
        "duplicative"
    }
}

fn score_from_verdict(raw: Option<&str>) -> f64 {
    match normalize_novelty_verdict(raw, 0.5) {
        "significant" => 0.85,
        "incremental" => 0.6,
        "marginal" => 0.3,
        "duplicative" => 0.05,
        _ => 0.5,
    }
}

fn repair_reproducibility_review(parsed: serde_json::Value) -> Option<serde_json::Value> {
    let obj = parsed.as_object()?;
    let environment = obj.get("environment").cloned().unwrap_or_else(|| {
        serde_json::json!({
            "hardware": null,
            "software": null,
            "dependencies": [],
        })
    });
    let concerns = obj
        .get("concerns")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(normalize_reproducibility_concern)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "code_availability": normalize_code_availability(obj.get("code_availability").and_then(|v| v.as_str())),
        "code_url": nullable_string(obj.get("code_url")),
        "data_availability": normalize_data_availability(obj.get("data_availability").and_then(|v| v.as_str())),
        "data_url": nullable_string(obj.get("data_url")),
        "environment": normalize_environment(&environment),
        "concerns": concerns,
        "reproducibility_score": obj
            .get("reproducibility_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.35)
            .clamp(0.0, 1.0),
        "confidence": obj
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.55)
            .clamp(0.0, 1.0),
    }))
}

fn fallback_reproducibility_review(artifact: &serde_json::Value) -> serde_json::Value {
    let body = artifact
        .get("sections")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("body_markdown").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let code_url = first_url_matching(&body, &["github.com", "gitlab.com", "zenodo.org"]);
    let has_code = code_url.is_some();
    serde_json::json!({
        "code_availability": if has_code { "open_source" } else { "unspecified" },
        "code_url": code_url,
        "data_availability": "unspecified",
        "data_url": null,
        "environment": {
            "hardware": null,
            "software": null,
            "dependencies": [],
        },
        "concerns": [{
            "area": "code",
            "description": "The extracted paper does not provide enough machine-checkable implementation detail or runnable code artifacts for a direct reproduction from GrokRxiv artifacts alone.",
            "severity": if has_code { "minor" } else { "major" },
        }, {
            "area": "data",
            "description": "The extracted paper does not expose a separate reproducibility data bundle in the review input.",
            "severity": "minor",
        }],
        "reproducibility_score": if has_code { 0.55 } else { 0.25 },
        "confidence": 0.55,
    })
}

fn normalize_reproducibility_concern(item: &serde_json::Value) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "area": normalize_repro_area(item.get("area").and_then(|v| v.as_str())),
        "description": text_field(item, &["description", "concern", "issue", "notes"])?,
        "severity": normalize_repro_severity(item.get("severity").and_then(|v| v.as_str())),
    }))
}

fn normalize_environment(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "hardware": nullable_string(value.get("hardware")),
        "software": nullable_string(value.get("software")),
        "dependencies": value
            .get("dependencies")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
}

fn normalize_code_availability(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "open_source" | "open source" | "public" | "available" => "open_source",
        "available_on_request" | "available on request" | "request" => "available_on_request",
        "proprietary" | "closed" | "closed_source" => "proprietary",
        _ => "unspecified",
    }
}

fn normalize_data_availability(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "public" | "open" | "available" => "public",
        "restricted" => "restricted",
        "synthetic" => "synthetic",
        "private" => "private",
        _ => "unspecified",
    }
}

fn normalize_repro_area(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "code" => "code",
        "data" => "data",
        "compute" => "compute",
        "hyperparameters" | "hyperparameter" => "hyperparameters",
        "evaluation" => "evaluation",
        _ => "other",
    }
}

fn normalize_repro_severity(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "critical" => "critical",
        "major" => "major",
        "minor" => "minor",
        _ => "info",
    }
}

fn first_url_matching(body: &str, hosts: &[&str]) -> Option<String> {
    body.split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '.')
            })
        })
        .find(|token| {
            token.starts_with("http")
                && hosts
                    .iter()
                    .any(|host| token.to_ascii_lowercase().contains(host))
        })
        .map(str::to_string)
}

fn repair_citation_review(parsed: serde_json::Value) -> Option<serde_json::Value> {
    let obj = parsed.as_object()?;
    let entries = obj
        .get("entries")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| normalize_citation_entry(item, idx))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        return None;
    }

    let missing_references = obj
        .get("missing_references")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let title = text_field(item, &["title", "citation", "work"])?;
                    let reason = text_field(item, &["reason", "comment", "notes"])
                        .unwrap_or_else(|| "Flagged by CLI citation reviewer.".to_string());
                    Some(serde_json::json!({
                        "title": title,
                        "reason": reason,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "entries": entries,
        "missing_references": missing_references,
        "summary": text_field(&parsed, &["summary"]).unwrap_or_else(|| {
            "CLI citation output was normalized to the GrokRxiv citation schema.".to_string()
        }),
        "confidence": obj
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0),
    }))
}

fn fallback_citation_review(artifact: &serde_json::Value) -> serde_json::Value {
    let entries = artifact
        .get("bibliography")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(idx, item)| fallback_citation_entry(item, idx, artifact))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    serde_json::json!({
        "entries": entries,
        "missing_references": [],
        "summary": "Deterministic citation review generated from extracted bibliography and in-text citation contexts. Entries are preserved for moderation; external existence lookups were not performed in this stage.",
        "confidence": 0.55,
    })
}

fn normalize_citation_entry(item: &serde_json::Value, idx: usize) -> Option<serde_json::Value> {
    let citation = item.get("citation")?;
    let citation_obj = normalize_citation(citation, idx);
    Some(serde_json::json!({
        "citation": citation_obj,
        "exists": item.get("exists").and_then(|v| v.as_bool()).unwrap_or(false),
        "resolved_doi": nullable_string(item.get("resolved_doi")),
        "resolved_url": nullable_string(item.get("resolved_url").or_else(|| item.get("url"))),
        "relevance": normalize_relevance(item.get("relevance").and_then(|v| v.as_str())),
        "notes": nullable_string(item.get("notes").or_else(|| item.get("comment"))),
        "explanation": text_field(item, &["explanation", "reason", "comment", "notes"])
            .unwrap_or_else(|| "CLI citation output was normalized to the GrokRxiv citation schema.".to_string()),
    }))
}

fn fallback_citation_entry(
    item: &serde_json::Value,
    idx: usize,
    artifact: &serde_json::Value,
) -> serde_json::Value {
    let contexts = citation_contexts_for(item, artifact);
    let (notes, explanation, relevance) = if contexts.is_empty() {
        (
            "No matching in-text citation context was found in the extracted body.".to_string(),
            "Bibliography entry preserved for human moderation; no matching extracted citation context was found and no external citation lookup was completed.".to_string(),
            "medium",
        )
    } else {
        let joined = contexts
            .iter()
            .take(3)
            .map(|(section, sentence)| format!("{section}: {sentence}"))
            .collect::<Vec<_>>()
            .join(" | ");
        (
            format!("{} extracted citation context(s) matched.", contexts.len()),
            format!("Cited in extracted paper context(s): {joined}"),
            "high",
        )
    };
    serde_json::json!({
        "citation": normalize_citation(item, idx),
        "exists": false,
        "resolved_doi": null,
        "resolved_url": null,
        "relevance": relevance,
        "notes": notes,
        "explanation": explanation,
    })
}

fn normalize_citation(citation: &serde_json::Value, idx: usize) -> serde_json::Value {
    let key = citation_key(citation).unwrap_or_else(|| format!("[{}]", idx + 1));
    serde_json::json!({
        "key": key,
        "raw": nullable_string(citation.get("raw")).or_else(|| Some(citation.to_string())),
        "title": nullable_string(citation.get("title")),
        "authors": citation
            .get("authors")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "year": citation.get("year").and_then(|v| v.as_i64()),
        "venue": nullable_string(citation.get("venue")),
        "doi": nullable_string(citation.get("doi")),
        "arxiv_id": nullable_string(citation.get("arxiv_id")),
        "url": nullable_string(citation.get("url")),
    })
}

fn citation_key(citation: &serde_json::Value) -> Option<String> {
    citation
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            let raw = citation.get("raw")?.as_str()?.trim();
            if !raw.is_empty() && raw.len() <= 96 && !raw.contains(' ') {
                Some(raw.to_string())
            } else {
                None
            }
        })
}

fn citation_contexts_for(
    citation: &serde_json::Value,
    artifact: &serde_json::Value,
) -> Vec<(String, String)> {
    let Some(key) = citation_key(citation) else {
        return Vec::new();
    };
    // Strip surrounding brackets so a key like `[1]` produces needles for `[1]`
    // (numeric citation style) not `[[1]]`. Some extractors emit `key="[1]"`,
    // others emit `key="1"`; handle both.
    let bare = key
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let bracketed = format!("[{bare}]");
    let mut needles: Vec<String> = vec![
        format!("@{bare}"),    // BibTeX-style `@Deutsch1991`
        bracketed.clone(),     // Numeric `[1]`
        format!("{{{bare}}}"), // LaTeX `{Deutsch1991}`
    ];
    // Preserve the original key as a needle too — covers cases where the
    // body already includes the bracketed form (e.g. raw cite key `Deutsch1991`).
    if key != bare && !needles.contains(&key) {
        needles.push(key.clone());
    }
    artifact
        .get("sections")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .flat_map(|section| {
            let heading = section
                .get("heading")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let body = section
                .get("body_markdown")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            extract_sentences(body)
                .into_iter()
                .filter({
                    let needles = needles.clone();
                    move |sentence| needles.iter().any(|needle| sentence.contains(needle))
                })
                .map(move |sentence| (heading.clone(), truncate_context(&sentence)))
        })
        .take(5)
        .collect()
}

fn extract_sentences(body: &str) -> Vec<String> {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentences = Vec::new();
    let mut current = String::new();
    for ch in normalized.chars() {
        current.push(ch);
        if matches!(ch, '.' | '?' | '!') {
            let sentence = current.trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_string());
            }
            current.clear();
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }
    sentences
}

fn truncate_context(sentence: &str) -> String {
    const MAX: usize = 360;
    if sentence.chars().count() <= MAX {
        return sentence.to_string();
    }
    let mut out: String = sentence.chars().take(MAX.saturating_sub(16)).collect();
    out.push_str("... [truncated]");
    out
}

fn normalize_relevance(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("").to_ascii_lowercase().as_str() {
        "high" => "high",
        "medium" => "medium",
        "low" => "low",
        "unrelated" => "unrelated",
        _ => "medium",
    }
}

fn nullable_string(v: Option<&serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn text_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match value.get(*key) {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                return Some(s.clone());
            }
            Some(other) if other.is_object() || other.is_array() => {
                if let Some(s) = text_field(other, &["title", "raw", "key"]) {
                    return Some(s);
                }
            }
            Some(other) if !other.is_null() => return Some(other.to_string()),
            _ => {}
        }
    }
    None
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
                tracing::info!(
                    provider = "openai",
                    auth_method = %auth_method,
                    "codex CLI uses local CLI auth; provider API key env is scrubbed"
                );
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
                tracing::info!(
                    provider = "gemini",
                    auth_method = %auth_method,
                    "gemini CLI uses local CLI auth; provider API key env is scrubbed"
                );
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

/// Best-effort read of the local Gemini CLI auth files. The Gemini CLI's
/// `/auth` flow stores OAuth credentials under `~/.gemini/oauth_creds.json`
/// and the selected method under `~/.gemini/settings.json`; prefer those over
/// any unrelated gcloud ADC file that may also exist on the machine. Returns
/// `(auth_method, account, quota_project)`.
fn inspect_gemini_auth() -> (String, String, String) {
    let Ok(home) = std::env::var("HOME") else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let gemini_dir = PathBuf::from(&home).join(".gemini");
    let settings = gemini_dir.join("settings.json");
    let selected_type = std::fs::read(&settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|val| {
            val.pointer("/security/auth/selectedType")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    let account = std::fs::read(gemini_dir.join("google_accounts.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|val| {
            val.get("active")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    if gemini_dir.join("oauth_creds.json").exists() {
        let auth_method = match selected_type.as_deref() {
            Some("oauth-personal") => "gemini_cli_oauth_personal",
            Some("compute-default-credentials") => "gemini_cli_compute_adc",
            Some("gemini-api-key") => "gemini_cli_api_key",
            Some("vertex-ai") => "gemini_cli_vertex_ai",
            Some(other) => other,
            None => "gemini_cli_oauth",
        };
        return (auth_method.into(), account, "n/a".into());
    }

    let path = PathBuf::from(home)
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ("unknown".into(), "unknown".into(), "unknown".into());
    };
    let typ = val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
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
    use grokrxiv_schemas::AgentRole;

    fn stub_spec(provider: &str, model: &str) -> AgentSpec {
        let mut s =
            AgentSpec::api_default(AgentRole::Summary, provider.to_string(), model.to_string());
        s.runner = AgentRunnerKind::Cli;
        s.schema = serde_json::json!({});
        s
    }

    struct EnvVarGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvVarGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { saved }
        }

        fn set_owned(vars: &[(&'static str, String)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { saved }
        }

        fn clear(vars: &[&'static str]) -> Self {
            let saved = vars
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in vars {
                std::env::remove_var(key);
            }
            Self { saved }
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
        assert!(
            msg.contains("foo"),
            "error should name the bad provider: {msg}"
        );
    }

    /// Phase: spec.timeout_secs plumbing. Run as a single test to keep env-var
    /// state changes serial (parallel test threads share process env).
    #[test]
    fn cli_timeout_for_resolution_order() {
        // 1. env var wins over everything.
        {
            let _guard = EnvVarGuard::set(&[("GROKRXIV_CLI_TIMEOUT_SECS", "42")]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 999;
            assert_eq!(cli_timeout_for(&spec), Duration::from_secs(42));
        }
        // 2. no env var → spec wins over default.
        {
            let _guard = EnvVarGuard::clear(&["GROKRXIV_CLI_TIMEOUT_SECS"]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 120;
            assert_eq!(cli_timeout_for(&spec), Duration::from_secs(120));
        }
        // 3. no env var, spec=0 → falls back to default.
        {
            let _guard = EnvVarGuard::clear(&["GROKRXIV_CLI_TIMEOUT_SECS"]);
            let mut spec = stub_spec("claude", "claude-haiku-4-5");
            spec.timeout_secs = 0;
            assert_eq!(
                cli_timeout_for(&spec),
                Duration::from_secs(DEFAULT_CLI_TIMEOUT_SECS)
            );
        }
    }

    #[test]
    fn extraction_tool_command_runs_inside_workdir() {
        let r = CliRunner::new();
        let spec = stub_spec("gemini", "gemini-test");
        let workdir = std::env::temp_dir().join("grokrxiv-cli-tool-cwd-test");

        let built = build_tool_command(&r, &spec, "prompt", &workdir).expect("build command");

        assert_eq!(built.cwd.as_deref(), Some(workdir.as_path()));
    }

    #[test]
    fn review_workdir_materializes_explicit_input_files() {
        let spec = stub_spec("gemini", "gemini-test");
        let input = AgentInput {
            paper_id: uuid::Uuid::nil(),
            review_id: uuid::Uuid::nil(),
            role: AgentRole::Summary,
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
        assert!(rendered.contains("Do not search parent directories"));
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
        let spec = stub_spec("gemini", "gemini-2.5-pro");
        let built = build_command(&r, &spec, "the prompt body").unwrap();

        assert!(
            built.program.ends_with("gemini"),
            "program should be gemini binary, got {}",
            built.program
        );

        let args = &built.args;

        // A7 lock-in #1: `-p` carries the prompt body with the
        // `/grokrxiv-review` skill prefix prepended. The prompt is passed as
        // a positional argv value (gemini's headless mode reads from `-p`),
        // not stdin — so the exact string we send is what the model sees.
        let prompt_idx = args
            .iter()
            .position(|a| a == "-p")
            .expect("missing -p flag in gemini args");
        let prompt_value = args
            .get(prompt_idx + 1)
            .expect("missing -p value in gemini args");
        assert!(
            prompt_value.starts_with("/grokrxiv-review"),
            "expected -p value to start with /grokrxiv-review, got {prompt_value:?}"
        );
        assert!(
            prompt_value.contains("the prompt body"),
            "expected -p value to contain the original prompt, got {prompt_value:?}"
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

        // A7 lock-in #2: `-o json` is set so gemini emits the
        // `{"session_id": ..., "response": "<inner>", "stats": {...}}`
        // wrapper that `extract_json_text` unwraps below. Without this flag
        // gemini emits prose and every CLI Gemini call falls into the
        // corrective-retry path (B3).
        assert!(
            args.windows(2).any(|w| w[0] == "-o" && w[1] == "json"),
            "missing `-o json` pair in {args:?}"
        );

        // A7 lock-in #3: stdin_payload mirrors the argv prompt (so the field
        // can be used for debug logs of the exact text the model received).
        assert!(
            built.stdin_payload.starts_with("/grokrxiv-review"),
            "stdin_payload should also carry the /grokrxiv-review prefix, got {:?}",
            built.stdin_payload
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
                "models": {"gemini-2.5-flash": {"tokens": {"total": 99}}}
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
                    "gemini-2.5-flash": {
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

    #[test]
    fn novelty_repair_normalizes_common_cli_shape() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../schemas/novelty_review.schema.json"
        ))
        .expect("novelty schema parses");
        let raw = serde_json::json!({
            "novelty_score": 0.82,
            "related_work": [{
                "citation": {"key": "Smith2024", "title": "Baseline Work"},
                "relation": "complementary",
                "comment": "The submitted paper uses a different technique."
            }],
            "missing_prior_art": [{"work": "Older Method", "comment": "Should be discussed."}],
            "verdict": "The paper is a significant contribution.",
            "confidence": 0.7
        })
        .to_string();

        let repaired =
            repair_review_output(AgentRole::Novelty, &raw, &schema, &serde_json::json!({}))
                .expect("repair validates");

        assert_eq!(repaired["verdict"], "significant");
        assert_eq!(repaired["related_work"][0]["citation_key"], "Smith2024");
        assert_eq!(repaired["related_work"][0]["relation"], "orthogonal");
        assert_eq!(
            repaired["related_work"][0]["delta"],
            "The submitted paper uses a different technique."
        );
    }

    #[test]
    fn citation_repair_builds_honest_fallback_from_bibliography() {
        let schema = citation_review_schema_for_test();
        let artifact = serde_json::json!({
            "bibliography": [{
                "raw": "doe2024",
                "title": "A Useful Paper",
                "doi": "10.1234/example",
                "arxiv_id": null
            }],
            "sections": [{
                "heading": "Introduction",
                "body_markdown": "This result follows earlier work [@doe2024]."
            }]
        });

        let repaired = repair_review_output(AgentRole::Citation, "", &schema, &artifact)
            .expect("fallback validates");

        assert_eq!(repaired["entries"].as_array().unwrap().len(), 1);
        assert_eq!(repaired["entries"][0]["citation"]["key"], "doe2024");
        assert_eq!(
            repaired["entries"][0]["citation"]["title"],
            "A Useful Paper"
        );
        assert_eq!(repaired["entries"][0]["exists"], false);
        assert_eq!(repaired["entries"][0]["relevance"], "high");
        assert!(repaired["entries"][0]["explanation"]
            .as_str()
            .unwrap()
            .contains("Introduction"));
        assert!(repaired["summary"]
            .as_str()
            .unwrap()
            .contains("external existence lookups were not performed"));
    }

    #[test]
    fn reproducibility_repair_builds_schema_valid_fallback() {
        let schema = reproducibility_review_schema_for_test();
        let artifact = serde_json::json!({
            "sections": [{
                "heading": "Methods",
                "body_markdown": "We solve the equations numerically and do not provide source code."
            }]
        });

        let repaired = repair_review_output(AgentRole::Reproducibility, "", &schema, &artifact)
            .expect("fallback validates");

        assert_eq!(repaired["code_availability"], "unspecified");
        assert_eq!(repaired["data_availability"], "unspecified");
        assert_eq!(
            repaired["environment"]["dependencies"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(repaired["concerns"].as_array().unwrap().len() >= 2);
        assert_eq!(repaired["concerns"][0]["area"], "code");
    }

    fn citation_review_schema_for_test() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "entries": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "citation": {
                                "type": "object",
                                "additionalProperties": false,
                                "properties": {
                                    "key": {"type": "string"},
                                    "raw": {"type": ["string", "null"]},
                                    "title": {"type": ["string", "null"]},
                                    "authors": {"type": "array", "items": {"type": "string"}},
                                    "year": {"type": ["integer", "null"]},
                                    "venue": {"type": ["string", "null"]},
                                    "doi": {"type": ["string", "null"]},
                                    "arxiv_id": {"type": ["string", "null"]},
                                    "url": {"type": ["string", "null"]}
                                },
                                "required": ["key", "raw", "title", "authors", "year", "venue", "doi", "arxiv_id", "url"]
                            },
                            "exists": {"type": "boolean"},
                            "resolved_doi": {"type": ["string", "null"]},
                            "resolved_url": {"type": ["string", "null"]},
                            "relevance": {"type": "string", "enum": ["high", "medium", "low", "unrelated"]},
                            "notes": {"type": ["string", "null"]},
                            "explanation": {"type": "string"}
                        },
                        "required": ["citation", "exists", "resolved_doi", "resolved_url", "relevance", "notes", "explanation"]
                    }
                },
                "missing_references": {"type": "array"},
                "summary": {"type": "string"},
                "confidence": {"type": "number"}
            },
            "required": ["entries", "missing_references", "summary", "confidence"]
        })
    }

    fn reproducibility_review_schema_for_test() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../../../schemas/reproducibility_review.schema.json"
        ))
        .unwrap()
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
            cwd: None,
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
            cwd: None,
        };

        let err = exec_and_capture(
            &built,
            Duration::from_secs(5),
            grokrxiv_schemas::AgentRole::Summary,
            "claude",
        )
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
    async fn exec_and_capture_scrubs_provider_api_env_for_cli_child() {
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
printf '{"anthropic":"%s","openai":"%s","google_genai":"%s","google":"%s","gemini":"%s","marker":"%s","gemini_trust":"%s"}\n' "${ANTHROPIC_API_KEY+x}" "${OPENAI_API_KEY+x}" "${GOOGLE_GENERATIVE_AI_API_KEY+x}" "${GOOGLE_API_KEY+x}" "${GEMINI_API_KEY+x}" "${GROKRXIV_CLI_API_ENV_SCRUBBED:-}" "${GEMINI_CLI_TRUST_WORKSPACE:-}"
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
            schema_path: None,
            cwd: None,
        };

        let stdout = exec_and_capture(
            &built,
            Duration::from_secs(5),
            grokrxiv_schemas::AgentRole::Summary,
            "gemini",
        )
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
        assert_eq!(observed["gemini_trust"], "true");
    }

    #[tokio::test]
    async fn extraction_api_fallback_is_refused_when_direct_api_disabled() {
        let _env = EnvVarGuard::set(&[
            ("GROKRXIV_EXTRACTION_TOOL_FALLBACK", "api"),
            ("GROKRXIV_EXTRACTOR", "cli"),
        ]);
        let r = CliRunner::new();
        let spec = stub_spec("claude", "claude-test");
        let workdir = std::env::temp_dir().join("grokrxiv-cli-fallback-refused-test");
        let _ = std::fs::create_dir_all(&workdir);
        let ctx = ToolCtx {
            workdir: &workdir,
            semantic_ast: None,
            arxiv_id: "2401.00001",
            http: std::sync::Arc::new(reqwest::Client::new()),
        };

        let err = r
            .complete_with_tools(&spec, &[], &[], &ctx)
            .await
            .expect_err("legacy API fallback must be fail-closed in CLI mode");
        let msg = err.to_string();
        assert!(
            msg.contains("GROKRXIV_EXTRACTION_TOOL_FALLBACK=api refused"),
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
    fn gemini_auth_prefers_local_oauth_creds_over_gcloud_adc() {
        let home = tempfile::tempdir().unwrap();
        let gemini = home.path().join(".gemini");
        let gcloud = home.path().join(".config").join("gcloud");
        std::fs::create_dir_all(&gemini).unwrap();
        std::fs::create_dir_all(&gcloud).unwrap();
        std::fs::write(
            gemini.join("settings.json"),
            r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
        )
        .unwrap();
        std::fs::write(
            gemini.join("google_accounts.json"),
            r#"{"active":"mlong168@gmail.com","old":[]}"#,
        )
        .unwrap();
        std::fs::write(
            gemini.join("oauth_creds.json"),
            r#"{"access_token":"redacted","refresh_token":"redacted"}"#,
        )
        .unwrap();
        std::fs::write(
            gcloud.join("application_default_credentials.json"),
            r#"{"type":"authorized_user","quota_project_id":"wrong-project"}"#,
        )
        .unwrap();

        let _home = EnvVarGuard::set_owned(&[("HOME", home.path().to_string_lossy().into_owned())]);
        let (auth_method, account, quota_project) = inspect_gemini_auth();
        assert_eq!(auth_method, "gemini_cli_oauth_personal");
        assert_eq!(account, "mlong168@gmail.com");
        assert_eq!(quota_project, "n/a");
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
