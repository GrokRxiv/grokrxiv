//! `agh doctor` — preflight checks for env, DB, runners, publisher.
//!
//! Returns a structured `DoctorReport` so the CLI can emit JSON or human text.
//! Critical checks (DB URL + at least one configured review runner) set the
//! exit code to 1.

use crate::agents::AgentRunnerKind;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Outcome of a single check.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Check succeeded.
    Ok,
    /// Check was skipped (dependency or env var not set).
    Skipped,
    /// Check failed.
    Fail,
}

/// One named check result.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// Status.
    pub status: CheckStatus,
    /// Human-readable message.
    pub message: String,
}

impl CheckResult {
    fn ok(msg: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Ok,
            message: msg.into(),
        }
    }
    fn skipped(msg: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Skipped,
            message: msg.into(),
        }
    }
    fn fail(msg: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            message: msg.into(),
        }
    }
}

/// Doctor's structured report.
#[derive(Debug, Default, Clone, Serialize)]
pub struct DoctorReport {
    /// Active config profile.
    pub profile: String,
    /// DATABASE_URL presence + parse check.
    pub database_url: Option<CheckResult>,
    /// Supabase migration applied check (best effort).
    pub supabase_migrations: Option<CheckResult>,
    /// Per-API runner key check.
    pub api_runners: ApiRunnerStatus,
    /// CLI runners (claude / codex / Gemini-family CLI).
    pub cli_runners: CliRunnerStatus,
    /// Publisher (GitHub) status.
    pub publisher: Option<CheckResult>,
    /// Frontend revalidate endpoint reachability.
    pub web_revalidate: Option<CheckResult>,
    /// HTML quality cleanup configuration.
    pub html_quality: Option<CheckResult>,
    /// Pandoc binary check (required for TeX→Markdown conversion).
    pub pandoc: Option<CheckResult>,
    /// LaTeXML binary check (optional; checked only when semantic AST is enabled).
    pub latexml: Option<CheckResult>,
    /// Local Supabase Storage REST endpoint reachability (Tier-2 writes).
    pub supabase_storage: Option<CheckResult>,
    /// `grokrxiv-data` Tier-1 Git repo presence + initialisation.
    pub data_repo: Option<CheckResult>,
    /// Per-extraction-agent YAML config parseability.
    pub extraction_agents: ExtractionAgentsStatus,
}

/// Per-provider API key + reachability.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ApiRunnerStatus {
    /// Anthropic.
    pub anthropic: Option<CheckResult>,
    /// OpenAI.
    pub openai: Option<CheckResult>,
    /// Gemini.
    pub gemini: Option<CheckResult>,
}

/// Local CLI binary detection.
#[derive(Debug, Default, Clone, Serialize)]
pub struct CliRunnerStatus {
    /// `claude` CLI on PATH.
    pub claude: Option<CheckResult>,
    /// `codex` CLI on PATH.
    pub codex: Option<CheckResult>,
    /// `gemini` CLI on PATH.
    pub gemini: Option<CheckResult>,
}

/// Per-extraction-agent YAML config parse outcome.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ExtractionAgentsStatus {
    /// `agents/extraction/vlm.yaml` parseability.
    pub vlm: Option<CheckResult>,
    /// `agents/extraction/macros.yaml` parseability.
    pub macros: Option<CheckResult>,
    /// `agents/extraction/equations.yaml` parseability.
    pub equations: Option<CheckResult>,
    /// `agents/extraction/theorems.yaml` parseability.
    pub theorems: Option<CheckResult>,
    /// `agents/extraction/citations.yaml` parseability.
    pub citations: Option<CheckResult>,
}

impl DoctorReport {
    /// Empty report seeded with the active profile name.
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            ..Default::default()
        }
    }

    /// True if a critical check failed (DB URL or no configured review runner).
    pub fn has_critical_failure(&self) -> bool {
        self.has_critical_failure_with_runner_requirement(doctor_requires_review_runner())
    }

    fn has_critical_failure_with_runner_requirement(&self, require_review_runner: bool) -> bool {
        let db_fail = matches!(
            self.database_url.as_ref().map(|c| c.status),
            Some(CheckStatus::Fail)
        );
        let any_api_ok = [
            &self.api_runners.anthropic,
            &self.api_runners.openai,
            &self.api_runners.gemini,
        ]
        .iter()
        .any(|c| matches!(c.as_ref().map(|c| c.status), Some(CheckStatus::Ok)));
        let any_cli_ok = [
            &self.cli_runners.claude,
            &self.cli_runners.codex,
            &self.cli_runners.gemini,
        ]
        .iter()
        .any(|c| matches!(c.as_ref().map(|c| c.status), Some(CheckStatus::Ok)));
        let required_cli_fail = [
            &self.cli_runners.claude,
            &self.cli_runners.codex,
            &self.cli_runners.gemini,
        ]
        .iter()
        .any(|c| {
            c.as_ref().is_some_and(|check| {
                check.status == CheckStatus::Fail
                    && check.message.contains("required by configured CLI role")
            })
        });
        db_fail || required_cli_fail || (require_review_runner && !(any_api_ok || any_cli_ok))
    }

    /// Pretty-print the report to stdout.
    pub fn print_human(&self) {
        println!("AgentHero doctor (profile: {})", self.profile);
        println!();
        println!("Database:");
        print_line("DATABASE_URL", self.database_url.as_ref());
        print_line("supabase_migrations", self.supabase_migrations.as_ref());
        println!();
        println!("API runners:");
        print_line("anthropic", self.api_runners.anthropic.as_ref());
        print_line("openai", self.api_runners.openai.as_ref());
        print_line("gemini", self.api_runners.gemini.as_ref());
        println!();
        println!("CLI runners:");
        print_line("claude", self.cli_runners.claude.as_ref());
        print_line("codex", self.cli_runners.codex.as_ref());
        print_line("gemini-cli", self.cli_runners.gemini.as_ref());
        println!();
        println!("Publisher:");
        print_line("github", self.publisher.as_ref());
        println!();
        println!("Refresh pipeline:");
        print_line("web_revalidate", self.web_revalidate.as_ref());
        print_line("html_quality", self.html_quality.as_ref());
        println!();
        println!("Document converters:");
        print_line("pandoc", self.pandoc.as_ref());
        print_line("latexml", self.latexml.as_ref());

        println!();
        println!("RPT3 storage pipeline:");
        print_line("supabase_storage", self.supabase_storage.as_ref());
        print_line("data_repo", self.data_repo.as_ref());
        print_line("extr/vlm", self.extraction_agents.vlm.as_ref());
        print_line("extr/macros", self.extraction_agents.macros.as_ref());
        print_line("extr/equations", self.extraction_agents.equations.as_ref());
        print_line("extr/theorems", self.extraction_agents.theorems.as_ref());
        print_line("extr/citations", self.extraction_agents.citations.as_ref());

        println!();
        if self.has_critical_failure() {
            println!("RESULT: FAIL (one or more critical checks failed)");
        } else {
            println!("RESULT: OK");
        }
    }
}

fn print_line(name: &str, r: Option<&CheckResult>) {
    match r {
        Some(c) => {
            let badge = match c.status {
                CheckStatus::Ok => " ok ",
                CheckStatus::Skipped => "skip",
                CheckStatus::Fail => "FAIL",
            };
            println!("  [{badge}] {name:<22} {}", c.message);
        }
        None => println!("  [skip] {name:<22} (not checked)"),
    }
}

/// Run all the preflight checks and emit a report.
pub async fn doctor(profile: &str, json: bool) -> anyhow::Result<i32> {
    let mut report = DoctorReport::new(profile);

    check_database(&mut report).await;
    check_api_runners(&mut report).await;
    check_cli_runners(&mut report);
    check_publisher(&mut report);
    check_refresh_pipeline(&mut report).await;
    check_doc_converters(&mut report);
    check_supabase_storage(&mut report).await;
    check_data_repo(&mut report);
    check_extraction_agent_yaml(&mut report);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        report.print_human();
    }
    Ok(if report.has_critical_failure() { 1 } else { 0 })
}

async fn check_database(report: &mut DoctorReport) {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        report.database_url = Some(CheckResult::fail("DATABASE_URL is unset"));
        return;
    };
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        report.database_url = Some(CheckResult::ok("DATABASE_URL set (postgres scheme)"));
    } else {
        report.database_url = Some(CheckResult::fail(format!(
            "DATABASE_URL has unexpected scheme: {}",
            url.split(':').next().unwrap_or("(none)")
        )));
    }
    // Migration check: best effort, not performed here to keep doctor offline.
    report.supabase_migrations = Some(CheckResult::skipped(
        "not checked (run `bash agenthero/apps/grokrxiv/infra/supabase/setup.sh`)",
    ));
}

async fn check_api_runners(report: &mut DoctorReport) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build();
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(err = %e, "doctor: could not build http client");
            return;
        }
    };

    // Anthropic
    report.api_runners.anthropic = Some(match nonblank_env("ANTHROPIC_API_KEY") {
        Some(_) => CheckResult::ok("ANTHROPIC_API_KEY set"),
        None => CheckResult::skipped("ANTHROPIC_API_KEY unset"),
    });
    // OpenAI
    report.api_runners.openai = Some(match nonblank_env("OPENAI_API_KEY") {
        Some(key) => match ping_openai(&client, &key).await {
            Ok(()) => CheckResult::ok("OPENAI_API_KEY reachable (/v1/models)"),
            Err(e) => CheckResult::fail(format!("OPENAI_API_KEY set but unreachable: {e}")),
        },
        None => CheckResult::skipped("OPENAI_API_KEY unset"),
    });
    // Gemini
    report.api_runners.gemini = Some(match nonblank_env("GOOGLE_GENERATIVE_AI_API_KEY") {
        Some(_) => CheckResult::ok("GOOGLE_GENERATIVE_AI_API_KEY set"),
        None => CheckResult::skipped("GOOGLE_GENERATIVE_AI_API_KEY unset"),
    });
}

async fn ping_openai(client: &reqwest::Client, key: &str) -> anyhow::Result<()> {
    let res = client
        .get("https://api.openai.com/v1/models")
        .header("authorization", format!("Bearer {key}"))
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("HTTP {}", res.status());
    }
    Ok(())
}

fn check_cli_runners(report: &mut DoctorReport) {
    let config = CliRunnerCheckConfig::from_env();
    check_cli_runners_with(report, &config);
}

struct CliRunnerCheckConfig {
    path: OsString,
    home: PathBuf,
    require_auth: bool,
    required_bins: BTreeSet<String>,
}

impl CliRunnerCheckConfig {
    fn from_env() -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/grokrxiv"));
        let cli_selected = std::env::var("AGENTHERO_RUNNER")
            .ok()
            .map(|v| v.eq_ignore_ascii_case("cli"))
            .unwrap_or(false)
            || std::env::var("AGENTHERO_EXTRACTOR")
                .ok()
                .map(|v| v.eq_ignore_ascii_case("cli"))
                .unwrap_or(false);
        Self {
            path,
            home,
            require_auth: env_truthy("GROKRXIV_REQUIRE_CLI_AUTH") || cli_selected,
            required_bins: required_cli_binaries_for_agents_dir(&review_agents_dir_from_env())
                .unwrap_or_else(|e| {
                    tracing::warn!(err = %e, "doctor: could not load configured CLI runner requirements");
                    BTreeSet::new()
                }),
        }
    }
}

fn check_cli_runners_with(report: &mut DoctorReport, config: &CliRunnerCheckConfig) {
    for (name, slot) in [
        ("claude", &mut report.cli_runners.claude),
        ("codex", &mut report.cli_runners.codex),
        ("gemini", &mut report.cli_runners.gemini),
    ] {
        let binary = cli_binary_for_cli_name(name);
        let required = config.require_auth
            || config.required_bins.contains(name)
            || config.required_bins.contains(&binary);
        *slot = Some(match binary_available_in_path(&binary, &config.path) {
            Some(p) => match cli_auth_status(name, &binary, &config.home) {
                Some(auth) => CheckResult::ok(format!("found {p}; auth {auth}")),
                None if config.require_auth => CheckResult::fail(format!(
                    "found {p}; auth missing in {}",
                    config.home.display()
                )),
                None => CheckResult::ok(format!("found {p}; auth not checked")),
            },
            None if required => CheckResult::fail(format!(
                "`{binary}` not on PATH (required by configured CLI role)"
            )),
            None => CheckResult::skipped(format!("`{binary}` not on PATH")),
        });
    }
}

fn which_in_path(bin: &str, path: &OsString) -> Option<String> {
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

/// Resolve the review-agent YAML directory using the same default as `AppState`.
pub(crate) fn review_agents_dir_from_env() -> PathBuf {
    std::env::var("AGENTHERO_AGENTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("agents")
        })
}

/// Return local CLI binaries required by `runner: cli` review-agent YAML.
pub(crate) fn required_cli_binaries_for_agents_dir(
    agents_dir: &Path,
) -> anyhow::Result<BTreeSet<String>> {
    #[derive(serde::Deserialize)]
    struct ReviewAgentCliConfig {
        provider: String,
        #[serde(default)]
        runner: Option<AgentRunnerKind>,
    }

    let mut required = BTreeSet::new();
    for path in yaml_files_under(agents_dir)? {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        let cfg: ReviewAgentCliConfig = serde_yaml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        if cfg.runner.unwrap_or_default() == AgentRunnerKind::Cli {
            required.insert(cli_binary_for_provider(&cfg.provider)?);
        }
    }
    Ok(required)
}

fn yaml_files_under(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_yaml_files(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_yaml_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("read agents directory {}: {e}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_yaml_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("yaml") {
            out.push(path);
        }
    }
    Ok(())
}

/// Map a review-agent provider tag to the local CLI binary it uses.
pub(crate) fn cli_binary_for_provider(provider: &str) -> anyhow::Result<String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "claude" | "anthropic" => Ok(cli_binary_for_cli_name("claude")),
        "openai" | "codex" => Ok(cli_binary_for_cli_name("codex")),
        "gemini" | "google" => Ok(cli_binary_for_cli_name("gemini")),
        other => anyhow::bail!("unsupported provider for CliRunner: {other}"),
    }
}

fn cli_binary_for_cli_name(name: &str) -> String {
    match name {
        "claude" => std::env::var("AGENTHERO_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
        "codex" => std::env::var("AGENTHERO_CODEX_BIN").unwrap_or_else(|_| "codex".to_string()),
        "gemini" => std::env::var("AGENTHERO_GEMINI_BIN").unwrap_or_else(|_| "gemini".to_string()),
        other => other.to_string(),
    }
}

/// Return where a binary resolves, supporting absolute/relative override paths.
pub(crate) fn binary_available_in_path(bin: &str, path: &OsString) -> Option<String> {
    let candidate = Path::new(bin);
    if candidate.is_absolute() || bin.contains(std::path::MAIN_SEPARATOR) {
        return candidate.is_file().then(|| candidate.display().to_string());
    }
    which_in_path(bin, path)
}

fn cli_auth_status(name: &str, _binary: &str, home: &Path) -> Option<&'static str> {
    match name {
        "claude" => {
            if home.join(".claude.json").is_file() {
                Some("present (.claude.json)")
            } else if home.join(".claude").is_dir() {
                Some("present (.claude)")
            } else {
                None
            }
        }
        "codex" => home
            .join(".codex")
            .join("auth.json")
            .is_file()
            .then_some("present (.codex/auth.json)"),
        "gemini" => home
            .join(".gemini")
            .join("oauth_creds.json")
            .is_file()
            .then_some("present (.gemini/oauth_creds.json)"),
        _ => None,
    }
}

fn check_publisher(report: &mut DoctorReport) {
    report.publisher = Some(match nonblank_env("GITHUB_TOKEN") {
        Some(_) => CheckResult::ok("GITHUB_TOKEN set (PR opens will be live)"),
        None => CheckResult::fail("GITHUB_TOKEN unset (approve requires a live PR token)"),
    });
}

struct RefreshPipelineConfig {
    web_revalidate_url: Option<String>,
    html_quality_disabled: bool,
    html_quality_model: String,
    html_quality_timeout_secs: u64,
    web_probe_timeout: Duration,
}

impl RefreshPipelineConfig {
    fn from_env() -> Self {
        Self {
            web_revalidate_url: nonblank_env("WEB_REVALIDATE_URL"),
            html_quality_disabled: env_truthy("GROKRXIV_HTML_QUALITY_DISABLE"),
            html_quality_model: nonblank_env("GROKRXIV_HTML_QUALITY_MODEL")
                .unwrap_or_else(|| "gpt-5.5".to_string()),
            html_quality_timeout_secs: std::env::var("GROKRXIV_HTML_QUALITY_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|secs| *secs > 0)
                .unwrap_or(180),
            web_probe_timeout: std::env::var("AGENTHERO_DOCTOR_WEB_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|secs| *secs > 0)
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(3)),
        }
    }
}

async fn check_refresh_pipeline(report: &mut DoctorReport) {
    let config = RefreshPipelineConfig::from_env();
    check_refresh_pipeline_with(report, &config).await;
}

async fn check_refresh_pipeline_with(report: &mut DoctorReport, config: &RefreshPipelineConfig) {
    report.html_quality = Some(check_html_quality_with(config));
    let client = match reqwest::Client::builder()
        .timeout(config.web_probe_timeout)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            report.web_revalidate = Some(CheckResult::skipped(format!(
                "could not build http client: {e}"
            )));
            return;
        }
    };
    report.web_revalidate =
        Some(check_web_revalidate_with(&client, config.web_revalidate_url.as_deref()).await);
}

fn check_html_quality_with(config: &RefreshPipelineConfig) -> CheckResult {
    if config.html_quality_disabled {
        return CheckResult::skipped(
            "GROKRXIV_HTML_QUALITY_DISABLE set; HTML quality cleanup disabled",
        );
    }
    CheckResult::ok(format!(
        "enabled model={} timeout_secs={}",
        config.html_quality_model, config.html_quality_timeout_secs
    ))
}

async fn check_web_revalidate_with(client: &reqwest::Client, url: Option<&str>) -> CheckResult {
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return CheckResult::skipped("WEB_REVALIDATE_URL unset");
    };
    match client.head(url).send().await {
        Ok(r) if r.status().as_u16() < 500 => CheckResult::ok(format!(
            "{url} reachable via HEAD (HTTP {}; POST auth not checked)",
            r.status()
        )),
        Ok(r) => CheckResult::fail(format!("{url} returned HTTP {}", r.status())),
        Err(e) if e.is_timeout() => {
            CheckResult::fail(format!("{url} timed out during HEAD probe: {e}"))
        }
        Err(e) if e.is_connect() => CheckResult::fail(format!("{url} unreachable: {e}")),
        Err(e) => CheckResult::fail(format!("{url} probe failed: {e}")),
    }
}

fn nonblank_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn doctor_requires_review_runner() -> bool {
    !matches!(
        std::env::var("GROKRXIV_DOCTOR_REQUIRE_RUNNER").as_deref(),
        Ok("0") | Ok("false") | Ok("no") | Ok("off")
    )
}

struct DocConverterConfig {
    pandoc_bin: String,
    latexml_bin: String,
    latexmlpost_bin: String,
    enable_latexml: bool,
    disable_latexml: bool,
}

impl DocConverterConfig {
    fn from_env() -> Self {
        Self {
            pandoc_bin: std::env::var("GROKRXIV_PANDOC_BIN")
                .unwrap_or_else(|_| "pandoc".to_string()),
            latexml_bin: std::env::var("GROKRXIV_LATEXML_BIN")
                .unwrap_or_else(|_| "latexml".to_string()),
            latexmlpost_bin: std::env::var("GROKRXIV_LATEXMLPOST_BIN")
                .unwrap_or_else(|_| "latexmlpost".to_string()),
            enable_latexml: env_truthy("GROKRXIV_TEX_ENABLE_LATEXML"),
            disable_latexml: env_truthy("GROKRXIV_TEX_DISABLE_LATEXML"),
        }
    }
}

/// Pandoc + optional LaTeXML reachability. Pandoc is required (FAIL if <3.0 or
/// missing); LaTeXML is checked only when semantic AST enrichment is explicitly
/// enabled.
fn check_doc_converters(report: &mut DoctorReport) {
    let config = DocConverterConfig::from_env();
    check_doc_converters_with(report, &config);
}

fn check_doc_converters_with(report: &mut DoctorReport, config: &DocConverterConfig) {
    report.pandoc = Some(match run_version(&config.pandoc_bin) {
        Ok(out) => match parse_pandoc_major(&out) {
            Some(major) if major >= 3 => {
                CheckResult::ok(format!("pandoc {} (>= 3.0)", first_version_token(&out)))
            }
            Some(major) => CheckResult::fail(format!(
                "pandoc major version {major} < 3.0 (got: {})",
                first_version_token(&out)
            )),
            None => CheckResult::fail(format!(
                "could not parse pandoc version from: {}",
                out.lines().next().unwrap_or("")
            )),
        },
        Err(e) => CheckResult::fail(format!("`{} --version` failed: {e}", config.pandoc_bin)),
    });

    if config.disable_latexml {
        report.latexml = Some(CheckResult::skipped(
            "GROKRXIV_TEX_DISABLE_LATEXML set; LaTeXML semantic AST enrichment disabled",
        ));
        return;
    }
    if !config.enable_latexml {
        report.latexml = Some(CheckResult::skipped(
            "GROKRXIV_TEX_ENABLE_LATEXML unset; LaTeXML semantic AST enrichment disabled",
        ));
        return;
    }

    let latexml = match run_version(&config.latexml_bin) {
        Ok(out) => first_version_token(&out),
        Err(e) => {
            report.latexml = Some(CheckResult::fail(format!(
                "`{} --version` failed while GROKRXIV_TEX_ENABLE_LATEXML=1: {e}",
                config.latexml_bin
            )));
            return;
        }
    };
    let latexmlpost = match run_version(&config.latexmlpost_bin) {
        Ok(out) => first_version_token(&out),
        Err(e) => {
            report.latexml = Some(CheckResult::fail(format!(
                "`{} --version` failed while GROKRXIV_TEX_ENABLE_LATEXML=1: {e}",
                config.latexmlpost_bin
            )));
            return;
        }
    };
    report.latexml = Some(CheckResult::ok(format!(
        "latexml {latexml}; latexmlpost {latexmlpost}"
    )));
}

fn run_version(bin: &str) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !out.status.success() {
        anyhow::bail!("exit status {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn parse_pandoc_major(version_output: &str) -> Option<u32> {
    let first = version_output.lines().next()?;
    for tok in first.split_whitespace() {
        if let Some(major) = tok.split('.').next().and_then(|s| s.parse::<u32>().ok()) {
            return Some(major);
        }
    }
    None
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

fn first_version_token(version_output: &str) -> String {
    let first = version_output.lines().next().unwrap_or("");
    for tok in first.split_whitespace() {
        if tok
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .is_some()
        {
            return tok.to_string();
        }
    }
    first.to_string()
}

/// Local Supabase Storage REST endpoint reachability — pings
/// `<SUPABASE_URL>/storage/v1/health` (returns 200 when the local stack is
/// up). When `SUPABASE_URL` is unset we default to the standard local stack
/// at `http://127.0.0.1:54321` and try anyway, since the developer-facing
/// instruction is `supabase start`.
async fn check_supabase_storage(report: &mut DoctorReport) {
    let url =
        std::env::var("SUPABASE_URL").unwrap_or_else(|_| "http://127.0.0.1:54321".to_string());
    let target = format!("{}/storage/v1/health", url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            report.supabase_storage = Some(CheckResult::skipped("could not build http client"));
            return;
        }
    };
    report.supabase_storage = Some(match client.get(&target).send().await {
        // Any HTTP response (including 4xx) proves the storage process is up
        // and answering on the REST port; we only fail on 5xx or a transport
        // error.
        Ok(r) if r.status().as_u16() < 500 => {
            CheckResult::ok(format!("{target} reachable (HTTP {})", r.status()))
        }
        Ok(r) => CheckResult::fail(format!("{target} returned HTTP {}", r.status())),
        Err(e) => CheckResult::skipped(format!("{target} unreachable: {e} (try `supabase start`)")),
    });
}

/// `grokrxiv-data` Tier-1 Git repo presence + initialisation. The repo path
/// is picked from `GROKRXIV_DATA_REPO_PATH` with the operator's canonical
/// `~/Documents/Development/grokrxiv-data` as the default.
fn check_data_repo(report: &mut DoctorReport) {
    let path = std::env::var("GROKRXIV_DATA_REPO_PATH")
        .unwrap_or_else(|_| "/Users/mlong/Documents/Development/grokrxiv-data".to_string());
    let p = std::path::Path::new(&path);
    if !p.exists() {
        report.data_repo = Some(CheckResult::fail(format!(
            "{path} does not exist (clone GrokRxiv/grokrxiv-data here, or set GROKRXIV_DATA_REPO_PATH)"
        )));
        return;
    }
    let git_dir = p.join(".git");
    if !git_dir.exists() {
        report.data_repo = Some(CheckResult::fail(format!(
            "{path} exists but is not a Git repository (.git missing)"
        )));
        return;
    }
    let schemas_dir = p.join("schemas");
    let schema_note = if schemas_dir.is_dir() {
        " (schemas/ present)"
    } else {
        " (schemas/ missing — JSON validation will be a no-op)"
    };
    report.data_repo = Some(CheckResult::ok(format!("{path} ready{schema_note}")));
}

struct ExtractionAgentConfig {
    agents_dir: std::path::PathBuf,
}

impl ExtractionAgentConfig {
    fn from_env() -> Self {
        let agents_dir = std::env::var("AGENTHERO_AGENTS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("agents"));
        Self { agents_dir }
    }
}

/// Per-extraction-agent YAML config parseability. Reads
/// `extraction/<role>.yaml` from `AGENTHERO_AGENTS_DIR` or the default
/// `agents/` directory, matching the runtime extraction loader. Per-role
/// missing files map to `skipped` (Wave 2 may not have shipped one for every
/// role at audit time).
fn check_extraction_agent_yaml(report: &mut DoctorReport) {
    let config = ExtractionAgentConfig::from_env();
    check_extraction_agent_yaml_with(report, &config);
}

fn check_extraction_agent_yaml_with(report: &mut DoctorReport, config: &ExtractionAgentConfig) {
    fn check_one(config: &ExtractionAgentConfig, role: &str) -> CheckResult {
        let candidate = config
            .agents_dir
            .join("extraction")
            .join(format!("{role}.yaml"));
        if !candidate.exists() {
            return CheckResult::skipped(format!("{} not found", candidate.display()));
        }
        match std::fs::read_to_string(&candidate) {
            Ok(text) => match serde_yaml::from_str::<crate::agents::config::AgentConfig>(&text) {
                Ok(agent_config) => {
                    match crate::agents::config::validate_agent_config_detail(
                        role,
                        &agenthero_dag_runtime::AgentKind::Extractor,
                        &agent_config,
                    ) {
                        Ok(()) => {
                            CheckResult::ok(format!("{} parses and validates", candidate.display()))
                        }
                        Err(e) => CheckResult::fail(format!("{}: {e}", candidate.display())),
                    }
                }
                Err(e) => CheckResult::fail(format!("{}: {e}", candidate.display())),
            },
            Err(e) => CheckResult::fail(format!("{}: {e}", candidate.display())),
        }
    }

    report.extraction_agents.vlm = Some(check_one(config, "vlm"));
    report.extraction_agents.macros = Some(check_one(config, "macros"));
    report.extraction_agents.equations = Some(check_one(config, "equations"));
    report.extraction_agents.theorems = Some(check_one(config, "theorems"));
    report.extraction_agents.citations = Some(check_one(config, "citations"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

        fn clear(keys: &[&'static str]) -> Self {
            let saved = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in keys {
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

    #[cfg(unix)]
    fn fake_bin(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.path().join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "{body}").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.display().to_string()
    }

    #[test]
    fn nonblank_env_treats_blank_as_unset() {
        std::env::set_var("GROKRXIV_DOCTOR_TEST_BLANK", "   ");
        std::env::set_var("GROKRXIV_DOCTOR_TEST_SET", " value ");
        assert_eq!(nonblank_env("GROKRXIV_DOCTOR_TEST_BLANK"), None);
        assert_eq!(
            nonblank_env("GROKRXIV_DOCTOR_TEST_SET").as_deref(),
            Some("value")
        );
        std::env::remove_var("GROKRXIV_DOCTOR_TEST_BLANK");
        std::env::remove_var("GROKRXIV_DOCTOR_TEST_SET");
    }

    #[test]
    fn html_quality_doctor_reports_enabled_config_without_running_llm() {
        let config = RefreshPipelineConfig {
            web_revalidate_url: None,
            html_quality_disabled: false,
            html_quality_model: "gemini-3-flash-preview".to_string(),
            html_quality_timeout_secs: 42,
            web_probe_timeout: Duration::from_millis(50),
        };

        let result = check_html_quality_with(&config);

        assert_eq!(result.status, CheckStatus::Ok);
        assert!(
            result.message.contains("gemini-3-flash-preview"),
            "{}",
            result.message
        );
        assert!(result.message.contains("timeout_secs=42"));
    }

    #[tokio::test]
    async fn web_revalidate_doctor_reports_stopped_local_web() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();

        let result = check_web_revalidate_with(
            &client,
            Some(&format!("http://127.0.0.1:{port}/api/revalidate")),
        )
        .await;

        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.message.contains("unreachable"), "{}", result.message);
    }

    #[tokio::test]
    async fn web_revalidate_doctor_treats_http_response_as_reachable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/api/revalidate"))
            .respond_with(ResponseTemplate::new(405))
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();

        let result =
            check_web_revalidate_with(&client, Some(&format!("{}/api/revalidate", server.uri())))
                .await;

        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("HTTP 405"), "{}", result.message);
    }

    #[test]
    fn doctor_requires_runner_by_default_but_healthcheck_can_skip_it() {
        let mut report = DoctorReport::new("test");
        report.database_url = Some(CheckResult::ok("db"));

        assert!(report.has_critical_failure_with_runner_requirement(true));
        assert!(!report.has_critical_failure_with_runner_requirement(false));
    }

    #[test]
    fn doctor_treats_configured_cli_binary_failure_as_critical() {
        let mut report = DoctorReport::new("test");
        report.database_url = Some(CheckResult::ok("db"));
        report.api_runners.openai = Some(CheckResult::ok("OPENAI_API_KEY set"));
        report.cli_runners.claude = Some(CheckResult::fail(
            "`claude` not on PATH (required by configured CLI role)",
        ));

        assert!(report.has_critical_failure_with_runner_requirement(true));
    }

    #[test]
    fn doctor_extraction_agents_honor_configured_agents_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let extraction = tmp.path().join("extraction");
        std::fs::create_dir_all(&extraction).unwrap();
        for (role, schema) in [
            ("vlm", "vlm"),
            ("macros", "macros"),
            ("equations", "equations"),
            ("theorems", "theorems"),
            ("citations", "citations"),
        ] {
            std::fs::write(
                extraction.join(format!("{role}.yaml")),
                format!(
                    r#"
kind: extractor
provider: gemini
model: gemini-2.5-flash
runner: cli
execution_mode: tool_loop
prompt_template: prompts/extraction/{role}.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/extraction/{schema}.schema.json
tools: [submit]
max_iters: 1
max_cost_usd: 0.01
"#
                ),
            )
            .unwrap();
        }

        let config = ExtractionAgentConfig {
            agents_dir: tmp.path().to_path_buf(),
        };
        let mut report = DoctorReport::new("test");
        check_extraction_agent_yaml_with(&mut report, &config);

        assert_eq!(
            report.extraction_agents.vlm.as_ref().map(|c| c.status),
            Some(CheckStatus::Ok)
        );
        assert_eq!(
            report
                .extraction_agents
                .citations
                .as_ref()
                .map(|c| c.status),
            Some(CheckStatus::Ok)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_runner_check_fails_when_cli_mode_requires_missing_auth() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let claude = fake_bin(&tmp, "claude", "echo '2.1.144 (Claude Code)'");
        std::fs::rename(&claude, bin_dir.join("claude")).unwrap();

        let config = CliRunnerCheckConfig {
            path: std::env::join_paths([bin_dir.as_path()]).unwrap(),
            home: tmp.path().join("home"),
            require_auth: true,
            required_bins: BTreeSet::new(),
        };
        let mut report = DoctorReport::new("test");

        check_cli_runners_with(&mut report, &config);

        let result = report.cli_runners.claude.expect("claude check");
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(
            result.message.contains("auth missing"),
            "{}",
            result.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_binary_for_provider_uses_gemini_cli() {
        let _clear = EnvVarGuard::clear(&["AGENTHERO_GEMINI_BIN"]);

        assert_eq!(cli_binary_for_provider("gemini").unwrap(), "gemini");
        assert_eq!(cli_binary_for_cli_name("gemini"), "gemini");

        let _set = EnvVarGuard::set(&[("AGENTHERO_GEMINI_BIN", "/opt/bin/gemini")]);
        assert_eq!(
            cli_binary_for_provider("gemini").unwrap(),
            "/opt/bin/gemini"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_binary_for_provider_rejects_unsupported_providers() {
        let err = cli_binary_for_provider("not-a-provider").unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported provider for CliRunner"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_runner_check_accepts_binary_and_auth_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        for name in ["claude", "codex", "gemini"] {
            let bin = fake_bin(&tmp, name, &format!("echo '{name} ok'"));
            std::fs::rename(&bin, bin_dir.join(name)).unwrap();
        }
        std::fs::write(home.join(".claude.json"), "{}").unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex").join("auth.json"), "{}").unwrap();
        std::fs::create_dir_all(home.join(".gemini")).unwrap();
        std::fs::write(home.join(".gemini").join("oauth_creds.json"), "{}").unwrap();

        let config = CliRunnerCheckConfig {
            path: std::env::join_paths([bin_dir.as_path()]).unwrap(),
            home,
            require_auth: true,
            required_bins: BTreeSet::new(),
        };
        let mut report = DoctorReport::new("test");

        check_cli_runners_with(&mut report, &config);

        assert_eq!(
            report.cli_runners.claude.as_ref().map(|c| c.status),
            Some(CheckStatus::Ok)
        );
        assert_eq!(
            report.cli_runners.codex.as_ref().map(|c| c.status),
            Some(CheckStatus::Ok)
        );
        assert_eq!(
            report.cli_runners.gemini.as_ref().map(|c| c.status),
            Some(CheckStatus::Ok)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cli_runner_check_fails_only_configured_missing_cli_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let config = CliRunnerCheckConfig {
            path: std::ffi::OsString::new(),
            home: tmp.path().join("home"),
            require_auth: false,
            required_bins: BTreeSet::from(["claude".to_string()]),
        };
        let mut report = DoctorReport::new("test");

        check_cli_runners_with(&mut report, &config);

        assert_eq!(
            report.cli_runners.claude.as_ref().map(|c| c.status),
            Some(CheckStatus::Fail)
        );
        assert_eq!(
            report.cli_runners.codex.as_ref().map(|c| c.status),
            Some(CheckStatus::Skipped)
        );
        assert_eq!(
            report.cli_runners.gemini.as_ref().map(|c| c.status),
            Some(CheckStatus::Skipped)
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_skips_latexml_when_semantic_ast_is_not_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = DocConverterConfig {
            pandoc_bin: fake_bin(&tmp, "pandoc", "echo 'pandoc 3.9.0.2'"),
            latexml_bin: tmp.path().join("missing-latexml").display().to_string(),
            latexmlpost_bin: tmp.path().join("missing-latexmlpost").display().to_string(),
            enable_latexml: false,
            disable_latexml: false,
        };

        let mut report = DoctorReport::new("test");
        check_doc_converters_with(&mut report, &config);

        assert_eq!(
            report.pandoc.as_ref().map(|c| c.status),
            Some(CheckStatus::Ok)
        );
        let latexml = report.latexml.expect("latexml check");
        assert_eq!(latexml.status, CheckStatus::Skipped);
        assert!(
            latexml.message.contains("GROKRXIV_TEX_ENABLE_LATEXML"),
            "unexpected message: {}",
            latexml.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_requires_latexmlpost_when_semantic_ast_is_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let config = DocConverterConfig {
            pandoc_bin: fake_bin(&tmp, "pandoc", "echo 'pandoc 3.9.0.2'"),
            latexml_bin: fake_bin(&tmp, "latexml", "echo 'latexml 0.8.8'"),
            latexmlpost_bin: tmp.path().join("missing-latexmlpost").display().to_string(),
            enable_latexml: true,
            disable_latexml: false,
        };

        let mut report = DoctorReport::new("test");
        check_doc_converters_with(&mut report, &config);

        let latexml = report.latexml.expect("latexml check");
        assert_eq!(latexml.status, CheckStatus::Fail);
        assert!(
            latexml.message.contains("latexmlpost"),
            "unexpected message: {}",
            latexml.message
        );
    }
}
