//! `grokrxiv` CLI surface.
//!
//! The binary's `main()` dispatches to one of the subcommands below. Each
//! variant delegates to a small function so the library/HTTP path and the
//! CLI path call the same plumbing — no duplication.

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use grokrxiv_schemas::AgentRole;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::agents::{AgentMode, AgentRunnerKind, RevisionTarget, SandboxPolicy};
use crate::doctor as doctor_mod;
use crate::runtime_config::{
    parse_role_model, parse_role_runner, provider_api_allowed, render as render_runtime_config,
    ExtractorKind, RuntimeConfig, RuntimeConfigOverrides, ALLOW_PROVIDER_API_ENV,
};

type PaperListRow = (
    Uuid,
    String,
    String,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
    Option<String>,
    Option<String>,
    Option<chrono::DateTime<chrono::Utc>>,
);

/// GrokRxiv — agentic peer-review pipeline for arXiv.
#[derive(Debug, Parser)]
#[command(
    name = "grokrxiv",
    version,
    about = "GrokRxiv — agentic peer-review pipeline for arXiv",
    long_about = None,
)]
pub struct Cli {
    /// Subcommand to dispatch. Defaults to `Serve` when unset.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Whether agents should run in `review_only` mode (default) or
    /// `review_and_revise` mode (emit a `revision_artifact` alongside the
    /// usual review output). RPT2 Track F.
    #[arg(long, value_enum, global = true, default_value_t = AgentMode::ReviewOnly)]
    pub mode: AgentMode,

    /// When `--mode review_and_revise`, controls what gets patched: the
    /// paper's LaTeX source (`paper_latex`, default) or GrokRxiv's own
    /// review output (`grokrxiv_review_output`).
    #[arg(long, value_enum, global = true, default_value_t = RevisionTarget::PaperLatex)]
    pub revision_target: RevisionTarget,

    // ---------- RPT2 Track I global runner/sandbox/cost/profile flags ----------
    /// Default runner backend for all roles.
    #[arg(long, value_enum, global = true)]
    pub runner: Option<AgentRunnerKind>,
    /// Staged extraction backend used by `ingest` before review.
    #[arg(long, value_enum, global = true)]
    pub extractor: Option<ExtractorKind>,
    /// Per-role runner override, e.g. `--runner-for technical_correctness=cli`.
    /// Repeatable.
    #[arg(long, global = true, value_parser = parse_role_runner, value_name = "ROLE=RUNNER")]
    pub runner_for: Vec<(AgentRole, AgentRunnerKind)>,
    /// Sandbox policy applied to runners that support it.
    #[arg(long, value_enum, global = true)]
    pub sandbox: Option<SandboxPolicy>,
    /// Cloud agent provider (e.g. `vercel_open_agents`, `e2b`).
    #[arg(long, global = true)]
    pub cloud_provider: Option<String>,
    /// LiteLLM gateway URL (overrides env).
    #[arg(long, global = true)]
    pub litellm_url: Option<String>,
    /// Ollama host (overrides env).
    #[arg(long, global = true)]
    pub ollama_host: Option<String>,
    /// Per-role model override, e.g. `--model-for summary=claude-haiku-4-5`.
    /// Repeatable.
    #[arg(long, global = true, value_parser = parse_role_model, value_name = "ROLE=MODEL")]
    pub model_for: Vec<(AgentRole, String)>,
    /// Hard cap on total cost (USD) for one review.
    #[arg(long, global = true)]
    pub max_cost_usd: Option<f64>,
    /// Skip the review cache.
    #[arg(long, global = true)]
    pub no_cache: bool,
    /// Offline mode (disallow network where avoidable).
    #[arg(long, global = true)]
    pub offline: bool,
    /// Plan-only: print what would run but don't make LLM calls.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Emit JSON instead of human-readable text on commands that support it.
    #[arg(long, global = true)]
    pub json: bool,
    /// Emit short foreground progress lines to stderr.
    #[arg(long, global = true, conflicts_with = "no_status")]
    pub status: bool,
    /// Suppress foreground progress lines for background runs.
    #[arg(long, global = true)]
    pub no_status: bool,
    /// Named TOML profile to load.
    #[arg(long, global = true, default_value = "default")]
    pub profile: String,
    /// Path to the TOML config file. Defaults to `~/.grokrxiv/config.toml`.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// `config show` flag: print provider secrets in cleartext.
    #[arg(long, global = true)]
    pub show_secrets: bool,
    /// Track 8a: dump the rendered prompt for each role to
    /// `./debug-prompts/<arxiv_id>/<role>.md` after the review finishes.
    /// Exports `GROKRXIV_DEBUG_PROMPT_DIR` for the supervisor; the supervisor
    /// writes one file per role per paper. The directory is printed at the
    /// end of the run.
    #[arg(long, global = true)]
    pub debug_prompt: bool,
    /// RPT3 Wave-3 Team-F: skip selected extraction stages. Comma-separated
    /// names from `{vlm, macros, equations, theorems, citations}`. Each
    /// skipped stage produces a `status: "skipped"` entry in
    /// `extraction_report.json`. Exported as `GROKRXIV_INGEST_SKIP_STAGES`.
    #[arg(long, global = true, value_name = "STAGES")]
    pub skip_stages: Option<String>,
    /// RPT3 Wave-3 Team-F: skip Tier-2 (Supabase) writes even when
    /// `SUPABASE_URL` + `SUPABASE_SERVICE_ROLE_KEY` are set. Tier-1 (the
    /// local grokrxiv-data clone) is still written. Exported as
    /// `GROKRXIV_DRY_RUN_STORAGE=1`.
    #[arg(long, global = true)]
    pub dry_run_storage: bool,
}

/// Hint for `grokrxiv review <source>` when the source can't be inferred.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum SourceType {
    /// arXiv id or URL.
    Arxiv,
    /// Local PDF file.
    Pdf,
    /// Local LaTeX (.tex) file.
    Tex,
    /// Mixed bundle / unknown.
    Mixed,
}

/// Top-level CLI subcommand variants.
#[derive(Debug, Subcommand)]
pub enum Command {
    // ---------- service ----------
    /// Run the HTTP API + tokio supervisor + scheduler (default).
    Serve,
    /// Print which env vars / external deps / DB / LLM providers are reachable.
    Doctor,
    /// Print the resolved orchestrator config (legacy env-only view + the
    /// layered RuntimeConfig). Secrets are redacted unless `--show-secrets`.
    Config {
        /// Print provider secrets in cleartext instead of `***`.
        #[arg(long)]
        show_secrets: bool,
    },
    /// Apply pending Supabase migrations (idempotent).
    Migrate,
    /// Print ALL_CATEGORIES, DEFAULT_ACTIVE_CATEGORIES, and the active env diff.
    Categories,

    // ---------- ingestion ----------
    /// Synchronously ingest + review one or more papers.
    Ingest {
        /// arXiv IDs (e.g. `2605.12484`).
        #[arg(required = true)]
        arxiv_ids: Vec<String>,
    },
    /// Fetch + extract one or more papers, validate reviewer input, then stop before review.
    Extract {
        /// arXiv IDs (e.g. `2605.12484`).
        #[arg(required = true)]
        arxiv_ids: Vec<String>,
    },
    /// Bulk OAI-PMH backfill across an arXiv date range.
    IngestRange {
        /// Start of the date range (inclusive).
        #[arg(long)]
        from: chrono::NaiveDate,
        /// End of the date range (inclusive).
        #[arg(long)]
        to: chrono::NaiveDate,
        /// Comma-separated category set (defaults to DEFAULT_ACTIVE_CATEGORIES).
        #[arg(long)]
        categories: Option<String>,
        /// Skip the auto-review enqueue (metadata-only backfill).
        #[arg(long)]
        no_review: bool,
    },
    /// One-shot equivalent of the daily scheduler tick.
    IngestDaily,

    // ---------- review lifecycle ----------
    /// List reviews or papers.
    List {
        /// Whether to list reviews or papers.
        #[command(subcommand)]
        what: ListKind,
    },
    /// Pretty-print a review (meta + agents + verifier statuses).
    Show {
        /// UUID of the review to print.
        review_id: Uuid,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// The canonical end-to-end entry point.
    ///
    /// `source` can be:
    /// - an arXiv id (e.g. `2605.12484`),
    /// - an arXiv URL (`https://arxiv.org/abs/...` / `/pdf/...`),
    /// - a local PDF path (`./paper.pdf`),
    /// - a local LaTeX path (`./paper.tex`),
    /// - `-` to read from stdin (use `--type` to disambiguate kind),
    /// - `@<path>` to read a newline-delimited file of one source per line.
    Review {
        /// Source: arXiv id | URL | path | `-` | `@file`.
        source: String,
        /// Force the source kind when it can't be inferred (e.g. stdin).
        #[arg(long, value_enum)]
        r#type: Option<SourceType>,
    },
    /// Re-run the review DAG against an already-ingested paper.
    ReReview {
        /// UUID of the paper to re-review.
        paper_id: Uuid,
    },
    /// Run the review DAG for a paper that was already extracted.
    ReviewExtracted {
        /// Supersede any active review for this paper.
        #[arg(long)]
        force: bool,
        /// arXiv id, arXiv URL, or paper UUID from `grokrxiv list extracted`.
        source: String,
    },
    /// Re-run the verifier ladder against a review.
    Verify {
        /// UUID of the review to re-verify.
        review_id: Uuid,
    },
    /// Re-emit one or all artifacts for a persisted review.
    Render {
        /// UUID of the review to render.
        review_id: Uuid,
        /// Output artifact format.
        #[arg(long, value_enum, default_value = "html")]
        format: RenderFormat,
        /// Optional destination path; defaults to `artifacts/<review_id>/`.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },

    // ---------- moderation (admin) ----------
    /// Open the publication PR on `GrokRxiv/grokrxiv-reviews`.
    Approve {
        /// UUID of the review to approve for PR handoff. This does not merge or publish.
        review_id: Uuid,
    },
    /// Mark a review rejected; status stays `awaiting_moderation`.
    Reject {
        /// UUID of the review to reject.
        review_id: Uuid,
        /// Human-readable reason recorded on the moderation row.
        #[arg(long)]
        reason: String,
    },
    /// Request changes from the moderator queue.
    RequestChanges {
        /// UUID of the review awaiting changes.
        review_id: Uuid,
        /// Moderator notes recorded on the moderation row.
        #[arg(long)]
        notes: String,
    },
    /// Withdraw a published review (status → withdrawn; revalidates).
    Withdraw {
        /// UUID of the review to withdraw.
        review_id: Uuid,
        /// Reason recorded on the corrections row.
        #[arg(long)]
        reason: String,
    },
    /// Append a correction; status → corrected.
    Correct {
        /// UUID of the review being corrected.
        review_id: Uuid,
        /// Path to a Markdown file containing the correction rationale.
        #[arg(long, value_name = "PATH")]
        rationale_md: std::path::PathBuf,
    },

    // ---------- conveniences ----------
    /// Print (and on macOS, `open`) the canonical /reviews/<id> URL.
    Open {
        /// UUID of the review to open in the browser.
        review_id: Uuid,
    },
    /// Stream the jobs table tail.
    TailJobs {
        /// Optional `kind` filter (e.g. `Ingest`, `Review`).
        #[arg(long)]
        kind: Option<String>,
        /// Optional `state` filter (e.g. `running`, `failed`).
        #[arg(long)]
        state: Option<String>,
    },
}

/// Selector for `grokrxiv list`.
#[derive(Debug, Subcommand)]
pub enum ListKind {
    /// List reviews.
    Reviews {
        /// Optional review status filter (e.g. `awaiting_moderation`).
        #[arg(long = "review-status", visible_alias = "state")]
        review_status: Option<String>,
        /// Optional field filter (e.g. `cs.AI`).
        #[arg(long)]
        field: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// List papers.
    Papers {
        /// Optional field filter (e.g. `cs.AI`).
        #[arg(long)]
        field: Option<String>,
        /// Only show papers that already have at least one review.
        #[arg(long)]
        has_review: bool,
        /// Only show papers with successful extracted artifacts.
        #[arg(long)]
        extracted: bool,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// List papers with successful extracted artifacts.
    Extracted {
        /// Optional field filter (e.g. `cs.AI`).
        #[arg(long)]
        field: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Emit JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

/// Output format for `grokrxiv render`.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum RenderFormat {
    /// Self-contained HTML.
    Html,
    /// CommonMark Markdown.
    Md,
    /// LaTeX source.
    Tex,
    /// PDF (rendered from LaTeX).
    Pdf,
    /// Zip archive containing every other format.
    Zip,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Run the parsed CLI. Returns a process exit code.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let status_enabled =
        cli.status || (!cli.no_status && !cli.json && std::io::stderr().is_terminal());
    crate::cli_status::set_enabled(status_enabled);

    // Resolve layered runtime config once per invocation. The result is held
    // in scope for any subcommand that wants to consult it (today: `config`,
    // `doctor`, `review`). Tracks H1/H2 already thread role-level overrides
    // through the agent registry at boot; here we expose the surface, leaving
    // the registry composition for those tracks to consume.
    let overrides = RuntimeConfigOverrides {
        runner: cli.runner,
        extractor: cli.extractor,
        sandbox: cli.sandbox,
        mode: Some(cli.mode),
        revision_target: Some(cli.revision_target),
        cloud_provider: cli.cloud_provider.clone(),
        litellm_url: cli.litellm_url.clone(),
        ollama_host: cli.ollama_host.clone(),
        max_cost_usd: cli.max_cost_usd,
        no_cache: cli.no_cache,
        offline: cli.offline,
        runner_for: cli.runner_for.clone(),
        model_for: cli.model_for.clone(),
    };
    let runtime_cfg = match RuntimeConfig::resolve(&overrides, &cli.profile, cli.config.as_deref())
    {
        Ok(cfg) => Some(cfg),
        Err(e) if e.to_string().contains("GROKRXIV_EXTRACTOR") => return Err(e),
        Err(e) => {
            tracing::warn!(err = %e, "could not resolve layered runtime config");
            None
        }
    };
    // RPT2 G follow-up: export the resolved per-role runner choice into env vars
    // the supervisor reads in its agent resolver. This is how `--runner cli` /
    // `--runner-for technical_correctness=cli` actually overrides the YAML's
    // `runner:` field at runtime (the supervisor's `resolve_agent` checks
    // `GROKRXIV_RUNNER_OVERRIDE` / `GROKRXIV_RUNNER_OVERRIDE_<ROLE>` env vars).
    if let Some(rt) = runtime_cfg.as_ref() {
        // Always export `default_runner` so the supervisor can pick up the
        // CLI's `--runner` flag (the resolved RuntimeConfig already merges
        // CLI > ENV > TOML > default).
        let kind = rt.default_runner;
        if let Ok(s) = serde_json::to_string(&kind) {
            let bare = s.trim_matches('"');
            std::env::set_var("GROKRXIV_RUNNER_OVERRIDE", bare);
        }
        for (role, kind) in &rt.runner_for {
            let role_slug = match role {
                grokrxiv_schemas::AgentRole::Summary => "SUMMARY",
                grokrxiv_schemas::AgentRole::TechnicalCorrectness => "TECHNICAL_CORRECTNESS",
                grokrxiv_schemas::AgentRole::Novelty => "NOVELTY",
                grokrxiv_schemas::AgentRole::Reproducibility => "REPRODUCIBILITY",
                grokrxiv_schemas::AgentRole::Citation => "CITATION",
                grokrxiv_schemas::AgentRole::MetaReviewer => "META_REVIEWER",
            };
            if let Ok(s) = serde_json::to_string(kind) {
                let bare = s.trim_matches('"');
                std::env::set_var(format!("GROKRXIV_RUNNER_OVERRIDE_{role_slug}"), bare);
            }
        }
        std::env::set_var("GROKRXIV_EXTRACTOR", rt.extractor.as_str());
        std::env::set_var(
            ALLOW_PROVIDER_API_ENV,
            if provider_api_allowed(rt) { "1" } else { "0" },
        );
    } else {
        std::env::set_var(ALLOW_PROVIDER_API_ENV, "0");
    }
    let json = cli.json;
    let show_secrets = cli.show_secrets;
    let profile = cli.profile.clone();
    let dry_run = cli.dry_run;
    let debug_prompt = cli.debug_prompt;

    // Track 8a: when `--debug-prompt` is set, export the directory the
    // supervisor will dump rendered prompts to. Resolved before the env is
    // forked off into spawned tasks. Default is `./debug-prompts`.
    let debug_prompt_dir: Option<std::path::PathBuf> = if debug_prompt {
        let dir = std::path::PathBuf::from("debug-prompts");
        std::env::set_var("GROKRXIV_DEBUG_PROMPT_DIR", &dir);
        Some(dir)
    } else {
        std::env::remove_var("GROKRXIV_DEBUG_PROMPT_DIR");
        None
    };

    // RPT3 Wave-3 Team-F: forward `--no-cache` / `--skip-stages` /
    // `--dry-run-storage` to the staged ingest orchestrator via env vars.
    // (The supervisor reads these in `ingest_options_from_env`.)
    let no_cache_resolved = runtime_cfg
        .as_ref()
        .map(|rt| rt.no_cache)
        .unwrap_or(cli.no_cache);
    if no_cache_resolved {
        std::env::set_var("GROKRXIV_INGEST_NO_CACHE", "1");
        std::env::set_var("GROKRXIV_NO_CACHE", "1");
    } else {
        std::env::remove_var("GROKRXIV_INGEST_NO_CACHE");
        std::env::remove_var("GROKRXIV_NO_CACHE");
    }
    if let Some(stages) = cli.skip_stages.as_deref() {
        std::env::set_var("GROKRXIV_INGEST_SKIP_STAGES", stages);
    } else {
        std::env::remove_var("GROKRXIV_INGEST_SKIP_STAGES");
    }
    if cli.dry_run_storage {
        std::env::set_var("GROKRXIV_DRY_RUN_STORAGE", "1");
    } else if std::env::var("GROKRXIV_DRY_RUN_STORAGE").as_deref() != Ok("1") {
        std::env::remove_var("GROKRXIV_DRY_RUN_STORAGE");
    }

    let command = cli.command.unwrap_or(Command::Serve);
    let is_review_command = matches!(
        command,
        Command::Review { .. }
            | Command::Ingest { .. }
            | Command::ReReview { .. }
            | Command::IngestRange { .. }
            | Command::IngestDaily
    );

    let result = match command {
        Command::Serve => super::serve::run().await,
        Command::Doctor => {
            let code = doctor_mod::doctor(&profile, json).await?;
            if code != 0 {
                anyhow::bail!("doctor: one or more critical checks failed");
            }
            Ok(())
        }
        Command::Config {
            show_secrets: cmd_show,
        } => print_config(show_secrets || cmd_show, runtime_cfg.as_ref(), json),
        Command::Migrate => migrate().await,
        Command::Categories => print_categories(),
        Command::Ingest { arxiv_ids } => ingest_many(&arxiv_ids, json).await,
        Command::Extract { arxiv_ids } => extract_many(&arxiv_ids, json).await,
        Command::IngestRange {
            from,
            to,
            categories,
            no_review,
        } => ingest_range(from, to, categories, no_review).await,
        Command::IngestDaily => ingest_daily().await,
        Command::List { what } => list(what).await,
        Command::Show { review_id, json } => show(review_id, json).await,
        Command::Review { source, r#type } => review_source(&source, r#type, json, dry_run).await,
        Command::ReReview { paper_id } => review_paper(paper_id).await,
        Command::ReviewExtracted { source, force } => review_extracted(&source, force).await,
        Command::Verify { review_id } => verify(review_id).await,
        Command::Render {
            review_id,
            format,
            out,
        } => render(review_id, format, out).await,
        Command::Approve { review_id } => approve(review_id, json).await,
        Command::Reject { review_id, reason } => reject(review_id, &reason).await,
        Command::RequestChanges { review_id, notes } => request_changes(review_id, &notes).await,
        Command::Withdraw { review_id, reason } => withdraw(review_id, &reason).await,
        Command::Correct {
            review_id,
            rationale_md,
        } => correct(review_id, &rationale_md).await,
        Command::Open { review_id } => open_review(review_id),
        Command::TailJobs { kind, state } => tail_jobs(kind, state).await,
    };

    if let Some(dir) = debug_prompt_dir.as_ref() {
        if is_review_command {
            println!("debug_prompt_dir={}", dir.display());
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Subcommand implementations. Where the supporting plumbing already exists
// (serve, ingest one paper, approve) we wire through; the rest emit a clear
// "not yet implemented in stub build" message that points at the right task.
// ---------------------------------------------------------------------------

fn print_config(
    show_secrets: bool,
    runtime: Option<&RuntimeConfig>,
    json: bool,
) -> anyhow::Result<()> {
    let cfg = super::Config::from_env();
    if json {
        // Compact JSON: env-derived legacy fields + the resolved layered runtime
        // config (if it resolved). Secret values are redacted by `render_runtime_config`.
        let runtime_json: serde_json::Value = match runtime {
            Some(r) => serde_json::from_str(&render_runtime_config(r, true, show_secrets))
                .unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
        let env_redact = |key: &str| -> String {
            match std::env::var(key) {
                Ok(s) if show_secrets => s,
                Ok(_) => "***".to_string(),
                Err(_) => "<unset>".to_string(),
            }
        };
        let redact = |v: Option<&str>| -> String {
            match v {
                Some(s) if show_secrets => s.to_string(),
                Some(_) => "***".to_string(),
                None => "<unset>".to_string(),
            }
        };
        let payload = serde_json::json!({
            "bind": cfg.bind,
            "database_url": redact(cfg.database_url.as_deref()),
            "arxiv_user_agent": cfg.arxiv_user_agent,
            "admin_token": redact(cfg.admin_token.as_deref()),
            "github_webhook_secret": redact(cfg.github_webhook_secret.as_deref()),
            "web_revalidate_url": cfg.web_revalidate_url,
            "revalidate_secret": redact(cfg.revalidate_secret.as_deref()),
            "ANTHROPIC_API_KEY": env_redact("ANTHROPIC_API_KEY"),
            "OPENAI_API_KEY": env_redact("OPENAI_API_KEY"),
            "GOOGLE_GENERATIVE_AI_API_KEY": env_redact("GOOGLE_GENERATIVE_AI_API_KEY"),
            "VLLM_BASE_URL": std::env::var("VLLM_BASE_URL").unwrap_or_else(|_| "<unset>".to_string()),
            "runtime": runtime_json,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    let redact = |v: Option<&str>| -> String {
        match v {
            Some(s) if show_secrets => s.to_string(),
            Some(_) => "***".to_string(),
            None => "<unset>".to_string(),
        }
    };
    // Provider secrets aren't stored on Config (they're consumed by the
    // llm-adapter via env directly); we report their env presence separately.
    let env_redact = |key: &str| -> String {
        match std::env::var(key) {
            Ok(s) if show_secrets => s,
            Ok(_) => "***".to_string(),
            Err(_) => "<unset>".to_string(),
        }
    };
    println!("bind                  = {}", cfg.bind);
    println!(
        "database_url          = {}",
        redact(cfg.database_url.as_deref())
    );
    println!("arxiv_user_agent      = {}", cfg.arxiv_user_agent);
    println!(
        "admin_token           = {}",
        redact(cfg.admin_token.as_deref())
    );
    println!(
        "github_webhook_secret = {}",
        redact(cfg.github_webhook_secret.as_deref())
    );
    println!(
        "web_revalidate_url    = {}",
        cfg.web_revalidate_url.as_deref().unwrap_or("<unset>")
    );
    println!(
        "revalidate_secret     = {}",
        redact(cfg.revalidate_secret.as_deref())
    );
    println!(
        "ANTHROPIC_API_KEY     = {}",
        env_redact("ANTHROPIC_API_KEY")
    );
    println!("OPENAI_API_KEY        = {}", env_redact("OPENAI_API_KEY"));
    println!(
        "GOOGLE_GENERATIVE_AI_API_KEY = {}",
        env_redact("GOOGLE_GENERATIVE_AI_API_KEY")
    );
    println!(
        "VLLM_BASE_URL         = {}",
        std::env::var("VLLM_BASE_URL").unwrap_or_else(|_| "<unset>".to_string())
    );
    if let Some(r) = runtime {
        println!();
        println!("Runtime (layered config):");
        for line in render_runtime_config(r, false, show_secrets).lines() {
            println!("  {line}");
        }
    }
    Ok(())
}

async fn migrate() -> anyhow::Result<()> {
    eprintln!("`migrate` is not yet wired (use `bash infra/supabase/setup.sh`). See task #11.");
    Ok(())
}

fn print_categories() -> anyhow::Result<()> {
    // Reach into the ingest crate for the canonical lists when --features full
    // is on; otherwise mirror the scheduler's default.
    println!("DEFAULT_ACTIVE_CATEGORIES (MVP):");
    for c in super::scheduler::DEFAULT_ACTIVE_CATEGORIES {
        println!("  - {c}");
    }
    println!();
    println!("INGEST_CATEGORIES env override:");
    match std::env::var("INGEST_CATEGORIES") {
        Ok(v) => println!("  {v}"),
        Err(_) => println!("  <unset> (using DEFAULT_ACTIVE_CATEGORIES)"),
    }
    Ok(())
}

async fn ingest_many(arxiv_ids: &[String], json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());

    if arxiv_ids.len() <= 1 {
        // Single-paper path stays direct so the M1 smoke output shape is unchanged.
        for id in arxiv_ids {
            crate::cli_status::emit(format!(
                "paper {id}: fetch -> extract(vlm, macros, equations, theorems, citations) -> review(summary, technical_correctness, novelty, reproducibility, citation, meta_reviewer) -> verifier -> render -> moderation"
            ));
            let review_id =
                super::supervisor::run_one_paper_blocking(&supervisor, &state, id).await?;
            crate::cli_status::emit(format!(
                "paper {id}: review_id={review_id} awaiting human moderation"
            ));
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "arxiv_id": id,
                        "review_id": review_id,
                    }))?
                );
            } else {
                println!("arxiv_id={id} review_id={review_id}");
            }
        }
        return Ok(());
    }

    // Parallel path — fan out, then collect. arXiv fetches are serialised
    // through the in-process semaphore in `grokrxiv_ingest::download`; the
    // DAGs run concurrently. This is the "ingest in parallel" path the
    // RPT1 multi-paper test exercises.
    use futures::stream::{FuturesUnordered, StreamExt};
    let mut futures = FuturesUnordered::new();
    for id in arxiv_ids {
        let id = id.clone();
        crate::cli_status::emit(format!(
            "paper {id}: queued for fetch/extract/review pipeline"
        ));
        let supervisor = supervisor.clone();
        let state = state.clone();
        futures.push(async move {
            let result = super::supervisor::run_one_paper_blocking(&supervisor, &state, &id).await;
            (id, result)
        });
    }
    let mut errors: Vec<(String, anyhow::Error)> = Vec::new();
    let mut successes: Vec<serde_json::Value> = Vec::new();
    while let Some((id, result)) = futures.next().await {
        match result {
            Ok(review_id) => {
                crate::cli_status::emit(format!(
                    "paper {id}: review_id={review_id} awaiting human moderation"
                ));
                if json {
                    successes.push(serde_json::json!({
                        "arxiv_id": id,
                        "review_id": review_id,
                    }));
                } else {
                    println!("arxiv_id={id} review_id={review_id}");
                }
            }
            Err(e) => {
                eprintln!("arxiv_id={id} ERROR: {e:?}");
                errors.push((id, e));
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string(&successes)?);
    }
    if !errors.is_empty() {
        anyhow::bail!(
            "{} of {} papers failed to ingest",
            errors.len(),
            arxiv_ids.len()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct ExtractCommandOutput {
    arxiv_id: String,
    paper_id: Uuid,
    artifact_root: String,
    review_input: String,
    audit: ExtractionAudit,
}

#[derive(Debug, Clone, Serialize)]
struct ExtractionAudit {
    review_ready: bool,
    body_chars: usize,
    section_count: usize,
    equation_count: usize,
    citation_count: usize,
    citation_context_count: usize,
    theorem_node_count: usize,
    extraction_stage_count: usize,
    warnings: Vec<String>,
    failures: Vec<String>,
}

async fn extract_many(arxiv_ids: &[String], json: bool) -> anyhow::Result<()> {
    #[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
    {
        let config = super::Config::from_env();
        let state = super::AppState::from_config(config).await?;
        let opts = crate::ingest_pipeline::IngestOptions::from_env();
        let repo_root = data_repo_root();
        let mut outputs = Vec::with_capacity(arxiv_ids.len());
        let mut failures = Vec::new();

        for id in arxiv_ids {
            crate::cli_status::emit(format!(
                "extract {id}: fetch -> Pandoc parse -> local extraction -> artifact audit"
            ));
            let result = crate::ingest_pipeline::run_ingest_pipeline(&state, id, &opts).await?;
            let audit = audit_review_input_artifacts(
                &repo_root,
                Some(&result.pointer.git_path),
                &result.review_input,
            )
            .with_context(|| format!("audit extracted artifacts for {id}"))?;
            let artifact_root = artifact_root_for(
                &repo_root,
                Some(&result.pointer.git_path),
                &result.review_input,
            );
            let review_input_path = repo_root
                .join(&result.pointer.git_path)
                .join("review_input.json");

            crate::cli_status::emit(format!(
                "extract {id}: body_chars={} sections={} equations={} citations={} contexts={} theorem_nodes={} ready={}",
                audit.body_chars,
                audit.section_count,
                audit.equation_count,
                audit.citation_count,
                audit.citation_context_count,
                audit.theorem_node_count,
                audit.review_ready
            ));

            if !json {
                println!(
                    "arxiv_id={id} paper_id={} artifact_root={} review_input={} review_ready={}",
                    result.paper_id,
                    artifact_root.display(),
                    review_input_path.display(),
                    audit.review_ready
                );
                println!(
                    "counts body_chars={} sections={} equations={} citations={} citation_contexts={} theorem_nodes={}",
                    audit.body_chars,
                    audit.section_count,
                    audit.equation_count,
                    audit.citation_count,
                    audit.citation_context_count,
                    audit.theorem_node_count
                );
                for warning in &audit.warnings {
                    eprintln!("warning: {warning}");
                }
                for failure in &audit.failures {
                    eprintln!("failure: {failure}");
                }
            }

            if !audit.review_ready {
                failures.push(format!("{id}: {}", audit.failures.join("; ")));
            }

            outputs.push(ExtractCommandOutput {
                arxiv_id: id.clone(),
                paper_id: result.paper_id,
                artifact_root: artifact_root.display().to_string(),
                review_input: review_input_path.display().to_string(),
                audit,
            });
        }

        if json {
            if outputs.len() == 1 {
                println!("{}", serde_json::to_string_pretty(&outputs[0])?);
            } else {
                println!("{}", serde_json::to_string_pretty(&outputs)?);
            }
        }

        if !failures.is_empty() {
            anyhow::bail!(
                "{} extraction audit(s) failed: {}",
                failures.len(),
                failures.join(" | ")
            );
        }
        Ok(())
    }
    #[cfg(not(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage")))]
    {
        let _ = (arxiv_ids, json);
        anyhow::bail!("extract requires --features full (grokrxiv-ingest + grokrxiv-storage)")
    }
}

fn data_repo_root() -> PathBuf {
    std::env::var("GROKRXIV_DATA_REPO_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/mlong/Documents/Development/grokrxiv-data"))
}

#[cfg(feature = "grokrxiv-storage")]
fn artifact_root_for(
    repo_root: &Path,
    git_path: Option<&str>,
    review_input: &grokrxiv_storage::ReviewInput,
) -> PathBuf {
    git_path
        .map(|p| repo_root.join(p))
        .or_else(|| relative_artifact_parent(&review_input.metadata).map(|p| repo_root.join(p)))
        .unwrap_or_else(|| repo_root.join("papers").join(&review_input.arxiv_id))
}

#[cfg(feature = "grokrxiv-storage")]
fn relative_artifact_parent(path: &str) -> Option<PathBuf> {
    if path.starts_with("supabase://") {
        return None;
    }
    let p = PathBuf::from(path);
    p.parent().map(Path::to_path_buf)
}

#[cfg(feature = "grokrxiv-storage")]
fn audit_review_input_artifacts(
    repo_root: &Path,
    git_path: Option<&str>,
    review_input: &grokrxiv_storage::ReviewInput,
) -> anyhow::Result<ExtractionAudit> {
    let mut warnings = Vec::new();
    let mut failures = Vec::new();

    let metadata = read_review_json(repo_root, &review_input.metadata, "metadata.json")?;
    let body = read_review_text(repo_root, &review_input.body_markdown, "body.md")?;
    let sections_doc = read_review_json(repo_root, &review_input.sections, "sections.json")?;
    let equations_doc = read_review_json(repo_root, &review_input.equations, "equations.json")?;
    let references_doc = read_review_json(repo_root, &review_input.references, "references.json")?;
    let theorem_doc =
        read_review_json(repo_root, &review_input.theorem_graph, "theorem_graph.json")?;
    let report_doc = read_review_json(
        repo_root,
        &review_input.extraction_report,
        "extraction_report.json",
    )?;

    if metadata
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        failures.push("metadata title is empty".to_string());
    }
    if metadata
        .get("abstract")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
    {
        failures.push("metadata abstract is empty".to_string());
    }

    let body_chars = body.chars().count();
    if body_chars < 1_000 {
        failures.push(format!(
            "body.md is too small for review context ({body_chars} chars)"
        ));
    }

    let section_count = array_len(&sections_doc, "sections");
    if section_count == 0 {
        failures.push("sections.json has no sections".to_string());
    }

    let equation_count = array_len(&equations_doc, "equations");
    if equation_count == 0 {
        if body_has_math_signal(&body) {
            failures.push(
                "equations.json has no equations even though body.md contains math markers"
                    .to_string(),
            );
        } else {
            warnings.push("equations.json has no equations".to_string());
        }
    }

    let citation_count = array_len(&references_doc, "citations");
    let citation_context_count = references_doc
        .get("citations")
        .and_then(serde_json::Value::as_array)
        .map(|citations| {
            citations
                .iter()
                .map(|c| array_len(c, "contexts"))
                .sum::<usize>()
        })
        .unwrap_or_default();
    if citation_count == 0 && body_has_citation_signal(&body) {
        failures.push(
            "references.json has no citations even though body.md contains citation markers"
                .to_string(),
        );
    }
    if citation_count > 0 && citation_context_count == 0 {
        failures.push(
            "references.json has citations but no citation contexts for reviewers".to_string(),
        );
    }

    let theorem_node_count = array_len(&theorem_doc, "nodes");
    if theorem_node_count == 0 && body_has_theorem_signal(&body) {
        warnings.push("theorem_graph.json has no nodes despite theorem-like text".to_string());
    }

    let extraction_stage_count = array_len(&report_doc, "stages");
    if extraction_stage_count == 0 {
        failures.push("extraction_report.json has no stages".to_string());
    }
    audit_extraction_report_provenance(&report_doc, &mut warnings, &mut failures);

    let artifact_root = artifact_root_for(repo_root, git_path, review_input);
    for file in [
        "metadata.json",
        "body.md",
        "sections.json",
        "equations.json",
        "references.json",
        "theorem_graph.json",
        "extraction_report.json",
        "review_input.json",
    ] {
        let path = artifact_root.join(file);
        if !path.exists() {
            failures.push(format!("missing reviewer artifact {}", path.display()));
        }
    }

    Ok(ExtractionAudit {
        review_ready: failures.is_empty(),
        body_chars,
        section_count,
        equation_count,
        citation_count,
        citation_context_count,
        theorem_node_count,
        extraction_stage_count,
        warnings,
        failures,
    })
}

#[cfg(feature = "grokrxiv-storage")]
fn read_review_json(repo_root: &Path, rel: &str, label: &str) -> anyhow::Result<serde_json::Value> {
    let path = review_artifact_path(repo_root, rel, label)?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(feature = "grokrxiv-storage")]
fn read_review_text(repo_root: &Path, rel: &str, label: &str) -> anyhow::Result<String> {
    let path = review_artifact_path(repo_root, rel, label)?;
    std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
}

#[cfg(feature = "grokrxiv-storage")]
fn review_artifact_path(repo_root: &Path, rel: &str, label: &str) -> anyhow::Result<PathBuf> {
    if rel.starts_with("supabase://") {
        anyhow::bail!(
            "{label} points to Supabase storage; local CLI reviewer path needs a Tier-1 file"
        )
    }
    Ok(repo_root.join(rel))
}

#[cfg(feature = "grokrxiv-storage")]
fn array_len(doc: &serde_json::Value, key: &str) -> usize {
    doc.get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

#[cfg(feature = "grokrxiv-storage")]
fn body_has_math_signal(body: &str) -> bool {
    body.contains("\\(")
        || body.contains("\\[")
        || body.contains("$$")
        || body.contains("\\begin{equation")
        || body.contains("\\begin{align")
}

#[cfg(feature = "grokrxiv-storage")]
fn body_has_citation_signal(body: &str) -> bool {
    body.contains("[@") || body.contains("\\cite")
}

#[cfg(feature = "grokrxiv-storage")]
fn body_has_theorem_signal(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("theorem")
        || lower.contains("lemma")
        || lower.contains("proposition")
        || lower.contains("corollary")
}

#[cfg(feature = "grokrxiv-storage")]
fn audit_extraction_report_provenance(
    report: &serde_json::Value,
    warnings: &mut Vec<String>,
    failures: &mut Vec<String>,
) {
    let Some(stages) = report.get("stages").and_then(serde_json::Value::as_array) else {
        return;
    };
    for stage in stages {
        let name = stage
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unknown>");
        let status = stage
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unknown>");
        if status == "degraded" {
            warnings.push(format!("extraction stage {name} degraded"));
        }
        if matches!(
            name,
            "vlm" | "macros" | "equations" | "theorems" | "citations"
        ) && status == "ok"
        {
            for field in ["model", "runner", "iters"] {
                if stage.get(field).is_none() || stage.get(field).is_some_and(|v| v.is_null()) {
                    failures.push(format!(
                        "extraction stage {name} missing provenance field {field}"
                    ));
                }
            }
        }
    }
}

async fn ingest_range(
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
    categories: Option<String>,
    no_review: bool,
) -> anyhow::Result<()> {
    #[cfg(feature = "grokrxiv-ingest")]
    {
        let config = super::Config::from_env();
        let state = super::AppState::from_config(config).await?;
        let Some(pool) = state.db.as_ref() else {
            anyhow::bail!("ingest-range: DATABASE_URL not configured");
        };
        let cats =
            categories.unwrap_or_else(|| super::scheduler::DEFAULT_ACTIVE_CATEGORIES.join(","));
        let cat_vec: Vec<String> = cats
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let cat_refs: Vec<&str> = cat_vec.iter().map(String::as_str).collect();
        let records =
            grokrxiv_ingest::fetch_listing(&cat_refs, from, to, &state.config.arxiv_user_agent)
                .await?;
        println!("discovered={}", records.len());
        for meta in records {
            let field = meta.primary_category();
            let extract = grokrxiv_schemas::PaperExtract {
                arxiv_id: meta.arxiv_id.clone(),
                title: meta.title,
                authors: meta.authors,
                abstract_: meta.abstract_text,
                field,
                sections: Vec::new(),
                figures: Vec::new(),
                bibliography: Vec::new(),
                source_format: None,
            };
            let paper_id = crate::db::upsert_paper(pool, &extract, meta.submitted_date).await?;
            println!("paper_id={paper_id} arxiv_id={}", extract.arxiv_id);
            if !no_review
                && meta
                    .submitted_date
                    .map(|d| {
                        super::scheduler::paper_in_auto_review_window(
                            d,
                            state.config.scheduler.auto_review_from,
                        )
                    })
                    .unwrap_or(false)
            {
                let review_id =
                    super::supervisor::run_review_for_paper_blocking(&state, paper_id).await?;
                println!("arxiv_id={} review_id={review_id}", extract.arxiv_id);
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = (from, to, categories, no_review);
        anyhow::bail!("ingest-range requires --features full (grokrxiv-ingest)")
    }
}

async fn ingest_daily() -> anyhow::Result<()> {
    let today = chrono::Utc::now().date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    ingest_range(yesterday, today, None, false).await
}

async fn list(what: ListKind) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let Some(pool) = state.db.as_ref() else {
        anyhow::bail!("list: DATABASE_URL not configured");
    };
    match what {
        ListKind::Reviews {
            review_status,
            limit,
            json,
            ..
        } => {
            let rows =
                crate::db::list_reviews(pool, review_status.as_deref(), limit as i64).await?;
            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else if rows.is_empty() {
                println!("(no reviews)");
            } else {
                println!("{:36}  {:22}  {:12}  title", "id", "status", "arxiv_id");
                for r in rows {
                    let title = truncate(&r.title, 60);
                    println!("{}  {:22}  {:12}  {}", r.id, r.status, r.arxiv_id, title);
                }
            }
        }
        ListKind::Papers {
            json,
            limit,
            field,
            has_review,
            extracted,
        } => {
            list_papers(pool, field.as_deref(), has_review, extracted, limit, json).await?;
        }
        ListKind::Extracted { json, limit, field } => {
            list_papers(pool, field.as_deref(), false, true, limit, json).await?;
        }
    }
    Ok(())
}

async fn list_papers(
    pool: &sqlx::PgPool,
    field: Option<&str>,
    has_review: bool,
    extracted: bool,
    limit: u32,
    json: bool,
) -> anyhow::Result<()> {
    let rows: Vec<PaperListRow> = sqlx::query_as(
        "select p.id, p.arxiv_id, p.title, p.field, p.ingested_at, \
                pa.extraction_status, pa.git_path, pa.updated_at \
         from papers p \
         left join paper_assets pa on pa.paper_id = p.id \
         where ($1::text is null or p.field = $1) \
           and ($2::bool = false or exists (select 1 from reviews r where r.paper_id = p.id)) \
           and ($3::bool = false or pa.extraction_status = 'ready') \
         order by coalesce(pa.updated_at, p.ingested_at) desc \
         limit $4",
    )
    .bind(field)
    .bind(has_review)
    .bind(extracted)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(
                |(id, arxiv, title, field, ts, extraction_status, git_path, extracted_at)| {
                    serde_json::json!({
                        "id": id,
                        "arxiv_id": arxiv,
                        "title": title,
                        "field": field,
                        "ingested_at": ts,
                        "extraction_status": extraction_status,
                        "git_path": git_path,
                        "extracted_at": extracted_at,
                    })
                },
            )
            .collect();
        println!("{}", serde_json::to_string(&v)?);
    } else if rows.is_empty() {
        println!("(no papers)");
    } else {
        println!(
            "{:36}  {:12}  {:8}  {:10}  {:24}  title",
            "id", "arxiv_id", "field", "extract", "git_path"
        );
        for (id, arxiv, title, field, _, extraction_status, git_path, _) in rows {
            println!(
                "{}  {:12}  {:8}  {:10}  {:24}  {}",
                id,
                arxiv,
                field.as_deref().unwrap_or(""),
                extraction_status.as_deref().unwrap_or("pending"),
                truncate(git_path.as_deref().unwrap_or(""), 24),
                truncate(&title, 70)
            );
        }
    }
    Ok(())
}

async fn show(review_id: Uuid, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let Some(pool) = state.db.as_ref() else {
        anyhow::bail!("show: DATABASE_URL not configured");
    };
    let Some(row) = crate::db::show_review(pool, review_id).await? else {
        anyhow::bail!("show: review {review_id} not found");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&row)?);
    } else {
        println!("id          = {}", row.id);
        println!("status      = {}", row.status);
        println!("arxiv_id    = {}", row.arxiv_id);
        println!("title       = {}", row.title);
        println!("paper_id    = {}", row.paper_id);
        println!("agents      = {}", row.agents_count);
        println!("corrections = {}", row.corrections_count);
        println!("created_at  = {}", row.created_at);
        if let Some(pr) = row.github_pr_url.as_deref() {
            println!("pr_url      = {}", pr);
        }
        if let Some(meta) = row.meta_review.as_ref() {
            if let Some(summary) = meta.get("summary").and_then(|v| v.as_str()) {
                println!("summary     = {}", truncate(summary, 200));
            }
            if let Some(rec) = meta.get("recommendation").and_then(|v| v.as_str()) {
                println!("recommend   = {}", rec);
            }
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

async fn review_paper(paper_id: Uuid) -> anyhow::Result<()> {
    #[cfg(feature = "grokrxiv-ingest")]
    {
        let config = super::Config::from_env();
        let state = super::AppState::from_config(config).await?;
        let review_id = super::supervisor::run_review_for_paper_blocking(&state, paper_id).await?;
        println!("paper_id={paper_id} review_id={review_id}");
        Ok(())
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = paper_id;
        anyhow::bail!("review requires --features full (grokrxiv-ingest)")
    }
}

async fn review_extracted(source: &str, force: bool) -> anyhow::Result<()> {
    #[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
    {
        let config = super::Config::from_env();
        let state = super::AppState::from_config(config).await?;
        let Some(pool) = state.db.as_ref() else {
            anyhow::bail!("review-extracted: DATABASE_URL not configured");
        };
        let (paper_id, arxiv_id, title) = resolve_extracted_paper(pool, source).await?;
        if !force {
            if let Some((review_id, status, pr_url)) =
                active_review_for_paper(pool, paper_id).await?
            {
                let mut msg = format!(
                    "review-extracted: paper {arxiv_id} already has active review {review_id} (status={status}); use `grokrxiv show {review_id}`"
                );
                if let Some(pr_url) = pr_url {
                    msg.push_str(&format!(" or review PR {pr_url}"));
                }
                msg.push_str("; pass `--force` to supersede it");
                anyhow::bail!(msg);
            }
        }
        crate::cli_status::emit(format!(
            "paper {arxiv_id}: reviewing cached extraction for `{}`",
            truncate(&title, 80)
        ));
        let review_id = super::supervisor::run_review_for_paper_blocking(&state, paper_id).await?;
        println!("arxiv_id={arxiv_id} paper_id={paper_id} review_id={review_id}");
        Ok(())
    }
    #[cfg(not(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage")))]
    {
        let _ = (source, force);
        anyhow::bail!(
            "review-extracted requires --features full (grokrxiv-ingest + grokrxiv-storage)"
        )
    }
}

#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
async fn active_review_for_paper(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
) -> anyhow::Result<Option<(Uuid, String, Option<String>)>> {
    let row = sqlx::query_as(
        "select id, status, github_pr_url \
         from reviews \
         where paper_id = $1 \
           and status in ('draft','in_review','awaiting_moderation','pr_open','published','corrected') \
         order by created_at desc \
         limit 1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

#[cfg(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage"))]
async fn resolve_extracted_paper(
    pool: &sqlx::PgPool,
    source: &str,
) -> anyhow::Result<(Uuid, String, String)> {
    let source = source.trim();
    let row: Option<(Uuid, String, String, Option<String>, Option<String>)> = if let Ok(id) =
        Uuid::parse_str(source)
    {
        sqlx::query_as(
            "select p.id, p.arxiv_id, p.title, pa.extraction_status, pa.git_path \
                 from papers p left join paper_assets pa on pa.paper_id = p.id \
                 where p.id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?
    } else if let Some(arxiv_id) = parse_arxiv_source(source) {
        sqlx::query_as(
            "select p.id, p.arxiv_id, p.title, pa.extraction_status, pa.git_path \
                 from papers p left join paper_assets pa on pa.paper_id = p.id \
                 where p.arxiv_id = $1",
        )
        .bind(arxiv_id)
        .fetch_optional(pool)
        .await?
    } else {
        anyhow::bail!("review-extracted: `{source}` is not a paper UUID, arXiv id, or arXiv URL");
    };

    let Some((paper_id, arxiv_id, title, status, git_path)) = row else {
        anyhow::bail!(
            "review-extracted: no paper row for `{source}`; run `grokrxiv extract {source}` first"
        );
    };
    if status.as_deref() != Some("ready") || git_path.is_none() {
        anyhow::bail!(
            "review-extracted: paper {arxiv_id} is not extracted yet (status={}); run `grokrxiv extract {arxiv_id}` first",
            status.as_deref().unwrap_or("pending")
        );
    }
    Ok((paper_id, arxiv_id, title))
}

/// Source resolution for `grokrxiv review <source>`.
#[derive(Debug, Clone)]
enum ResolvedSource {
    /// arXiv id (already normalised).
    Arxiv(String),
    /// Local file path. Kind is best-guess from the extension.
    LocalFile(std::path::PathBuf, SourceType),
}

/// Try to recognise the source as an arXiv id or arXiv URL. Returns the bare
/// id (without version suffix) when matched.
fn parse_arxiv_source(s: &str) -> Option<String> {
    let s = s.trim();
    // Bare modern ID, e.g. "2605.12484" or "2605.12484v3"
    if let Some(id) = parse_bare_modern(s) {
        return Some(id);
    }
    // Modern URL: https://arxiv.org/abs/<id> or /pdf/<id>[.pdf]
    if let Some(id) = parse_arxiv_url(s) {
        return Some(id);
    }
    // Legacy: archive/7digits, e.g. math-ph/0506010
    if parse_legacy_id(s).is_some() {
        return Some(s.to_string());
    }
    None
}

fn parse_bare_modern(s: &str) -> Option<String> {
    // YYMM.NNNNN with optional version
    let mut parts = s.splitn(2, 'v');
    let base = parts.next()?;
    let (a, b) = base.split_once('.')?;
    if a.len() < 4 || a.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    if b.len() < 4 || b.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    Some(base.to_string())
}

fn parse_arxiv_url(s: &str) -> Option<String> {
    let stripped = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let stripped = stripped.strip_prefix("arxiv.org/").unwrap_or(stripped);
    let stripped = stripped
        .strip_prefix("abs/")
        .or_else(|| stripped.strip_prefix("pdf/"))?;
    let stripped = stripped.strip_suffix(".pdf").unwrap_or(stripped);
    parse_bare_modern(stripped).or_else(|| parse_legacy_id(stripped))
}

fn parse_legacy_id(s: &str) -> Option<String> {
    // archive[.subj]/7digits
    let (archive, rest) = s.split_once('/')?;
    if archive.is_empty()
        || !archive
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '.')
    {
        return None;
    }
    let rest = rest
        .strip_prefix(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest);
    if rest.len() != 7 || rest.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    Some(s.to_string())
}

fn guess_local_kind(path: &std::path::Path) -> SourceType {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
    {
        Some(ref ext) if ext == "pdf" => SourceType::Pdf,
        Some(ref ext) if ext == "tex" => SourceType::Tex,
        _ => SourceType::Mixed,
    }
}

/// Expand a single source argument into one or more resolved sources.
///
/// - `@file` reads a newline-delimited file.
/// - `-` reads stdin (with optional `--type`).
/// - Otherwise we try arXiv id/URL first, then fall back to local file.
async fn resolve_source(
    source: &str,
    type_hint: Option<SourceType>,
) -> anyhow::Result<Vec<ResolvedSource>> {
    if let Some(path) = source.strip_prefix('@') {
        let body = tokio::fs::read_to_string(path).await?;
        let mut out = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            out.extend(Box::pin(resolve_source(line, type_hint)).await?);
        }
        return Ok(out);
    }
    if source == "-" {
        // Drain stdin into a temp file. The kind defaults to Mixed.
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::with_capacity(1024 * 64);
        tokio::io::stdin().read_to_end(&mut buf).await?;
        let kind = type_hint.unwrap_or(SourceType::Mixed);
        let ext = match kind {
            SourceType::Pdf => ".pdf",
            SourceType::Tex => ".tex",
            SourceType::Arxiv | SourceType::Mixed => ".bin",
        };
        let mut path = std::env::temp_dir();
        path.push(format!(
            "grokrxiv-stdin-{}{ext}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::write(&path, &buf).await?;
        return Ok(vec![ResolvedSource::LocalFile(path, kind)]);
    }
    if let Some(id) = parse_arxiv_source(source) {
        return Ok(vec![ResolvedSource::Arxiv(id)]);
    }
    let path = std::path::PathBuf::from(source);
    if path.is_file() {
        let kind = type_hint.unwrap_or_else(|| guess_local_kind(&path));
        return Ok(vec![ResolvedSource::LocalFile(path, kind)]);
    }
    anyhow::bail!("could not resolve source `{source}` (not an arXiv id/URL, not a local file)")
}

/// Canonical end-to-end entry point — `grokrxiv review <source>`.
async fn review_source(
    source: &str,
    type_hint: Option<SourceType>,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let resolved = resolve_source(source, type_hint).await?;
    if dry_run {
        let plan: Vec<serde_json::Value> = resolved
            .iter()
            .map(|s| match s {
                ResolvedSource::Arxiv(id) => serde_json::json!({"kind": "arxiv", "id": id}),
                ResolvedSource::LocalFile(p, k) => serde_json::json!({
                    "kind": "local",
                    "path": p.display().to_string(),
                    "type": format!("{k:?}"),
                }),
            })
            .collect();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"plan": plan}))?
            );
        } else {
            println!("dry-run plan:");
            for p in plan {
                println!("  {}", p);
            }
        }
        return Ok(());
    }

    // arXiv sources are dispatched through the existing ingest_many path which
    // handles parallelism + rate limiting. Local files are not supported
    // end-to-end in RPT2 — we surface a clear error pointing the operator
    // back at the arXiv path.
    let arxiv_ids: Vec<String> = resolved
        .iter()
        .filter_map(|s| match s {
            ResolvedSource::Arxiv(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let local: Vec<String> = resolved
        .iter()
        .filter_map(|s| match s {
            ResolvedSource::LocalFile(p, _) => Some(p.display().to_string()),
            _ => None,
        })
        .collect();
    if !local.is_empty() {
        anyhow::bail!(
            "local-file review path is not wired in this build ({} local input(s) deferred to a follow-up). \
             Use an arXiv id/URL for the canonical end-to-end review.",
            local.len()
        );
    }
    if arxiv_ids.is_empty() {
        anyhow::bail!("no reviewable sources resolved from `{source}`");
    }

    if json {
        review_arxiv_ids_json(&arxiv_ids).await
    } else {
        ingest_many(&arxiv_ids, false).await
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn review_arxiv_ids_json(arxiv_ids: &[String]) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());

    let mut results: Vec<serde_json::Value> = Vec::with_capacity(arxiv_ids.len());
    for id in arxiv_ids {
        crate::cli_status::emit(format!(
            "paper {id}: fetch -> extract -> review -> verifier -> render -> moderation"
        ));
        let review_id = super::supervisor::run_one_paper_blocking(&supervisor, &state, id).await?;
        crate::cli_status::emit(format!(
            "paper {id}: review_id={review_id} awaiting human moderation"
        ));
        // Pull status + per-agent verifier_status for the JSON envelope so the
        // smoke test can `jq -e .agents | length == 6 and all(.verifier_status==pass)`.
        let mut envelope = serde_json::json!({
            "arxiv_id": id,
            "review_id": review_id,
            "status": "awaiting_moderation",
        });
        if let Some(pool) = state.db.as_ref() {
            if let Ok((status,)) =
                sqlx::query_as::<_, (String,)>("select status from reviews where id = $1")
                    .bind(review_id)
                    .fetch_one(pool)
                    .await
            {
                envelope["status"] = serde_json::Value::String(status);
            }
            let agents: Vec<(String, Option<String>)> = sqlx::query_as(
                "select role, verifier_status from review_agents where review_id = $1 order by role",
            )
            .bind(review_id)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            let agents_json: Vec<serde_json::Value> = agents
                .into_iter()
                .map(|(role, vs)| {
                    serde_json::json!({
                        "role": role,
                        "verifier_status": vs.unwrap_or_else(|| "unknown".to_string()),
                    })
                })
                .collect();
            envelope["agents"] = serde_json::Value::Array(agents_json);
        }
        results.push(envelope);
    }
    if results.len() == 1 {
        println!("{}", serde_json::to_string_pretty(&results[0])?);
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"reviews": results}))?
        );
    }
    Ok(())
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn review_arxiv_ids_json(_arxiv_ids: &[String]) -> anyhow::Result<()> {
    anyhow::bail!("review --json requires --features full (grokrxiv-ingest)")
}

async fn verify(review_id: Uuid) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let Some(pool) = state.db.as_ref() else {
        anyhow::bail!("verify: DATABASE_URL not configured");
    };
    let rows: Vec<(String, Option<String>, Option<serde_json::Value>)> = sqlx::query_as(
        "select role, verifier_status, verifier_notes from review_agents \
         where review_id = $1 order by role",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        anyhow::bail!("verify: no review_agents rows found for {review_id}");
    }
    for (role, status, notes) in rows {
        println!(
            "role={role} verifier_status={} notes_present={}",
            status.unwrap_or_else(|| "<unset>".to_string()),
            notes.is_some()
        );
    }
    Ok(())
}

async fn render(
    review_id: Uuid,
    format: RenderFormat,
    out: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    #[cfg(feature = "grokrxiv-render")]
    {
        let _ = (format, out);
        let config = super::Config::from_env();
        let state = super::AppState::from_config(config).await?;
        super::supervisor::render_to_disk(&state, review_id).await?;
        println!("review_id={review_id} artifacts=artifacts/{review_id}");
        Ok(())
    }
    #[cfg(not(feature = "grokrxiv-render"))]
    {
        let _ = (review_id, format, out);
        anyhow::bail!("render requires --features full (grokrxiv-render)")
    }
}

async fn approve(review_id: Uuid, json: bool) -> anyhow::Result<()> {
    crate::cli_status::emit(format!(
        "review {review_id}: opening publication PR; human merge is required before publish"
    ));
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    approve_impl(&state, review_id, json).await
}

#[cfg(feature = "grokrxiv-publisher")]
async fn approve_impl(state: &super::AppState, review_id: Uuid, json: bool) -> anyhow::Result<()> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    // Read the review row + the joined paper for branch + field + arxiv_id.
    let row: (Uuid, String, String, Option<String>, Uuid) = sqlx::query_as(
        "select r.id, p.arxiv_id, p.title, p.field, p.id \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let (_, arxiv_id, title, field, paper_id) = row;

    // Read on-disk artifacts produced by the M1 run.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{arxiv_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        arxiv_id = arxiv_id,
    );
    for name in ["review.html", "review.md", "review.tex", "bundle.zip"] {
        let path = dir_local.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            files.push((format!("{repo_prefix}/{name}"), bytes));
        } else {
            tracing::warn!(path = %path.display(), "approve: artifact missing, skipping");
        }
    }
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts found under artifacts/{review_id} — \
             re-run `grokrxiv ingest <arxiv_id>` to regenerate."
        );
    }

    // GitHub token + repo are required for the real PR. Without them we
    // simulate so the CLI is still runnable for local-only flows.
    let Some(token) = std::env::var("GITHUB_TOKEN").ok() else {
        tracing::warn!(
            %review_id,
            artifacts = files.len(),
            "GITHUB_TOKEN not set — simulating approval (no PR opened)"
        );
        let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
        let simulated = format!(
            "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-{}",
            &review_id.simple().to_string()[..8]
        );
        let _ = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
            .bind(review_id)
            .bind(&simulated)
            .execute(pool)
            .await;
        if json {
            println!(
                "{}",
                serde_json::json!({"review_id": review_id, "pr_url": simulated, "status": "pr_open"})
            );
        } else {
            println!("pr_url={simulated}");
        }
        crate::cli_status::emit(format!(
            "review {review_id}: simulated pr_open; publish waits for a real reviewed merge webhook"
        ));
        return Ok(());
    };

    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo = std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "grokrxiv-reviews".into());
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner, repo);

    let admin = AdminCaller::from_admin_endpoint();
    let pr_title = format!("Review: {} (arXiv:{})", title, arxiv_id);
    let params = OpenReviewPr {
        arxiv_id: arxiv_id.clone(),
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: pr_title,
        review_id,
        body_md: format!(
            "Approved by `grokrxiv approve {review_id}`. \
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
        ),
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;

    // Persist transition.
    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
        .bind(review_id)
        .bind(&pr_url)
        .execute(pool)
        .await;

    // FP-RPT3c C2 — close any superseded PR for this paper.
    close_superseded_pr_if_any_cli(pool, &publisher, &admin, paper_id, &pr_url).await;

    if json {
        println!(
            "{}",
            serde_json::json!({"review_id": review_id, "pr_url": pr_url, "status": "pr_open"})
        );
    } else {
        println!("pr_url={pr_url}");
    }
    crate::cli_status::emit(format!(
        "review {review_id}: pr_open at {pr_url}; review and merge the PR manually to publish"
    ));
    Ok(())
}

/// Local copy of supervisor::close_superseded_pr_if_any. Lives here so the
/// `grokrxiv approve` CLI command (which doesn't go through the supervisor
/// background worker) also closes the prior PR on supersede.
#[cfg(feature = "grokrxiv-publisher")]
async fn close_superseded_pr_if_any_cli(
    pool: &sqlx::PgPool,
    publisher: &grokrxiv_publisher::GithubPublisher,
    admin: &grokrxiv_publisher::AdminCaller,
    paper_id: Uuid,
    new_pr_url: &str,
) {
    let prior = match crate::db::fetch_superseded_pr_url(pool, paper_id).await {
        Ok(opt) => opt,
        Err(e) => {
            tracing::warn!(%paper_id, err = %e, "supersede: fetch_superseded_pr_url failed");
            return;
        }
    };
    let Some(prior_url) = prior else { return };
    let Some(prior_n) = grokrxiv_publisher::parse_pr_number(&prior_url) else {
        tracing::warn!(
            %paper_id,
            %prior_url,
            "supersede: prior PR URL did not parse to a numeric id (simulated PR?)",
        );
        return;
    };
    let new_n_str = grokrxiv_publisher::parse_pr_number(new_pr_url)
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| new_pr_url.to_string());
    let comment = format!(
        "Superseded by {new_n_str}.\n\
         The new review run incorporated extraction-pipeline fixes and the prior review row was transitioned to status='withdrawn'.",
    );
    if let Err(e) = publisher
        .close_pr_with_comment(admin, prior_n, &comment)
        .await
    {
        tracing::warn!(
            %paper_id,
            prior_pr = %prior_url,
            err = %e,
            "supersede: close_pr_with_comment failed — leaving prior PR as-is (likely already closed)",
        );
    } else {
        tracing::info!(
            %paper_id,
            prior_pr = %prior_url,
            new_pr = %new_pr_url,
            "supersede: closed prior PR",
        );
    }
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn approve_impl(
    _state: &super::AppState,
    review_id: Uuid,
    _json: bool,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "approve <{review_id}> requires --features full (grokrxiv-publisher) at build time."
    )
}

/// `grokrxiv reject <REVIEW_ID> --reason TEXT`. Updates the most-recent
/// moderation_queue row's state to `rejected`, leaves `reviews.status` at
/// `awaiting_moderation` per spec.
async fn reject(review_id: Uuid, reason: &str) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("reject: DATABASE_URL not configured"))?;
    let moderator = moderator_handle();
    let n = crate::db::update_moderation_state(
        pool,
        review_id,
        "rejected",
        Some(reason),
        Some(&moderator),
    )
    .await?;
    if n == 0 {
        anyhow::bail!(
            "reject: no moderation_queue row for review {review_id} (was insert_moderation_pending called?)"
        );
    }
    println!("rejected={review_id}");
    Ok(())
}

/// `grokrxiv request-changes <REVIEW_ID> --notes TEXT`.
async fn request_changes(review_id: Uuid, notes: &str) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("request-changes: DATABASE_URL not configured"))?;
    let moderator = moderator_handle();
    let n = crate::db::update_moderation_state(
        pool,
        review_id,
        "changes_requested",
        Some(notes),
        Some(&moderator),
    )
    .await?;
    if n == 0 {
        anyhow::bail!("request-changes: no moderation_queue row for review {review_id}");
    }
    println!("request-changes={review_id}");
    Ok(())
}

/// `grokrxiv withdraw <REVIEW_ID> --reason TEXT`. Inserts a withdrawal row in
/// `corrections`, flips `reviews.status` to `withdrawn`, fires a best-effort
/// revalidate on the configured frontend.
async fn withdraw(review_id: Uuid, reason: &str) -> anyhow::Result<()> {
    use grokrxiv_schemas::ReviewStatus;
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("withdraw: DATABASE_URL not configured"))?;
    let moderator = moderator_handle();
    crate::db::insert_correction(pool, review_id, "withdrawal", reason, &moderator).await?;
    let n = crate::db::set_review_status(pool, review_id, ReviewStatus::Withdrawn, None).await?;
    if n == 0 {
        anyhow::bail!("withdraw: no review row for {review_id}");
    }
    revalidate_best_effort(&state, review_id).await;
    println!("withdrawn={review_id}");
    Ok(())
}

/// `grokrxiv correct <REVIEW_ID> --rationale-md PATH`. Reads the markdown
/// rationale, inserts a `correction` row, flips `reviews.status` to
/// `corrected`, fires a best-effort revalidate.
async fn correct(review_id: Uuid, rationale_md: &std::path::Path) -> anyhow::Result<()> {
    use grokrxiv_schemas::ReviewStatus;
    let body = tokio::fs::read_to_string(rationale_md).await?;
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("correct: DATABASE_URL not configured"))?;
    let moderator = moderator_handle();
    crate::db::insert_correction(pool, review_id, "correction", &body, &moderator).await?;
    let n = crate::db::set_review_status(pool, review_id, ReviewStatus::Corrected, None).await?;
    if n == 0 {
        anyhow::bail!("correct: no review row for {review_id}");
    }
    revalidate_best_effort(&state, review_id).await;
    println!("corrected={review_id}");
    Ok(())
}

fn moderator_handle() -> String {
    std::env::var("GROKRXIV_MODERATOR")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "cli".to_string())
}

async fn revalidate_best_effort(state: &super::AppState, review_id: Uuid) {
    let Some(url) = state.config.web_revalidate_url.as_deref() else {
        return;
    };
    let mut req = state
        .http
        .post(url)
        .json(&serde_json::json!({ "review_id": review_id }));
    if let Some(secret) = state.config.revalidate_secret.as_deref() {
        req = req.header("x-revalidate-secret", secret);
    }
    if let Err(e) = req.send().await {
        tracing::warn!(err = %e, "revalidate POST failed");
    }
}

fn open_review(review_id: Uuid) -> anyhow::Result<()> {
    let base = std::env::var("NEXT_PUBLIC_SITE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let url = format!("{base}/reviews/{review_id}");
    println!("{url}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).status();
    }
    Ok(())
}

async fn tail_jobs(kind: Option<String>, state: Option<String>) -> anyhow::Result<()> {
    eprintln!(
        "tail jobs (kind={:?}, state={:?}): wiring against jobs table — task #15.",
        kind, state
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_parses_status_flags() {
        let status = Cli::try_parse_from(["grokrxiv", "--status", "doctor"]).unwrap();
        assert!(status.status);
        assert!(!status.no_status);

        let no_status = Cli::try_parse_from(["grokrxiv", "--no-status", "doctor"]).unwrap();
        assert!(!no_status.status);
        assert!(no_status.no_status);

        let both = Cli::try_parse_from(["grokrxiv", "--status", "--no-status", "doctor"]);
        assert!(
            both.is_err(),
            "--status and --no-status must be mutually exclusive"
        );
    }

    #[test]
    fn cli_parses_extract_command() {
        let parsed = Cli::try_parse_from([
            "grokrxiv",
            "--extractor",
            "cli",
            "--status",
            "extract",
            "2605.00561",
        ])
        .unwrap();

        assert_eq!(parsed.extractor, Some(ExtractorKind::Cli));
        assert!(parsed.status);
        match parsed.command {
            Some(Command::Extract { arxiv_ids }) => assert_eq!(arxiv_ids, vec!["2605.00561"]),
            other => panic!("expected extract command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_review_extracted_command() {
        let parsed = Cli::try_parse_from([
            "grokrxiv",
            "--runner",
            "cli",
            "--status",
            "review-extracted",
            "2605.00561",
        ])
        .unwrap();

        assert_eq!(parsed.runner, Some(AgentRunnerKind::Cli));
        assert!(parsed.status);
        match parsed.command {
            Some(Command::ReviewExtracted { source, force }) => {
                assert_eq!(source, "2605.00561");
                assert!(!force);
            }
            other => panic!("expected review-extracted command, got {other:?}"),
        }

        let forced =
            Cli::try_parse_from(["grokrxiv", "review-extracted", "--force", "2605.00561"]).unwrap();
        match forced.command {
            Some(Command::ReviewExtracted { source, force }) => {
                assert_eq!(source, "2605.00561");
                assert!(force);
            }
            other => panic!("expected forced review-extracted command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_list_extracted_and_review_status_filter() {
        let listed =
            Cli::try_parse_from(["grokrxiv", "list", "extracted", "--limit", "50"]).unwrap();
        match listed.command {
            Some(Command::List {
                what: ListKind::Extracted { limit, .. },
            }) => assert_eq!(limit, 50),
            other => panic!("expected list extracted command, got {other:?}"),
        }

        let reviews = Cli::try_parse_from([
            "grokrxiv",
            "list",
            "reviews",
            "--review-status",
            "awaiting_moderation",
            "--json",
        ])
        .unwrap();
        match reviews.command {
            Some(Command::List {
                what:
                    ListKind::Reviews {
                        review_status,
                        json,
                        ..
                    },
            }) => {
                assert_eq!(review_status.as_deref(), Some("awaiting_moderation"));
                assert!(json);
            }
            other => panic!("expected list reviews command, got {other:?}"),
        }
    }

    #[cfg(feature = "grokrxiv-storage")]
    #[test]
    fn extraction_audit_rejects_context_free_citations() {
        let repo = tempfile::tempdir().unwrap();
        let arxiv_id = "9999.00001";
        let rel = |file: &str| format!("papers/{arxiv_id}/{file}");
        let paper_dir = repo.path().join("papers").join(arxiv_id);
        std::fs::create_dir_all(&paper_dir).unwrap();
        std::fs::write(
            paper_dir.join("metadata.json"),
            r#"{"title":"Useful Paper","abstract":"A real abstract.","authors":[]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("body.md"),
            format!("## Intro\n\n{} [@smith2026].\n", "body ".repeat(260)),
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("sections.json"),
            r#"{"sections":[{"heading":"Intro","char_start":10,"char_end":1400}]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("references.json"),
            r#"{"citations":[{"key":"smith2026","title":"x","contexts":[]}]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("equations.json"),
            r#"{"equations":[{"id":"eq1","canonical_tex":"x=y"}]}"#,
        )
        .unwrap();
        std::fs::write(paper_dir.join("theorem_graph.json"), r#"{"nodes":[]}"#).unwrap();
        std::fs::write(
            paper_dir.join("extraction_report.json"),
            r#"{"stages":[{"name":"citations","status":"ok","runner":"cli","model":"gemini","iters":3}]}"#,
        )
        .unwrap();

        let review_input = grokrxiv_storage::ReviewInput {
            schema_version: "1".into(),
            arxiv_id: arxiv_id.into(),
            metadata: rel("metadata.json"),
            body_markdown: rel("body.md"),
            sections: rel("sections.json"),
            equations: rel("equations.json"),
            references: rel("references.json"),
            theorem_graph: rel("theorem_graph.json"),
            extraction_report: rel("extraction_report.json"),
            pdf_uri: None,
            source_uri: None,
            semantic_ast_uri: None,
            vlm_raw_uri: None,
            embeddings_uri: None,
        };

        let audit = audit_review_input_artifacts(repo.path(), None, &review_input).unwrap();
        assert!(!audit.review_ready);
        assert!(audit
            .failures
            .iter()
            .any(|msg| msg.contains("citation contexts")));
    }

    #[test]
    fn approve_help_is_pr_handoff_not_publish() {
        let mut cmd = Cli::command();
        let approve = cmd
            .find_subcommand_mut("approve")
            .expect("approve subcommand exists");
        let mut help = Vec::new();
        approve.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("Open the publication PR"));
        assert!(help.contains("does not merge or publish"));
        assert!(!help.contains("approve and publish"));
    }
}
