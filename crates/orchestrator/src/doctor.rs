//! `grokrxiv doctor` — preflight checks for env, DB, runners, publisher.
//!
//! Returns a structured `DoctorReport` so the CLI can emit JSON or human text.
//! Critical checks (DB URL + at least one configured review runner) set the
//! exit code to 1.

use serde::Serialize;
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
    /// CLI runners (claude / codex / gemini).
    pub cli_runners: CliRunnerStatus,
    /// Cloud runner connectivity.
    pub cloud_runners: CloudRunnerStatus,
    /// Local inference reachability.
    pub local_inference: LocalInferenceStatus,
    /// Publisher (GitHub) status.
    pub publisher: Option<CheckResult>,
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

/// Cloud runner reachability.
#[derive(Debug, Default, Clone, Serialize)]
pub struct CloudRunnerStatus {
    /// Vercel Open Agents.
    pub vercel_open_agents: Option<CheckResult>,
    /// E2B sandbox.
    pub e2b: Option<CheckResult>,
}

/// Local inference reachability.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LocalInferenceStatus {
    /// LiteLLM gateway.
    pub litellm: Option<CheckResult>,
    /// Ollama.
    pub ollama: Option<CheckResult>,
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
        let any_cloud_ok = [
            &self.cloud_runners.vercel_open_agents,
            &self.cloud_runners.e2b,
        ]
        .iter()
        .any(|c| matches!(c.as_ref().map(|c| c.status), Some(CheckStatus::Ok)));
        let any_local_ok = [&self.local_inference.litellm, &self.local_inference.ollama]
            .iter()
            .any(|c| matches!(c.as_ref().map(|c| c.status), Some(CheckStatus::Ok)));
        db_fail
            || (require_review_runner
                && !(any_api_ok || any_cli_ok || any_cloud_ok || any_local_ok))
    }

    /// Pretty-print the report to stdout.
    pub fn print_human(&self) {
        println!("GrokRxiv doctor (profile: {})", self.profile);
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
        println!("Cloud runners:");
        print_line(
            "vercel_open_agents",
            self.cloud_runners.vercel_open_agents.as_ref(),
        );
        print_line("e2b", self.cloud_runners.e2b.as_ref());
        println!();
        println!("Local inference:");
        print_line("litellm", self.local_inference.litellm.as_ref());
        print_line("ollama", self.local_inference.ollama.as_ref());
        println!();
        println!("Publisher:");
        print_line("github", self.publisher.as_ref());
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
    check_cloud_runners(&mut report).await;
    check_local_inference(&mut report).await;
    check_publisher(&mut report);
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
    report.supabase_migrations = Some(CheckResult::skipped("not checked (run `just supabase`)"));
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
    for (name, slot) in [
        ("claude", &mut report.cli_runners.claude),
        ("codex", &mut report.cli_runners.codex),
        ("gemini", &mut report.cli_runners.gemini),
    ] {
        *slot = Some(match which(name) {
            Some(p) => CheckResult::ok(format!("found {p}")),
            None => CheckResult::skipped(format!("`{name}` not on PATH")),
        });
    }
}

fn which(bin: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    None
}

async fn check_cloud_runners(report: &mut DoctorReport) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    // Vercel Open Agents
    report.cloud_runners.vercel_open_agents = Some(match std::env::var("VERCEL_OPEN_AGENTS_URL") {
        Ok(url) => match client.get(format!("{url}/healthz")).send().await {
            Ok(r) if r.status().is_success() => CheckResult::ok(format!("{url} reachable")),
            Ok(r) => CheckResult::fail(format!("{url} returned HTTP {}", r.status())),
            Err(e) => CheckResult::fail(format!("{url} unreachable: {e}")),
        },
        Err(_) => CheckResult::skipped("VERCEL_OPEN_AGENTS_URL unset"),
    });
    // E2B
    report.cloud_runners.e2b = Some(match std::env::var("E2B_API_KEY") {
        Ok(_) => CheckResult::ok("E2B_API_KEY set"),
        Err(_) => CheckResult::skipped("E2B_API_KEY unset"),
    });
}

async fn check_local_inference(report: &mut DoctorReport) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    report.local_inference.litellm = Some(
        match std::env::var("GROKRXIV_LITELLM_URL").or_else(|_| std::env::var("LITELLM_URL")) {
            Ok(url) => match client.get(format!("{url}/health")).send().await {
                Ok(r) if r.status().is_success() => CheckResult::ok(format!("{url} reachable")),
                Ok(r) => CheckResult::fail(format!("{url} returned HTTP {}", r.status())),
                Err(e) => CheckResult::fail(format!("{url} unreachable: {e}")),
            },
            Err(_) => CheckResult::skipped("GROKRXIV_LITELLM_URL unset"),
        },
    );
    report.local_inference.ollama = Some(match std::env::var("OLLAMA_HOST") {
        Ok(url) => {
            let normalized = if url.starts_with("http") {
                url.clone()
            } else {
                format!("http://{url}")
            };
            match client.get(format!("{normalized}/api/tags")).send().await {
                Ok(r) if r.status().is_success() => {
                    CheckResult::ok(format!("{normalized} reachable"))
                }
                Ok(r) => CheckResult::fail(format!("{normalized} returned HTTP {}", r.status())),
                Err(e) => CheckResult::fail(format!("{normalized} unreachable: {e}")),
            }
        }
        Err(_) => CheckResult::skipped("OLLAMA_HOST unset"),
    });
}

fn check_publisher(report: &mut DoctorReport) {
    report.publisher = Some(match nonblank_env("GITHUB_TOKEN") {
        Some(_) => CheckResult::ok("GITHUB_TOKEN set (PR opens will be live)"),
        None => CheckResult::skipped("GITHUB_TOKEN unset (approve will simulate PRs)"),
    });
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
        let agents_dir = std::env::var("GROKRXIV_AGENTS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("agents"));
        Self { agents_dir }
    }
}

/// Per-extraction-agent YAML config parseability. Reads
/// `extraction/<role>.yaml` from `GROKRXIV_AGENTS_DIR` or the default
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
            Ok(text) => match serde_yaml::from_str::<serde_yaml::Value>(&text) {
                Ok(_) => CheckResult::ok(format!("{} parses", candidate.display())),
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
    fn doctor_requires_runner_by_default_but_healthcheck_can_skip_it() {
        let mut report = DoctorReport::new("test");
        report.database_url = Some(CheckResult::ok("db"));

        assert!(report.has_critical_failure_with_runner_requirement(true));
        assert!(!report.has_critical_failure_with_runner_requirement(false));
    }

    #[test]
    fn doctor_extraction_agents_honor_configured_agents_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let extraction = tmp.path().join("extraction");
        std::fs::create_dir_all(&extraction).unwrap();
        for role in ["vlm", "macros", "equations", "theorems", "citations"] {
            std::fs::write(extraction.join(format!("{role}.yaml")), "provider: cli\n").unwrap();
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
