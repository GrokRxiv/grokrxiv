//! Layered runtime configuration: CLI flags > ENV > TOML file > defaults.
//!
//! `Config` (in `config.rs`) is the legacy env-only config the supervisor
//! still consumes. `RuntimeConfig` here is the new per-invocation config the
//! CLI assembles from layered sources for the `grokrxiv` binary's operator
//! surface (RPT2 Track I). It augments — does not replace — `Config`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use grokrxiv_schemas::AgentRole;
use serde::Deserialize;

use crate::agents::{AgentMode, AgentRunnerKind, RevisionTarget, SandboxPolicy};

/// Layered runtime configuration consumed by the CLI before it builds the
/// `AppState` / agent registry. CLI flags override env vars, which override
/// the TOML file, which overrides the built-in defaults.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Default runner backend (API / CLI / cloud / local-inference).
    pub default_runner: AgentRunnerKind,
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
    pub runner_for: HashMap<AgentRole, AgentRunnerKind>,
    /// Per-role sandbox override.
    pub sandbox_for: HashMap<AgentRole, SandboxPolicy>,
    /// Per-role cloud-provider override.
    pub cloud_provider_for: HashMap<AgentRole, String>,
    /// Per-role model override (lands as `AgentSpec.model`).
    pub model_for: HashMap<AgentRole, String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_runner: AgentRunnerKind::Api,
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
        apply_env(&mut out);

        // ---- 4. CLI ----
        apply_cli(&mut out, cli_overrides);

        Ok(out)
    }
}

/// Subset of the parsed `Cli` whose values get layered onto a `RuntimeConfig`.
///
/// Holding it in its own struct avoids a cyclic dep between `cli.rs` and
/// `runtime_config.rs` while letting the CLI hand the values over verbatim.
#[derive(Debug, Default, Clone)]
pub struct RuntimeConfigOverrides {
    /// `--runner`.
    pub runner: Option<AgentRunnerKind>,
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
    pub runner_for: Vec<(AgentRole, AgentRunnerKind)>,
    /// `--model-for <role>=<model>` pairs.
    pub model_for: Vec<(AgentRole, String)>,
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

fn apply_env(out: &mut RuntimeConfig) {
    if let Ok(v) = std::env::var("GROKRXIV_RUNNER") {
        if let Some(r) = parse_runner(&v) {
            out.default_runner = r;
        }
    }
    if let Ok(v) = std::env::var("GROKRXIV_SANDBOX") {
        if let Some(r) = parse_sandbox(&v) {
            out.default_sandbox = r;
        }
    }
    if let Ok(v) = std::env::var("GROKRXIV_MODE") {
        if let Some(r) = parse_mode(&v) {
            out.default_mode = r;
        }
    }
    if let Ok(v) = std::env::var("GROKRXIV_CLOUD_PROVIDER") {
        out.cloud_provider = Some(v);
    }
    if let Ok(v) = std::env::var("GROKRXIV_LITELLM_URL") {
        out.litellm_url = Some(v);
    }
    if let Ok(v) = std::env::var("OLLAMA_HOST") {
        out.ollama_host = Some(v);
    }
    if let Ok(v) = std::env::var("GROKRXIV_SERVICE_TOKEN") {
        out.service_token = Some(v);
    }
    if let Ok(v) = std::env::var("GROKRXIV_MAX_COST_USD") {
        if let Ok(parsed) = v.parse::<f64>() {
            out.max_cost_usd = Some(parsed);
        }
    }
    if matches!(std::env::var("GROKRXIV_NO_CACHE").as_deref(), Ok("1") | Ok("true")) {
        out.no_cache = true;
    }
    if matches!(std::env::var("GROKRXIV_OFFLINE").as_deref(), Ok("1") | Ok("true")) {
        out.offline = true;
    }
}

fn apply_cli(out: &mut RuntimeConfig, cli: &RuntimeConfigOverrides) {
    if let Some(r) = cli.runner {
        out.default_runner = r;
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
        out.runner_for.insert(*role, *runner);
    }
    for (role, model) in &cli.model_for {
        out.model_for.insert(*role, model.clone());
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

fn parse_role(s: &str) -> Option<AgentRole> {
    match s.trim().to_ascii_lowercase().as_str() {
        "summary" => Some(AgentRole::Summary),
        "technical_correctness" | "technical-correctness" | "technical" => {
            Some(AgentRole::TechnicalCorrectness)
        }
        "novelty" => Some(AgentRole::Novelty),
        "reproducibility" | "repro" => Some(AgentRole::Reproducibility),
        "citation" => Some(AgentRole::Citation),
        "meta_reviewer" | "meta-reviewer" | "meta" => Some(AgentRole::MetaReviewer),
        _ => None,
    }
}

/// Parse `<role>=<runner>` (used by `--runner-for`).
pub fn parse_role_runner(s: &str) -> Result<(AgentRole, AgentRunnerKind), String> {
    let (role_s, runner_s) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <role>=<runner>, got `{s}`"))?;
    let role =
        parse_role(role_s).ok_or_else(|| format!("unknown role `{role_s}` in `{s}`"))?;
    let runner = parse_runner(runner_s)
        .ok_or_else(|| format!("unknown runner `{runner_s}` in `{s}`"))?;
    Ok((role, runner))
}

/// Parse `<role>=<model-id>` (used by `--model-for`).
pub fn parse_role_model(s: &str) -> Result<(AgentRole, String), String> {
    let (role_s, model_s) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <role>=<model>, got `{s}`"))?;
    let role =
        parse_role(role_s).ok_or_else(|| format!("unknown role `{role_s}` in `{s}`"))?;
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

    #[test]
    fn parse_role_runner_ok() {
        let (role, runner) = parse_role_runner("summary=cli").unwrap();
        assert_eq!(role, AgentRole::Summary);
        assert_eq!(runner, AgentRunnerKind::Cli);
    }

    #[test]
    fn parse_role_runner_bad_role() {
        assert!(parse_role_runner("nope=cli").is_err());
    }

    #[test]
    fn parse_role_runner_bad_runner() {
        assert!(parse_role_runner("summary=jetpack").is_err());
    }

    #[test]
    fn parse_role_model_ok() {
        let (role, model) = parse_role_model("technical_correctness=claude-opus-4-7").unwrap();
        assert_eq!(role, AgentRole::TechnicalCorrectness);
        assert_eq!(model, "claude-opus-4-7");
    }

    #[test]
    fn resolve_defaults() {
        // No HOME-relative config, no env, no CLI overrides → default.
        // Force no_config by pointing config_path to a path that doesn't exist
        // and clearing relevant env vars within the test scope.
        let cfg = RuntimeConfig::resolve(
            &RuntimeConfigOverrides::default(),
            "nonexistent",
            Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
        )
        .unwrap();
        assert_eq!(cfg.default_runner, AgentRunnerKind::Api);
        assert_eq!(cfg.default_sandbox, SandboxPolicy::None);
    }

    #[test]
    fn resolve_cli_overrides() {
        let mut over = RuntimeConfigOverrides::default();
        over.runner = Some(AgentRunnerKind::Cli);
        over.no_cache = true;
        over.runner_for
            .push((AgentRole::Summary, AgentRunnerKind::Cloud));
        let cfg = RuntimeConfig::resolve(
            &over,
            "default",
            Some(Path::new("/tmp/grokrxiv-nonexistent-config-test.toml")),
        )
        .unwrap();
        assert_eq!(cfg.default_runner, AgentRunnerKind::Cli);
        assert!(cfg.no_cache);
        assert_eq!(
            cfg.runner_for.get(&AgentRole::Summary).copied(),
            Some(AgentRunnerKind::Cloud)
        );
    }
}
