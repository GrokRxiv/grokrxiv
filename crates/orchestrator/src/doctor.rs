//! `grokrxiv doctor` — preflight checks for env, DB, runners, publisher.
//!
//! Returns a structured `DoctorReport` so the CLI can emit JSON or human text.
//! Critical checks (DB URL + at least one API runner) set the exit code to 1.

use serde::Serialize;
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

impl DoctorReport {
    /// Empty report seeded with the active profile name.
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            ..Default::default()
        }
    }

    /// True if a critical check failed (DB URL or zero API runners).
    pub fn has_critical_failure(&self) -> bool {
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
        db_fail || !any_api_ok
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
    report.supabase_migrations =
        Some(CheckResult::skipped("not checked (run `just supabase`)"));
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
    report.api_runners.anthropic = Some(match std::env::var("ANTHROPIC_API_KEY") {
        Ok(_) => CheckResult::ok("ANTHROPIC_API_KEY set"),
        Err(_) => CheckResult::skipped("ANTHROPIC_API_KEY unset"),
    });
    // OpenAI
    report.api_runners.openai = Some(match std::env::var("OPENAI_API_KEY") {
        Ok(key) => match ping_openai(&client, &key).await {
            Ok(()) => CheckResult::ok("OPENAI_API_KEY reachable (/v1/models)"),
            Err(e) => CheckResult::fail(format!("OPENAI_API_KEY set but unreachable: {e}")),
        },
        Err(_) => CheckResult::skipped("OPENAI_API_KEY unset"),
    });
    // Gemini
    report.api_runners.gemini = Some(match std::env::var("GOOGLE_GENERATIVE_AI_API_KEY") {
        Ok(_) => CheckResult::ok("GOOGLE_GENERATIVE_AI_API_KEY set"),
        Err(_) => CheckResult::skipped("GOOGLE_GENERATIVE_AI_API_KEY unset"),
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
    report.local_inference.litellm = Some(match std::env::var("GROKRXIV_LITELLM_URL")
        .or_else(|_| std::env::var("LITELLM_URL"))
    {
        Ok(url) => match client.get(format!("{url}/health")).send().await {
            Ok(r) if r.status().is_success() => CheckResult::ok(format!("{url} reachable")),
            Ok(r) => CheckResult::fail(format!("{url} returned HTTP {}", r.status())),
            Err(e) => CheckResult::fail(format!("{url} unreachable: {e}")),
        },
        Err(_) => CheckResult::skipped("GROKRXIV_LITELLM_URL unset"),
    });
    report.local_inference.ollama = Some(match std::env::var("OLLAMA_HOST") {
        Ok(url) => {
            let normalized = if url.starts_with("http") {
                url.clone()
            } else {
                format!("http://{url}")
            };
            match client.get(format!("{normalized}/api/tags")).send().await {
                Ok(r) if r.status().is_success() => CheckResult::ok(format!("{normalized} reachable")),
                Ok(r) => CheckResult::fail(format!("{normalized} returned HTTP {}", r.status())),
                Err(e) => CheckResult::fail(format!("{normalized} unreachable: {e}")),
            }
        }
        Err(_) => CheckResult::skipped("OLLAMA_HOST unset"),
    });
}

fn check_publisher(report: &mut DoctorReport) {
    report.publisher = Some(match std::env::var("GITHUB_TOKEN") {
        Ok(_) => CheckResult::ok("GITHUB_TOKEN set (PR opens will be live)"),
        Err(_) => CheckResult::skipped("GITHUB_TOKEN unset (approve will simulate PRs)"),
    });
}
