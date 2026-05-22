//! Layered runtime configuration: CLI flags > ENV > TOML file > defaults.
//!
//! `Config` (in `config.rs`) is the legacy env-only config the supervisor
//! still consumes. `RuntimeConfig` here is the new per-invocation config the
//! CLI assembles from layered sources for the `grokrxiv` binary's operator
//! surface (RPT2 Track I). It augments — does not replace — `Config`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::{AgentMode, AgentRunnerKind, RevisionTarget, SandboxPolicy};

/// Internal guard consumed by `ApiRunner`.
///
/// The CLI sets this after resolving `--runner` / `--extractor`. Direct
/// provider APIs are disabled unless the operator explicitly selected an API
/// backend for review or extraction.
pub const ALLOW_PROVIDER_API_ENV: &str = "AGENTHERO_ALLOW_PROVIDER_API";

/// Central env interpretation for direct provider API access.
pub fn direct_provider_api_allowed_from_env() -> bool {
    matches!(
        std::env::var(ALLOW_PROVIDER_API_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

/// Prefix for internal, already-resolved model overrides exported by the CLI
/// before `AppState` builds the per-role agent registry.
pub const MODEL_OVERRIDE_ENV_PREFIX: &str = "AGENTHERO_MODEL_OVERRIDE_";

/// Which backend runs staged extraction agents during ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[clap(rename_all = "snake_case")]
pub enum ExtractorKind {
    /// Local CLI subprocesses (`claude` / `gemini`) propose tool calls.
    Cli,
    /// Direct provider API tool-calling.
    Api,
}

impl Default for ExtractorKind {
    fn default() -> Self {
        Self::Cli
    }
}

impl ExtractorKind {
    /// Stable lowercase string for env exports and config rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Api => "api",
        }
    }

    /// Corresponding runner registry key.
    pub fn runner_kind(self) -> AgentRunnerKind {
        match self {
            Self::Cli => AgentRunnerKind::Cli,
            Self::Api => AgentRunnerKind::Api,
        }
    }
}

/// Layered runtime configuration consumed by the CLI before it builds the
/// `AppState` / agent registry. CLI flags override env vars, which override
/// the TOML file, which overrides the built-in defaults.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Default runner backend (API / CLI / cloud / local-inference).
    pub default_runner: AgentRunnerKind,
    /// Default extraction backend (CLI by default; API when explicitly opted in).
    pub extractor: ExtractorKind,
    /// Default sandbox policy.
    pub default_sandbox: SandboxPolicy,
    /// Default mode (`review_only` or `review_and_revise`).
    pub default_mode: AgentMode,
    /// Where revisions land when mode is `review_and_revise`.
    pub revision_target: RevisionTarget,
    /// Hard cap on total cost (USD) for one review.
    pub max_cost_usd: Option<f64>,
    /// Skip the review cache (force fresh runner calls).
    pub no_cache: bool,
    /// Offline mode — disallow network for runners that can avoid it.
    pub offline: bool,
    /// Selected cloud-agent provider (e.g. `vercel_open_agents`, `e2b`).
    pub cloud_provider: Option<String>,
    /// LiteLLM gateway URL (preferred for local-inference + multi-provider routing).
    pub litellm_url: Option<String>,
    /// Direct Ollama host (fallback when no LiteLLM gateway).
    pub ollama_host: Option<String>,
    /// Bearer token clients must present to call the web API write endpoints.
    pub service_token: Option<String>,
    /// Per-role runner override.
    pub runner_for: HashMap<String, AgentRunnerKind>,
    /// Per-role sandbox override.
    pub sandbox_for: HashMap<String, SandboxPolicy>,
    /// Per-role cloud-provider override.
    pub cloud_provider_for: HashMap<String, String>,
    /// Per-role model override (lands as `AgentSpec.model`).
    pub model_for: HashMap<String, String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_runner: AgentRunnerKind::Cli,
            extractor: ExtractorKind::Cli,
            default_sandbox: SandboxPolicy::None,
            default_mode: AgentMode::ReviewOnly,
            revision_target: RevisionTarget::PaperLatex,
            max_cost_usd: None,
            no_cache: false,
            offline: false,
            cloud_provider: None,
            litellm_url: None,
            ollama_host: None,
            service_token: None,
            runner_for: HashMap::new(),
            sandbox_for: HashMap::new(),
            cloud_provider_for: HashMap::new(),
            model_for: HashMap::new(),
        }
    }
}

/// On-disk TOML profile shape.
#[derive(Debug, Default, Deserialize)]
pub struct TomlConfig {
    /// Per-profile sub-tables.
    #[serde(default)]
    pub profiles: HashMap<String, TomlProfile>,
}

/// TOML representation of a single profile.
#[derive(Debug, Default, Deserialize)]
pub struct TomlProfile {
    /// Default runner backend.
    pub runner: Option<String>,
    /// Default staged-extraction backend (`cli` / `api`).
    pub extractor: Option<String>,
    /// Default sandbox policy.
    pub sandbox: Option<String>,
    /// Default mode.
    pub mode: Option<String>,
    /// Cloud provider.
    pub cloud_provider: Option<String>,
    /// LiteLLM URL.
    pub litellm_url: Option<String>,
    /// Ollama host.
    pub ollama_host: Option<String>,
    /// Service token (web API).
    pub service_token: Option<String>,
    /// Max-cost ceiling (USD).
    pub max_cost_usd: Option<f64>,
    /// `--no-cache`.
    pub no_cache: Option<bool>,
    /// `--offline`.
    pub offline: Option<bool>,
    /// Per-role runner overrides (e.g. `summary = "cli"`).
    #[serde(default)]
    pub runner_for: HashMap<String, String>,
    /// Per-role sandbox overrides.
    #[serde(default)]
    pub sandbox_for: HashMap<String, String>,
    /// Per-role cloud-provider overrides.
    #[serde(default)]
    pub cloud_provider_for: HashMap<String, String>,
    /// Per-role model overrides.
    #[serde(default)]
    pub model_for: HashMap<String, String>,
}

impl RuntimeConfig {
    /// Layered resolve. The order is: defaults → TOML file → env vars → CLI flags.
    ///
    /// `cli_overrides` carries the parsed Cli's global-flag values; pass
    /// `RuntimeConfigOverrides::default()` if you only want the file+env layer.
    pub fn resolve(
        cli_overrides: &RuntimeConfigOverrides,
        profile: &str,
        config_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let mut out = RuntimeConfig::default();

        // ---- 2. TOML file ----
        let path = config_path.map(PathBuf::from).or_else(default_toml_path);
        if let Some(path) = path.as_deref() {
            if path.is_file() {
                let raw = std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
                let toml_cfg: TomlConfig = toml::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
                if let Some(p) = toml_cfg.profiles.get(profile) {
                    apply_toml(&mut out, p);
                }
            }
        }

        // ---- 3. Environment ----
        apply_env(&mut out)?;

        // ---- 4. CLI ----
        apply_cli(&mut out, cli_overrides);

        Ok(out)
    }
}

/// Whether this resolved runtime config intentionally permits direct provider
/// API calls. CLI runs leave this false so local subscription-backed CLIs are
/// the only LLM path.
pub fn provider_api_allowed(cfg: &RuntimeConfig) -> bool {
    cfg.default_runner == AgentRunnerKind::Api
        || cfg.extractor == ExtractorKind::Api
        || cfg
            .runner_for
            .values()
            .any(|runner| *runner == AgentRunnerKind::Api)
}

/// Subset of the parsed `Cli` whose values get layered onto a `RuntimeConfig`.
///
/// Holding it in its own struct avoids a cyclic dep between `cli.rs` and
/// `runtime_config.rs` while letting the CLI hand the values over verbatim.
#[derive(Debug, Default, Clone)]
pub struct RuntimeConfigOverrides {
    /// `--runner`.
    pub runner: Option<AgentRunnerKind>,
    /// `--extractor`.
    pub extractor: Option<ExtractorKind>,
    /// `--sandbox`.
    pub sandbox: Option<SandboxPolicy>,
    /// `--mode`.
    pub mode: Option<AgentMode>,
    /// `--revision-target`.
    pub revision_target: Option<RevisionTarget>,
    /// `--cloud-provider`.
    pub cloud_provider: Option<String>,
    /// `--litellm-url`.
    pub litellm_url: Option<String>,
    /// `--ollama-host`.
    pub ollama_host: Option<String>,
    /// `--max-cost-usd`.
    pub max_cost_usd: Option<f64>,
    /// `--no-cache`.
    pub no_cache: bool,
    /// `--offline`.
    pub offline: bool,
    /// `--runner-for <role>=<runner>` pairs.
    pub runner_for: Vec<(String, AgentRunnerKind)>,
    /// `--model-for <role>=<model>` pairs.
    pub model_for: Vec<(String, String)>,
}

fn default_toml_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".grokrxiv").join("config.toml");
    Some(p)
}

fn apply_toml(out: &mut RuntimeConfig, p: &TomlProfile) {
    if let Some(s) = &p.runner {
        if let Some(r) = parse_runner(s) {
            out.default_runner = r;
        }
    }
    if let Some(s) = &p.extractor {
        if let Some(r) = parse_extractor(s) {
            out.extractor = r;
        }
    }
    if let Some(s) = &p.sandbox {
        if let Some(r) = parse_sandbox(s) {
            out.default_sandbox = r;
        }
    }
    if let Some(s) = &p.mode {
        if let Some(r) = parse_mode(s) {
            out.default_mode = r;
        }
    }
    if let Some(s) = &p.cloud_provider {
        out.cloud_provider = Some(s.clone());
    }
    if let Some(s) = &p.litellm_url {
        out.litellm_url = Some(s.clone());
    }
    if let Some(s) = &p.ollama_host {
        out.ollama_host = Some(s.clone());
    }
    if let Some(s) = &p.service_token {
        out.service_token = Some(s.clone());
    }
    if let Some(v) = p.max_cost_usd {
        out.max_cost_usd = Some(v);
    }
    if let Some(v) = p.no_cache {
        out.no_cache = v;
    }
    if let Some(v) = p.offline {
        out.offline = v;
    }
    for (role_s, runner_s) in &p.runner_for {
        if let (Some(role), Some(runner)) = (parse_role(role_s), parse_runner(runner_s)) {
            out.runner_for.insert(role, runner);
        }
    }
    for (role_s, sandbox_s) in &p.sandbox_for {
        if let (Some(role), Some(sandbox)) = (parse_role(role_s), parse_sandbox(sandbox_s)) {
            out.sandbox_for.insert(role, sandbox);
        }
    }
    for (role_s, cp) in &p.cloud_provider_for {
        if let Some(role) = parse_role(role_s) {
            out.cloud_provider_for.insert(role, cp.clone());
        }
    }
    for (role_s, model) in &p.model_for {
        if let Some(role) = parse_role(role_s) {
            out.model_for.insert(role, model.clone());
        }
    }
}

fn apply_env(out: &mut RuntimeConfig) -> anyhow::Result<()> {
    if let Ok(v) = std::env::var("AGENTHERO_RUNNER") {
        if let Some(r) = parse_runner(&v) {
            out.default_runner = r;
        }
    }
    if let Ok(v) = std::env::var("AGENTHERO_EXTRACTOR") {
        out.extractor = parse_extractor(&v).ok_or_else(|| {
            anyhow::anyhow!("invalid AGENTHERO_EXTRACTOR={v:?}; expected one of: cli, api")
        })?;
    } else if matches!(
        std::env::var("AGENTHERO_EXTRACTION_TOOL_FALLBACK").as_deref(),
        Ok("api")
    ) {
        out.extractor = ExtractorKind::Api;
    }
    if let Ok(v) = std::env::var("AGENTHERO_SANDBOX") {
        if let Some(r) = parse_sandbox(&v) {
            out.default_sandbox = r;
        }
    }
    if let Ok(v) = std::env::var("AGENTHERO_MODE") {
        if let Some(r) = parse_mode(&v) {
            out.default_mode = r;
        }
    }
    if let Ok(v) = std::env::var("AGENTHERO_CLOUD_PROVIDER") {
        out.cloud_provider = Some(v);
    }
    if let Ok(v) = std::env::var("AGENTHERO_LITELLM_URL") {
        out.litellm_url = Some(v);
    }
    if let Ok(v) = std::env::var("OLLAMA_HOST") {
        out.ollama_host = Some(v);
    }
    if let Ok(v) = std::env::var("AGENTHERO_SERVICE_TOKEN") {
        out.service_token = Some(v);
    }
    if let Ok(v) = std::env::var("AGENTHERO_MAX_COST_USD") {
        if let Ok(parsed) = v.parse::<f64>() {
            out.max_cost_usd = Some(parsed);
        }
    }
    if matches!(
        std::env::var("GROKRXIV_NO_CACHE").as_deref(),
        Ok("1") | Ok("true")
    ) {
        out.no_cache = true;
    }
    if matches!(
        std::env::var("AGENTHERO_OFFLINE").as_deref(),
        Ok("1") | Ok("true")
    ) {
        out.offline = true;
    }
    for role in configured_review_agent_roles() {
        if let Some(model) = model_from_env_var(&role_model_env_var(&role)) {
            out.model_for.insert(role, model);
        }
    }
    Ok(())
}

fn apply_cli(out: &mut RuntimeConfig, cli: &RuntimeConfigOverrides) {
    if let Some(r) = cli.runner {
        out.default_runner = r;
    }
    if let Some(r) = cli.extractor {
        out.extractor = r;
    }
    if let Some(s) = cli.sandbox {
        out.default_sandbox = s;
    }
    if let Some(m) = cli.mode {
        out.default_mode = m;
    }
    if let Some(t) = cli.revision_target {
        out.revision_target = t;
    }
    if let Some(s) = &cli.cloud_provider {
        out.cloud_provider = Some(s.clone());
    }
    if let Some(s) = &cli.litellm_url {
        out.litellm_url = Some(s.clone());
    }
    if let Some(s) = &cli.ollama_host {
        out.ollama_host = Some(s.clone());
    }
    if let Some(v) = cli.max_cost_usd {
        out.max_cost_usd = Some(v);
    }
    if cli.no_cache {
        out.no_cache = true;
    }
    if cli.offline {
        out.offline = true;
    }
    for (role, runner) in &cli.runner_for {
        out.runner_for.insert(role.clone(), *runner);
    }
    for (role, model) in &cli.model_for {
        out.model_for.insert(role.clone(), model.clone());
    }
}

fn parse_runner(s: &str) -> Option<AgentRunnerKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "api" => Some(AgentRunnerKind::Api),
        "cli" => Some(AgentRunnerKind::Cli),
        "cloud" => Some(AgentRunnerKind::Cloud),
        "local_inference" | "local-inference" | "local" => Some(AgentRunnerKind::LocalInference),
        _ => None,
    }
}

/// Parse a staged-extraction backend.
pub fn parse_extractor(s: &str) -> Option<ExtractorKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "api" => Some(ExtractorKind::Api),
        "cli" => Some(ExtractorKind::Cli),
        _ => None,
    }
}

fn parse_sandbox(s: &str) -> Option<SandboxPolicy> {
    match s.trim().to_ascii_lowercase().as_str() {
        "none" | "" => Some(SandboxPolicy::None),
        "container" | "docker" => Some(SandboxPolicy::Container),
        _ => None,
    }
}

fn parse_mode(s: &str) -> Option<AgentMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "review_only" | "review-only" => Some(AgentMode::ReviewOnly),
        "review_and_revise" | "review-and-revise" => Some(AgentMode::ReviewAndRevise),
        _ => None,
    }
}

fn parse_role(s: &str) -> Option<String> {
    let raw = s.trim();
    if raw.is_empty() {
        return None;
    }
    let canonical = raw.to_ascii_lowercase().replace('-', "_");
    canonical
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
        .then_some(canonical)
}

/// Role ids for the default review DAG, loaded from the manifest rather than
/// encoded in Rust. Missing manifests simply mean there is no env-model scan.
pub fn configured_review_agent_roles() -> Vec<String> {
    crate::agents::config::dag_agent_config_refs("paper-review")
        .map(|refs| refs.into_iter().map(|r| r.role_id).collect())
        .unwrap_or_default()
}

/// Upper-snake role suffix used in public env vars.
pub fn role_env_suffix(role: &str) -> String {
    role.trim()
        .replace('.', "_")
        .replace('-', "_")
        .to_ascii_uppercase()
}

/// Public per-agent model env var, e.g. `GROKRXIV_CITATION_MODEL`.
pub fn role_model_env_var(role: &str) -> String {
    format!("GROKRXIV_{}_MODEL", role_env_suffix(role))
}

/// Internal resolved model env var, e.g. `AGENTHERO_MODEL_OVERRIDE_CITATION`.
pub fn role_model_override_env_var(role: &str) -> String {
    format!("{}{}", MODEL_OVERRIDE_ENV_PREFIX, role_env_suffix(role))
}

/// Read a model override for a role. The CLI exports the resolved value to the
/// internal override var; non-CLI boot paths can still consume the public env
/// var directly.
pub fn model_override_for_role(role: &str) -> Option<String> {
    model_from_env_var(&role_model_override_env_var(role))
        .or_else(|| model_from_env_var(&role_model_env_var(role)))
}

fn model_from_env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Parse `<role>=<runner>` (used by `--runner-for`).
pub fn parse_role_runner(s: &str) -> Result<(String, AgentRunnerKind), String> {
    let (role_s, runner_s) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <role>=<runner>, got `{s}`"))?;
    let role = parse_role(role_s).ok_or_else(|| format!("unknown role `{role_s}` in `{s}`"))?;
    let runner =
        parse_runner(runner_s).ok_or_else(|| format!("unknown runner `{runner_s}` in `{s}`"))?;
    Ok((role, runner))
}

/// Parse `<role>=<model-id>` (used by `--model-for`).
pub fn parse_role_model(s: &str) -> Result<(String, String), String> {
    let (role_s, model_s) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <role>=<model>, got `{s}`"))?;
    let role = parse_role(role_s).ok_or_else(|| format!("unknown role `{role_s}` in `{s}`"))?;
    if model_s.is_empty() {
        return Err(format!("empty model id in `{s}`"));
    }
    Ok((role, model_s.to_string()))
}

/// Render the resolved config either as TOML (human) or JSON (machine).
/// Secrets (service token) are redacted unless `show_secrets` is true.
pub fn render(cfg: &RuntimeConfig, json: bool, show_secrets: bool) -> String {
    let redact = |s: &Option<String>| -> Option<String> {
        match s {
            Some(v) if show_secrets => Some(v.clone()),
            Some(_) => Some("***".to_string()),
            None => None,
        }
    };
    let runner_for: Vec<_> = cfg
        .runner_for
        .iter()
        .map(|(role, runner)| (format!("{role:?}"), format!("{runner:?}")))
        .collect();
    let model_for: Vec<_> = cfg
        .model_for
        .iter()
        .map(|(role, model)| (format!("{role:?}"), model.clone()))
        .collect();

    let v = serde_json::json!({
        "default_runner": format!("{:?}", cfg.default_runner),
        "extractor": cfg.extractor.as_str(),
        "direct_provider_api_allowed": provider_api_allowed(cfg),
        "default_sandbox": format!("{:?}", cfg.default_sandbox),
        "default_mode": format!("{:?}", cfg.default_mode),
        "revision_target": format!("{:?}", cfg.revision_target),
        "max_cost_usd": cfg.max_cost_usd,
        "no_cache": cfg.no_cache,
        "offline": cfg.offline,
        "cloud_provider": cfg.cloud_provider,
        "litellm_url": cfg.litellm_url,
        "ollama_host": cfg.ollama_host,
        "service_token": redact(&cfg.service_token),
        "runner_for": runner_for,
        "model_for": model_for,
    });
    if json {
        serde_json::to_string_pretty(&v).unwrap_or_default()
    } else {
        let mut s = String::new();
        s.push_str(&format!("default_runner   = {:?}\n", cfg.default_runner));
        s.push_str(&format!("extractor        = {}\n", cfg.extractor.as_str()));
        s.push_str(&format!(
            "direct_provider_api_allowed = {}\n",
            provider_api_allowed(cfg)
        ));
        s.push_str(&format!("default_sandbox  = {:?}\n", cfg.default_sandbox));
        s.push_str(&format!("default_mode     = {:?}\n", cfg.default_mode));
        s.push_str(&format!("revision_target  = {:?}\n", cfg.revision_target));
        s.push_str(&format!(
            "max_cost_usd     = {}\n",
            cfg.max_cost_usd
                .map(|v| v.to_string())
                .unwrap_or_else(|| "<unset>".into())
        ));
        s.push_str(&format!("no_cache         = {}\n", cfg.no_cache));
        s.push_str(&format!("offline          = {}\n", cfg.offline));
        s.push_str(&format!(
            "cloud_provider   = {}\n",
            cfg.cloud_provider.as_deref().unwrap_or("<unset>")
        ));
        s.push_str(&format!(
            "litellm_url      = {}\n",
            cfg.litellm_url.as_deref().unwrap_or("<unset>")
        ));
        s.push_str(&format!(
            "ollama_host      = {}\n",
            cfg.ollama_host.as_deref().unwrap_or("<unset>")
        ));
        s.push_str(&format!(
            "service_token    = {}\n",
            redact(&cfg.service_token).as_deref().unwrap_or("<unset>")
        ));
        if !cfg.runner_for.is_empty() {
            s.push_str("runner_for       =\n");
            for (role, runner) in &runner_for {
                s.push_str(&format!("  {role} = {runner}\n"));
            }
        }
        if !cfg.model_for.is_empty() {
            s.push_str("model_for        =\n");
            for (role, model) in &model_for {
                s.push_str(&format!("  {role} = {model}\n"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_extractor_env<T>(f: impl FnOnce() -> T) -> T {
        let prev = std::env::var("AGENTHERO_EXTRACTOR").ok();
        let prev_fallback = std::env::var("AGENTHERO_EXTRACTION_TOOL_FALLBACK").ok();
        std::env::remove_var("AGENTHERO_EXTRACTOR");
        std::env::remove_var("AGENTHERO_EXTRACTION_TOOL_FALLBACK");
        let out = f();
        if let Some(prev) = prev {
            std::env::set_var("AGENTHERO_EXTRACTOR", prev);
        }
        if let Some(prev) = prev_fallback {
            std::env::set_var("AGENTHERO_EXTRACTION_TOOL_FALLBACK", prev);
        }
        out
    }

    fn with_clean_model_env<T>(f: impl FnOnce() -> T) -> T {
        let names: Vec<String> = configured_review_agent_roles()
            .into_iter()
            .flat_map(|role| {
                [
                    role_model_env_var(&role),
                    role_model_override_env_var(&role),
                ]
            })
            .collect();
        let saved: Vec<(String, Option<String>)> = names
            .iter()
            .map(|name| (name.clone(), std::env::var(name).ok()))
            .collect();
        for name in &names {
            std::env::remove_var(name);
        }
        let out = f();
        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        out
    }

    #[test]
    fn parse_role_runner_ok() {
        let (role, runner) = parse_role_runner("summary=cli").unwrap();
        assert_eq!(role, "summary");
        assert_eq!(runner, AgentRunnerKind::Cli);
    }

    #[test]
    fn parse_role_runner_bad_role() {
        assert!(parse_role_runner("not/a/role=cli").is_err());
    }

    #[test]
    fn parse_role_runner_bad_runner() {
        assert!(parse_role_runner("summary=jetpack").is_err());
    }

    #[test]
    fn parse_role_model_ok() {
        let (role, model) = parse_role_model("technical_correctness=claude-opus-4-7").unwrap();
        assert_eq!(role, "technical_correctness");
        assert_eq!(model, "claude-opus-4-7");
    }

    #[test]
    fn resolves_per_agent_model_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cfg = with_clean_extractor_env(|| {
            with_clean_model_env(|| {
                std::env::set_var("GROKRXIV_NOVELTY_MODEL", "gemini-3-flash-preview");
                std::env::set_var("GROKRXIV_CITATION_MODEL", "gemini-3-flash-preview");
                RuntimeConfig::resolve(
                    &RuntimeConfigOverrides::default(),
                    "nonexistent",
                    Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
                )
            })
        })
        .unwrap();

        assert_eq!(
            cfg.model_for.get("novelty").map(String::as_str),
            Some("gemini-3-flash-preview")
        );
        assert_eq!(
            cfg.model_for.get("citation").map(String::as_str),
            Some("gemini-3-flash-preview")
        );
    }

    #[test]
    fn cli_model_for_beats_per_agent_model_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let cfg = with_clean_extractor_env(|| {
            with_clean_model_env(|| {
                std::env::set_var("GROKRXIV_CITATION_MODEL", "gemini-3-flash-preview");
                let mut over = RuntimeConfigOverrides::default();
                over.model_for
                    .push(("citation".to_string(), "gemini-3-pro-preview".to_string()));
                RuntimeConfig::resolve(
                    &over,
                    "nonexistent",
                    Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
                )
            })
        })
        .unwrap();

        assert_eq!(
            cfg.model_for.get("citation").map(String::as_str),
            Some("gemini-3-pro-preview")
        );
    }

    #[test]
    fn resolve_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        // No HOME-relative config, no env, no CLI overrides → default.
        // Force no_config by pointing config_path to a path that doesn't exist
        // and clearing relevant env vars within the test scope.
        let cfg = with_clean_extractor_env(|| {
            RuntimeConfig::resolve(
                &RuntimeConfigOverrides::default(),
                "nonexistent",
                Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
            )
        })
        .unwrap();
        assert_eq!(cfg.default_runner, AgentRunnerKind::Cli);
        assert_eq!(cfg.extractor, ExtractorKind::Cli);
        assert_eq!(cfg.default_sandbox, SandboxPolicy::None);
    }

    #[test]
    fn resolve_cli_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut over = RuntimeConfigOverrides::default();
        over.runner = Some(AgentRunnerKind::Cli);
        over.extractor = Some(ExtractorKind::Api);
        over.no_cache = true;
        over.runner_for
            .push(("summary".to_string(), AgentRunnerKind::Cloud));
        let cfg = with_clean_extractor_env(|| {
            RuntimeConfig::resolve(
                &over,
                "default",
                Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
            )
        })
        .unwrap();
        assert_eq!(cfg.default_runner, AgentRunnerKind::Cli);
        assert_eq!(cfg.extractor, ExtractorKind::Api);
        assert!(cfg.no_cache);
        assert_eq!(
            cfg.runner_for.get("summary").copied(),
            Some(AgentRunnerKind::Cloud)
        );
    }

    #[test]
    fn provider_api_allowed_only_when_api_backend_selected() {
        let mut cfg = RuntimeConfig::default();
        assert!(!provider_api_allowed(&cfg));

        cfg.extractor = ExtractorKind::Api;
        assert!(provider_api_allowed(&cfg));

        cfg.extractor = ExtractorKind::Cli;
        cfg.default_runner = AgentRunnerKind::Api;
        assert!(provider_api_allowed(&cfg));

        cfg.default_runner = AgentRunnerKind::Cli;
        cfg.runner_for
            .insert("summary".to_string(), AgentRunnerKind::Api);
        assert!(provider_api_allowed(&cfg));
    }

    #[test]
    fn parse_extractor_accepts_only_cli_or_api() {
        assert_eq!(parse_extractor("cli"), Some(ExtractorKind::Cli));
        assert_eq!(parse_extractor("api"), Some(ExtractorKind::Api));
        assert_eq!(parse_extractor("cloud"), None);
        assert_eq!(parse_extractor("local_inference"), None);
    }

    #[test]
    fn invalid_extractor_env_errors_clearly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("AGENTHERO_EXTRACTOR").ok();
        std::env::set_var("AGENTHERO_EXTRACTOR", "cloud");
        let err = RuntimeConfig::resolve(
            &RuntimeConfigOverrides::default(),
            "default",
            Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
        )
        .unwrap_err();
        if let Some(prev) = prev {
            std::env::set_var("AGENTHERO_EXTRACTOR", prev);
        } else {
            std::env::remove_var("AGENTHERO_EXTRACTOR");
        }
        assert!(
            err.to_string().contains("AGENTHERO_EXTRACTOR"),
            "unexpected error: {err}"
        );
    }
}
