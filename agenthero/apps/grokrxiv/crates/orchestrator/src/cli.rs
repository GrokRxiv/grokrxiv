//! `agh` CLI surface.
//!
//! The binary's `main()` dispatches to one of the subcommands below. Each
//! variant delegates to a small function so the library/HTTP path and the
//! CLI path call the same plumbing — no duplication.

use agenthero_dag_runtime::{
    AgentKind, DagEdge, DagManifest, DagNode, DagNodeKind, DagRole, DagTool, OneOrMany, RoleId,
    ToolExecutorKind,
};
use anyhow::Context as _;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;

use crate::agents::config as agent_config;
use crate::agents::{
    AgentInput, AgentMode, AgentRun, AgentRunnerKind, RevisionTarget, SandboxPolicy,
};
use crate::cli_status;
use crate::doctor as doctor_mod;
use crate::runtime_config::{
    parse_role_model, parse_role_runner, provider_api_allowed, render as render_runtime_config,
    role_env_suffix, role_model_override_env_var, ExtractorKind, RuntimeConfig,
    RuntimeConfigOverrides, ALLOW_PROVIDER_API_ENV,
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

const PAPER_REVIEW_DAG_ID: &str = "paper-review";
const CITATION_VERIFIER_POSTPROCESSOR: &str = "merge_citation_verifier";

/// AgentHero — DAGOps runtime for agentic applications as DAGs.
#[derive(Debug, Parser)]
#[command(
    name = "agh",
    version,
    about = "AgentHero — DAGOps runtime for agentic applications as DAGs",
    long_about = None,
    arg_required_else_help = true,
    subcommand_required = true,
)]
pub struct Cli {
    /// Subcommand to dispatch.
    #[command(subcommand)]
    pub command: Command,

    /// Whether agents should run in `review_only` mode (default) or
    /// `review_and_revise` mode (emit a `revision_artifact` alongside the
    /// usual review output).
    #[arg(long, value_enum, global = true, default_value_t = AgentMode::ReviewOnly, hide = true)]
    pub mode: AgentMode,

    /// When `--mode review_and_revise`, controls what gets patched: the
    /// paper's LaTeX source (`paper_latex`, default) or GrokRxiv's own
    /// review output (`grokrxiv_review_output`).
    #[arg(long, value_enum, global = true, default_value_t = RevisionTarget::PaperLatex, hide = true)]
    pub revision_target: RevisionTarget,

    /// Default runner backend for all roles.
    #[arg(long, value_enum, global = true)]
    pub runner: Option<AgentRunnerKind>,
    /// Staged extraction backend used by `ingest` before review.
    #[arg(long, value_enum, global = true)]
    pub extractor: Option<ExtractorKind>,
    /// Per-role runner override, e.g. `--runner-for technical_correctness=cli`.
    /// Repeatable.
    #[arg(long, global = true, value_parser = parse_role_runner, value_name = "ROLE=RUNNER", hide = true)]
    pub runner_for: Vec<(String, AgentRunnerKind)>,
    /// Sandbox policy applied to runners that support it.
    #[arg(long, value_enum, global = true, hide = true)]
    pub sandbox: Option<SandboxPolicy>,
    /// Per-role model override, e.g. `--model-for summary=claude-haiku-4-5`.
    /// Repeatable.
    #[arg(long, global = true, value_parser = parse_role_model, value_name = "ROLE=MODEL")]
    pub model_for: Vec<(String, String)>,
    /// Hard cap on total cost (USD) for one review.
    #[arg(long, global = true, hide = true)]
    pub max_cost_usd: Option<f64>,
    /// Skip the review cache.
    #[arg(long, global = true)]
    pub no_cache: bool,
    /// Offline mode (disallow network where avoidable).
    #[arg(long, global = true, hide = true)]
    pub offline: bool,
    /// Plan-only: print what would run but don't make LLM calls.
    #[arg(long, global = true, hide = true)]
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
    /// Path to the TOML config file. Defaults to `~/.agenthero/config.toml`.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,
    /// `config show` flag: print provider secrets in cleartext.
    #[arg(long, global = true, hide = true)]
    pub show_secrets: bool,
    /// Dump the rendered prompt for each role to
    /// `./debug-prompts/<arxiv_id>/<role>.md` after the review finishes.
    #[arg(long, global = true, hide = true)]
    pub debug_prompt: bool,
    /// Emit structured tracing diagnostics to stderr. By default foreground CLI
    /// runs show only human-readable status lines and final output.
    #[arg(long, global = true)]
    pub debug_logs: bool,
    /// Skip selected extraction stages. Comma-separated names from
    /// `{vlm, macros, equations, theorems, citations}`. Each skipped stage
    /// produces a `status: "skipped"` entry in `extraction_report.json`.
    #[arg(long, global = true, value_name = "STAGES", hide = true)]
    pub skip_stages: Option<String>,
    /// Skip Supabase storage writes even when `SUPABASE_URL` and
    /// `SUPABASE_SERVICE_ROLE_KEY` are set. The local grokrxiv-data clone is
    /// still written.
    #[arg(long, global = true, hide = true)]
    pub dry_run_storage: bool,
}

/// Hint for `agh app run grokrxiv review <source>` when the source can't be inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum SourceType {
    /// arXiv id or URL.
    Arxiv,
    /// Local PDF file.
    Pdf,
    /// Local LaTeX (.tex) file.
    Tex,
    /// Git repository containing a PDF or TeX manuscript.
    Git,
    /// Mixed bundle / unknown.
    Mixed,
}

/// Top-level CLI subcommand variants.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List, inspect, or run installed DAGOps apps.
    App {
        /// App registry operation.
        #[command(subcommand)]
        command: AppCommand,
    },

    // ---------- service ----------
    /// Run the HTTP API + tokio supervisor + scheduler.
    Serve,
    /// Print which env vars / external deps / DB / LLM providers are reachable.
    Doctor,
    /// Validate and inspect DAG-type manifests.
    Dag {
        /// DAG operation to run.
        #[command(subcommand)]
        command: DagCommand,
    },
    /// Validate installed app and DAG manifests.
    Validate {
        /// Optional DAG type id to validate.
        #[arg(long = "dag-type")]
        dag_type: Option<String>,
    },
    /// Place or scaffold agent configs.
    Agent {
        /// Agent operation to run.
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Print the resolved orchestrator config (legacy env-only view + the
    /// layered RuntimeConfig). Secrets are redacted unless `--show-secrets`.
    Config {
        /// Print provider secrets in cleartext instead of `***`.
        #[arg(long)]
        show_secrets: bool,
    },
    /// Apply pending Supabase migrations (idempotent).
    #[command(hide = true)]
    Migrate,
    /// Print ALL_CATEGORIES, DEFAULT_ACTIVE_CATEGORIES, and the active env diff.
    #[command(hide = true)]
    Categories,

    /// Inspect queued, running, completed, and failed jobs.
    Jobs {
        /// Jobs operation to run.
        #[command(subcommand)]
        command: JobsCommand,
    },
}

/// Subcommands for product app registry and execution operations.
#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// List installed DAGOps apps.
    List,
    /// Show one app's available actions.
    Show {
        /// App id, e.g. `grokrxiv` or `c2rust`.
        app: String,
    },
    /// Run one installed app action. With no action, prints that app's action catalog.
    Run {
        /// App id, e.g. `grokrxiv` or `c2rust`.
        app: String,
        /// App command path and action-specific arguments.
        #[arg(num_args = 0.., allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// List app run records from the runtime database.
    Runs {
        /// Optional app id filter.
        #[arg(long)]
        app: Option<String>,
    },
    /// Show one app run record.
    Status {
        /// App run UUID.
        run_id: Uuid,
    },
}

/// Subcommands for DAG manifest inspection.
#[derive(Debug, Subcommand)]
pub enum DagCommand {
    /// Validate all DAG manifests, or one selected DAG type.
    Validate {
        /// DAG type id to validate, e.g. `paper-review`.
        #[arg(long = "dag-type")]
        dag_type: Option<String>,
    },
    /// Run one registered DAG app through the generic executor.
    Run {
        /// DAG type id to run, e.g. `c2rust`.
        #[arg(long = "dag-type")]
        dag_type: String,
    },
    /// Add an agent role/node to one DAG manifest.
    AddAgent {
        /// DAG type id to edit, e.g. `paper-review`.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped role id, e.g. `type_theory_validator`.
        #[arg(long = "role-id")]
        role_id: String,
        /// Agent capability kind, e.g. `critic`, `code_generator`.
        #[arg(long)]
        kind: String,
        /// Agent config path. Defaults to `agents/<dag-type>/<role-id>.yaml`.
        #[arg(id = "agent_config", long = "agent-config")]
        config: Option<String>,
        /// Add an edge from this existing node to the new node. Repeatable.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node. Repeatable.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
    /// Remove an agent role/node from one DAG manifest.
    RemoveAgent {
        /// DAG type id to edit, e.g. `paper-review`.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped role id to remove.
        #[arg(long = "role-id")]
        role_id: String,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
    /// Add a tool definition/node to one DAG manifest.
    AddTool {
        /// DAG type id to edit, e.g. `paper-extract`.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped tool id, e.g. `doi_resolver`.
        #[arg(long = "tool-id")]
        tool_id: String,
        /// Tool executor kind: `rust` or `cli`.
        #[arg(long)]
        executor: String,
        /// Rust handler name. Defaults to the tool id for Rust executors.
        #[arg(long)]
        handler: Option<String>,
        /// CLI command token. Repeat for multiple argv tokens.
        #[arg(long = "command")]
        command: Vec<String>,
        /// Add an edge from this existing node to the new node. Repeatable.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node. Repeatable.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Artifact or node input name. Repeatable.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Artifact output name. Repeatable.
        #[arg(long = "output")]
        outputs: Vec<String>,
        /// Optional timeout in seconds.
        #[arg(long = "timeout-secs")]
        timeout_secs: Option<u64>,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
    /// Remove a tool definition/node from one DAG manifest.
    RemoveTool {
        /// DAG type id to edit, e.g. `paper-extract`.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped tool id to remove.
        #[arg(long = "tool-id")]
        tool_id: String,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
    /// Scaffold a Rust tool definition/node in one DAG manifest.
    ScaffoldTool {
        /// DAG type id to edit, e.g. `paper-extract`.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped tool id, e.g. `metadata_consistency_validator`.
        #[arg(long = "tool-id")]
        tool_id: String,
        /// Rust handler name. Defaults to the tool id.
        #[arg(long)]
        handler: Option<String>,
        /// Add an edge from this existing node to the new node. Repeatable.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node. Repeatable.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Artifact or node input name. Repeatable.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Artifact output name. Repeatable.
        #[arg(long = "output")]
        outputs: Vec<String>,
        /// Optional timeout in seconds.
        #[arg(long = "timeout-secs")]
        timeout_secs: Option<u64>,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
}

/// Subcommands for agent config placement/scaffolding.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Print DAG types compatible with an agent YAML's `kind`.
    Place {
        /// Path to an agent YAML file containing a `kind` field.
        path: PathBuf,
    },
}

/// Subcommands for scheduled review batches.
#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    /// Create a batch from an arXiv category/month listing.
    Create {
        /// arXiv OAI-PMH set, e.g. `math`, `cs`, or `quant-ph`.
        #[arg(long)]
        category: String,
        /// Month to review, in `YYYY-MM` form.
        #[arg(long)]
        month: String,
        /// Maximum number of papers scheduled per day.
        #[arg(long, default_value_t = 30)]
        daily_limit: u32,
        /// Maximum number of papers to schedule from the listing.
        #[arg(long)]
        max_items: Option<u32>,
        /// Open the GitHub review PR after each completed review.
        #[arg(long)]
        auto_pr: bool,
        /// First scheduled day. Defaults to the first day of `--month`.
        #[arg(long)]
        start_date: Option<chrono::NaiveDate>,
    },
    /// Run due items for a batch.
    Run {
        /// UUID returned by `batch create`.
        batch_id: Uuid,
        /// Treat this date as "today" when selecting due items.
        #[arg(long)]
        today: Option<chrono::NaiveDate>,
        /// Maximum number of due items to run in this invocation.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show batch progress and next due items.
    Status {
        /// UUID returned by `batch create`.
        batch_id: Uuid,
    },
    /// List recent batches.
    List {
        /// Maximum batches to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

/// Subcommands for job inspection.
#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// List jobs with optional kind/state filters.
    List {
        /// Optional `kind` filter (e.g. `ingest`, `review`).
        #[arg(long)]
        kind: Option<String>,
        /// Optional `state` filter (e.g. `queued`, `running`, `failed`).
        #[arg(long)]
        state: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

/// Selector for `agh app run grokrxiv list`.
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

/// Output format for `agh app run grokrxiv render`.
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
    let status_enabled = status_enabled_for_stderr(&cli, std::io::stderr().is_terminal());
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
        max_cost_usd: cli.max_cost_usd,
        no_cache: cli.no_cache,
        offline: cli.offline,
        runner_for: cli.runner_for.clone(),
        model_for: cli.model_for.clone(),
    };
    let runtime_cfg = match RuntimeConfig::resolve(&overrides, &cli.profile, cli.config.as_deref())
    {
        Ok(cfg) => Some(cfg),
        Err(e) if e.to_string().contains("AGENTHERO_EXTRACTOR") => return Err(e),
        Err(e) => {
            tracing::warn!(err = %e, "could not resolve layered runtime config");
            None
        }
    };
    // Export resolved per-role runner/model choices into the env vars the
    // supervisor reads while composing the agent registry.
    if let Some(rt) = runtime_cfg.as_ref() {
        // Always export `default_runner` so the supervisor can pick up the
        // CLI's `--runner` flag (the resolved RuntimeConfig already merges
        // CLI > ENV > TOML > default).
        let kind = rt.default_runner;
        if let Ok(s) = serde_json::to_string(&kind) {
            let bare = s.trim_matches('"');
            std::env::set_var("AGENTHERO_RUNNER_OVERRIDE", bare);
        }
        for role in crate::runtime_config::configured_review_agent_roles() {
            std::env::remove_var(role_model_override_env_var(&role));
        }
        for (role, kind) in &rt.runner_for {
            let role_slug = role_env_suffix(role);
            if let Ok(s) = serde_json::to_string(kind) {
                let bare = s.trim_matches('"');
                std::env::set_var(format!("AGENTHERO_RUNNER_OVERRIDE_{role_slug}"), bare);
            }
        }
        for (role, model) in &rt.model_for {
            std::env::set_var(role_model_override_env_var(role), model);
        }
        std::env::set_var("AGENTHERO_EXTRACTOR", rt.extractor.as_str());
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

    // When prompt debugging is enabled, export the directory the supervisor
    // uses for rendered prompt snapshots before any review tasks are spawned.
    let debug_prompt_dir: Option<std::path::PathBuf> = if debug_prompt {
        let dir = std::path::PathBuf::from("debug-prompts");
        std::env::set_var("GROKRXIV_DEBUG_PROMPT_DIR", &dir);
        Some(dir)
    } else {
        std::env::remove_var("GROKRXIV_DEBUG_PROMPT_DIR");
        None
    };

    // Forward CLI runtime toggles to the staged ingest orchestrator.
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

    let command = cli.command;
    let is_review_command = matches!(
        &command,
        Command::App {
            command: AppCommand::Run { app, args },
        } if app == "grokrxiv"
            && args
                .first()
                .is_some_and(|action| matches!(action.as_str(), "extract" | "review" | "review-extracted" | "approve" | "request-revisions"))
    );

    if dry_run {
        if let Command::App {
            command: AppCommand::Run { app, args },
        } = &command
        {
            if app == "grokrxiv" && args.first().is_some_and(|action| action == "approve") {
                let review_id = parse_uuid_arg(args.get(1), "review_id")?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "dry_run": true,
                            "app": "grokrxiv",
                            "action": "approve",
                            "review_id": review_id,
                        }))?
                    );
                } else {
                    println!("dry_run=true app=grokrxiv action=approve review_id={review_id}");
                }
                return Ok(());
            }
        }
    }

    let result = match command {
        Command::App { command } => app_command(command, json, dry_run).await,
        Command::Serve => super::serve::run().await,
        Command::Doctor => {
            let code = doctor_mod::doctor(&profile, json).await?;
            if code != 0 {
                anyhow::bail!("doctor: one or more critical checks failed");
            }
            Ok(())
        }
        Command::Dag { command } => dag_command(command, json).await,
        Command::Validate { dag_type } => {
            dag_command(DagCommand::Validate { dag_type }, json).await
        }
        Command::Agent { command } => agent_command(command, json).await,
        Command::Config {
            show_secrets: cmd_show,
        } => print_config(show_secrets || cmd_show, runtime_cfg.as_ref(), json),
        Command::Migrate => migrate().await,
        Command::Categories => print_categories(),
        Command::Jobs { command } => jobs_command(command, json).await,
    };

    if let Some(dir) = debug_prompt_dir.as_ref() {
        if is_review_command {
            println!("debug_prompt_dir={}", dir.display());
        }
    }

    result
}

async fn app_command(command: AppCommand, json: bool, dry_run: bool) -> anyhow::Result<()> {
    match command {
        AppCommand::List => app_list(json),
        AppCommand::Show { app } => app_show(&app, json),
        AppCommand::Run { app, args } => app_args_command(&app, args, json, dry_run).await,
        AppCommand::Runs { app } => app_runs(app.as_deref(), json).await,
        AppCommand::Status { run_id } => app_status(run_id, json).await,
    }
}

async fn app_args_command(
    app: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    if args.is_empty() {
        return app_show(app, json);
    }
    let resolved = crate::dag_apps::resolve_app_action_args(app, &args)?;
    app_run_command(app, &resolved.id, resolved.args, json, dry_run).await
}

pub(crate) fn status_enabled_for_stderr(cli: &Cli, stderr_is_terminal: bool) -> bool {
    cli.status || (!cli.no_status && stderr_is_terminal)
}

fn emit_pipeline_header(command: &str, subject: &str) {
    let runner = std::env::var("AGENTHERO_RUNNER_OVERRIDE")
        .or_else(|_| std::env::var("AGENTHERO_RUNNER"))
        .unwrap_or_else(|_| "cli".to_string());
    let extractor = std::env::var("AGENTHERO_EXTRACTOR").unwrap_or_else(|_| "cli".to_string());
    let cache = if matches!(std::env::var("GROKRXIV_NO_CACHE").as_deref(), Ok("1")) {
        "off"
    } else {
        "on"
    };
    let provider_api = if matches!(std::env::var(ALLOW_PROVIDER_API_ENV).as_deref(), Ok("1")) {
        "enabled"
    } else {
        "disabled"
    };
    let pairs = [
        ("runner", runner.as_str()),
        ("extractor", extractor.as_str()),
        ("cache", cache),
        ("provider_api", provider_api),
    ];
    cli_status::emit_header(command, subject, &pairs);
}

// ---------------------------------------------------------------------------
// Subcommand implementations. Where the supporting plumbing already exists
// (serve, ingest one paper, approve) we wire through; the rest emit a clear
// "not yet implemented in stub build" message that points at the right task.
// ---------------------------------------------------------------------------

fn app_list(json: bool) -> anyhow::Result<()> {
    let apps = crate::dag_apps::load_app_manifests()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "apps": apps.iter().map(|app| serde_json::json!({
                    "id": app.slug,
                    "label": app.label,
                    "actions": app.actions.iter().map(|action| serde_json::json!({
                        "id": action.id,
                        "command": action.command,
                        "dag_type": action.dag_type,
                        "description": action.description,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>()
            }))?
        );
    } else {
        for app in &apps {
            println!("{} — {}", app.slug, app.label);
            for action in &app.actions {
                println!("  {} -> {}", action.id, action.dag_type);
            }
        }
    }
    Ok(())
}

fn app_show(app_id: &str, json: bool) -> anyhow::Result<()> {
    let app = crate::dag_apps::load_app_manifest_by_slug(app_id)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": app.slug,
                "label": app.label,
                "actions": app.actions.iter().map(|action| serde_json::json!({
                    "id": action.id,
                    "command": action.command,
                    "dag_type": action.dag_type,
                    "description": action.description,
                })).collect::<Vec<_>>(),
            }))?
        );
    } else {
        println!("{} — {}", app.slug, app.label);
        for action in &app.actions {
            println!(
                "{}\n  dag_type={}\n  {}",
                action.id, action.dag_type, action.description
            );
        }
    }
    Ok(())
}

async fn app_run_command(
    app: &str,
    action: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let binding = crate::dag_apps::app_action_binding(app, action)?;
    let mut input = agenthero_dag_executor::DagIo::default();
    input.values.insert("app".into(), serde_json::json!(app));
    input
        .values
        .insert("action".into(), serde_json::json!(action));
    input
        .values
        .insert("dag_type".into(), serde_json::json!(binding.dag_type));
    let response = crate::dag_apps::run_app_action(app, action, args, input, json, dry_run).await?;
    if !response.ok {
        anyhow::bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| format!("app `{app}` action `{action}` failed"))
        );
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(report) = response.report {
        println!(
            "app={} action={} dag_type={} status={:?} nodes={}",
            app,
            action,
            report.dag_type,
            report.status,
            report.nodes.len()
        );
    } else if let Some(output) = response.output {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("app={} action={} ok", app, action);
    }
    Ok(())
}

/// Run one GrokRxiv app action inside the app-owned runtime.
pub async fn run_grokrxiv_action(
    action: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    match action {
        "extract" => {
            ensure_args_not_empty(&args, "GrokRxiv extract requires at least one source")?;
            extract_many(&args, json).await
        }
        "ingest" => {
            let request = parse_ingest_args(args)?;
            ingest_many(&request.arxiv_ids, request.auto_moderate, json).await
        }
        "ingest-range" => {
            let request = parse_ingest_range_args(args)?;
            ingest_range(
                request.from,
                request.to,
                request.categories,
                request.no_review,
            )
            .await
        }
        "ingest-daily" => {
            ensure_args_empty(&args, "GrokRxiv ingest-daily takes no arguments")?;
            ingest_daily().await
        }
        "review" => {
            let request = parse_grokrxiv_review_args(args)?;
            review_source(
                &request.source,
                request.source_type,
                ReviewSourceOptions {
                    rev: request.rev,
                    paper_path: request.paper_path,
                    title: request.title,
                    field: request.field,
                    corpus: request.corpus,
                    scan_root: request.scan_root,
                    limit: request.limit,
                    include: request.include,
                    exclude: request.exclude,
                    loop_enabled: request.loop_enabled,
                    debug_output: request.debug_output,
                    no_external_actions: request.no_external_actions,
                },
                json,
                dry_run,
            )
            .await
        }
        "review-extracted" => {
            let request = parse_review_extracted_args(args)?;
            review_extracted(&request.source, request.force, json).await
        }
        "re-review" => {
            let paper_id = parse_uuid_arg(args.first(), "paper_id")?;
            review_paper(paper_id).await
        }
        "verify" => {
            let review_id = parse_uuid_arg(args.first(), "review_id")?;
            verify(review_id).await
        }
        "render" => {
            let request = parse_render_args(args)?;
            render(request.review_id, request.format, request.out).await
        }
        "refresh-review" => {
            let review_id = parse_uuid_arg(args.first(), "review_id")?;
            refresh_review(review_id, json).await
        }
        "show" => {
            let review_id = parse_uuid_arg(args.first(), "review_id")?;
            show(review_id, json).await
        }
        "list" => {
            let request = parse_grokrxiv_list_args(args, json)?;
            list(request).await
        }
        "open" => {
            let review_id = parse_uuid_arg(args.first(), "review_id")?;
            open_review(review_id)
        }
        "approve" => {
            let request = parse_review_id_action_args(args, "approve", "force")?;
            approve(request.review_id, request.force, json).await
        }
        "request-revisions" => {
            let request = parse_review_id_notes_args(args, "request-revisions", false)?;
            request_revisions(request.review_id, request.notes.as_deref(), json).await
        }
        "request-changes" => {
            let request = parse_review_id_notes_args(args, "request-changes", true)?;
            request_changes(request.review_id, request.notes.as_deref().unwrap_or("")).await
        }
        "reject" => {
            let request = parse_review_id_reason_args(args, "reject")?;
            reject(request.review_id, &request.reason).await
        }
        "close" => {
            let request = parse_close_args(args)?;
            close_review(
                request.review_id,
                &request.reason,
                request.keep_github_pr,
                json,
            )
            .await
        }
        "withdraw" => {
            let request = parse_review_id_reason_args(args, "withdraw")?;
            withdraw(request.review_id, &request.reason).await
        }
        "correct" => {
            let request = parse_correct_args(args)?;
            correct(request.review_id, &request.rationale_md).await
        }
        "html-review" => {
            let request = parse_html_review_args(args)?;
            html_review_cmd(request.review_id, request.all, json).await
        }
        "feedback-loop-smoke" => {
            let request = parse_feedback_loop_smoke_args(args)?;
            feedback_loop_smoke(request.review_id, request.max_wait_secs, json).await
        }
        "batch-create" => {
            let command = parse_batch_create_args(args)?;
            batch_command(command, dry_run, json).await
        }
        "batch-run" => {
            let command = parse_batch_run_args(args)?;
            batch_command(command, dry_run, json).await
        }
        "batch-status" => {
            let batch_id = parse_uuid_arg(args.first(), "batch_id")?;
            batch_command(BatchCommand::Status { batch_id }, dry_run, json).await
        }
        "batch-list" => {
            let limit = parse_optional_limit(args, 20)?;
            batch_command(BatchCommand::List { limit }, dry_run, json).await
        }
        "validate-citations" => anyhow::bail!(
            "validate-citations is executed by the GrokRxiv citation-validation DAG adapter"
        ),
        other => anyhow::bail!("unknown GrokRxiv action `{other}`"),
    }
}

async fn app_runs(app: Option<&str>, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state_obj = super::AppState::from_config(config).await?;
    let pool = state_obj
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("app runs: DATABASE_URL not configured"))?;
    let rows = sqlx::query(
        "select id::text, app_id, action_id, state, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
         order by created_at desc \
         limit 50",
    )
    .bind(app)
    .fetch_all(pool)
    .await
    .context("list app runs")?;

    if json {
        let runs = rows
            .iter()
            .map(|row: &sqlx::postgres::PgRow| {
                use sqlx::Row as _;
                serde_json::json!({
                    "id": row.get::<String, _>(0),
                    "app_id": row.get::<String, _>(1),
                    "action_id": row.get::<String, _>(2),
                    "state": row.get::<String, _>(3),
                    "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>(4),
                    "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(5),
                    "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(6),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "runs": runs }))?
        );
    } else if rows.is_empty() {
        println!("No app runs found.");
    } else {
        for row in rows {
            use sqlx::Row as _;
            println!(
                "{} {}:{} {}",
                row.get::<String, _>(0),
                row.get::<String, _>(1),
                row.get::<String, _>(2),
                row.get::<String, _>(3)
            );
        }
    }
    Ok(())
}

async fn app_status(run_id: Uuid, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state_obj = super::AppState::from_config(config).await?;
    let pool = state_obj
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("app status: DATABASE_URL not configured"))?;
    let row = sqlx::query(
        "select id::text, app_id, action_id, state, input, output, error_code, error_message, \
                created_at, started_at, finished_at \
         from app_runs \
         where id = $1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .context("load app run status")?
    .ok_or_else(|| anyhow::anyhow!("app run not found: {run_id}"))?;

    use sqlx::Row as _;
    let payload = serde_json::json!({
        "id": row.get::<String, _>(0),
        "app_id": row.get::<String, _>(1),
        "action_id": row.get::<String, _>(2),
        "state": row.get::<String, _>(3),
        "input": row.get::<serde_json::Value, _>(4),
        "output": row.get::<serde_json::Value, _>(5),
        "error_code": row.get::<Option<String>, _>(6),
        "error_message": row.get::<Option<String>, _>(7),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>(8),
        "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(9),
        "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(10),
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "{} {}:{} {}",
            payload["id"].as_str().unwrap_or_default(),
            payload["app_id"].as_str().unwrap_or_default(),
            payload["action_id"].as_str().unwrap_or_default(),
            payload["state"].as_str().unwrap_or_default()
        );
    }
    Ok(())
}

struct ResearchReviewArgs {
    source: String,
    source_type: Option<SourceType>,
    rev: Option<String>,
    paper_path: Option<PathBuf>,
    title: Option<String>,
    field: Option<String>,
    corpus: bool,
    scan_root: Option<PathBuf>,
    limit: Option<usize>,
    include: Vec<String>,
    exclude: Vec<String>,
    loop_enabled: bool,
    debug_output: bool,
    no_external_actions: bool,
}

struct IngestArgs {
    arxiv_ids: Vec<String>,
    auto_moderate: bool,
}

struct IngestRangeArgs {
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
    categories: Option<String>,
    no_review: bool,
}

struct ReviewExtractedArgs {
    source: String,
    force: bool,
}

struct ReviewIdActionArgs {
    review_id: Uuid,
    force: bool,
}

struct ReviewIdNotesArgs {
    review_id: Uuid,
    notes: Option<String>,
}

struct ReviewIdReasonArgs {
    review_id: Uuid,
    reason: String,
}

struct RenderArgs {
    review_id: Uuid,
    format: RenderFormat,
    out: Option<PathBuf>,
}

struct CloseArgs {
    review_id: Uuid,
    reason: String,
    keep_github_pr: bool,
}

struct CorrectArgs {
    review_id: Uuid,
    rationale_md: PathBuf,
}

struct HtmlReviewArgs {
    review_id: Option<Uuid>,
    all: bool,
}

struct FeedbackLoopSmokeArgs {
    review_id: Uuid,
    max_wait_secs: u64,
}

fn parse_grokrxiv_review_args(args: Vec<String>) -> anyhow::Result<ResearchReviewArgs> {
    let mut iter = args.into_iter();
    let source = iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("GrokRxiv review requires a source"))?;
    let mut parsed = ResearchReviewArgs {
        source,
        source_type: None,
        rev: None,
        paper_path: None,
        title: None,
        field: None,
        corpus: false,
        scan_root: None,
        limit: None,
        include: Vec::new(),
        exclude: Vec::new(),
        loop_enabled: false,
        debug_output: false,
        no_external_actions: false,
    };

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--type" => {
                let value = next_arg(&mut iter, "--type")?;
                parsed.source_type = Some(parse_source_type(&value)?);
            }
            "--rev" => parsed.rev = Some(next_arg(&mut iter, "--rev")?),
            "--paper-path" => {
                parsed.paper_path = Some(PathBuf::from(next_arg(&mut iter, "--paper-path")?))
            }
            "--title" => parsed.title = Some(next_arg(&mut iter, "--title")?),
            "--field" => parsed.field = Some(next_arg(&mut iter, "--field")?),
            "--corpus" => parsed.corpus = true,
            "--scan-root" => {
                parsed.scan_root = Some(PathBuf::from(next_arg(&mut iter, "--scan-root")?))
            }
            "--limit" => {
                let value = next_arg(&mut iter, "--limit")?;
                parsed.limit = Some(
                    value
                        .parse()
                        .context("--limit must be a positive integer")?,
                );
            }
            "--include" => parsed.include.push(next_arg(&mut iter, "--include")?),
            "--exclude" => parsed.exclude.push(next_arg(&mut iter, "--exclude")?),
            "--loop" => parsed.loop_enabled = true,
            "--debug" => parsed.debug_output = true,
            "--no-external-actions" => parsed.no_external_actions = true,
            other => anyhow::bail!("unknown GrokRxiv review argument `{other}`"),
        }
    }
    Ok(parsed)
}

fn parse_ingest_args(args: Vec<String>) -> anyhow::Result<IngestArgs> {
    let mut arxiv_ids = Vec::new();
    let mut auto_moderate = false;
    for arg in args {
        if arg == "--auto-moderate" {
            auto_moderate = true;
        } else {
            arxiv_ids.push(arg);
        }
    }
    if arxiv_ids.is_empty() {
        anyhow::bail!("GrokRxiv ingest requires at least one arXiv id");
    }
    Ok(IngestArgs {
        arxiv_ids,
        auto_moderate,
    })
}

fn parse_ingest_range_args(args: Vec<String>) -> anyhow::Result<IngestRangeArgs> {
    let mut iter = args.into_iter();
    let mut from = None;
    let mut to = None;
    let mut categories = None;
    let mut no_review = false;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                from = Some(
                    next_arg(&mut iter, "--from")?
                        .parse()
                        .context("--from must be YYYY-MM-DD")?,
                )
            }
            "--to" => {
                to = Some(
                    next_arg(&mut iter, "--to")?
                        .parse()
                        .context("--to must be YYYY-MM-DD")?,
                )
            }
            "--categories" => categories = Some(next_arg(&mut iter, "--categories")?),
            "--no-review" => no_review = true,
            other => anyhow::bail!("unexpected ingest-range argument `{other}`"),
        }
    }
    Ok(IngestRangeArgs {
        from: from.ok_or_else(|| anyhow::anyhow!("ingest-range requires --from"))?,
        to: to.ok_or_else(|| anyhow::anyhow!("ingest-range requires --to"))?,
        categories,
        no_review,
    })
}

fn parse_grokrxiv_list_args(args: Vec<String>, json: bool) -> anyhow::Result<ListKind> {
    let mut iter = args.into_iter();
    let what = iter.next().ok_or_else(|| {
        anyhow::anyhow!("GrokRxiv list requires `reviews`, `papers`, or `extracted`")
    })?;
    match what.as_str() {
        "reviews" => {
            let mut review_status = None;
            let mut field = None;
            let mut limit = 20;
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--review-status" | "--state" => {
                        review_status = Some(next_arg(&mut iter, "--review-status")?)
                    }
                    "--field" => field = Some(next_arg(&mut iter, "--field")?),
                    "--limit" => {
                        limit = next_arg(&mut iter, "--limit")?
                            .parse()
                            .context("--limit must be a positive integer")?;
                    }
                    other => anyhow::bail!("unexpected GrokRxiv list reviews argument `{other}`"),
                }
            }
            Ok(ListKind::Reviews {
                review_status,
                field,
                limit,
                json,
            })
        }
        "papers" => {
            let mut field = None;
            let mut has_review = false;
            let mut extracted = false;
            let mut limit = 20;
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--field" => field = Some(next_arg(&mut iter, "--field")?),
                    "--has-review" => has_review = true,
                    "--extracted" => extracted = true,
                    "--limit" => {
                        limit = next_arg(&mut iter, "--limit")?
                            .parse()
                            .context("--limit must be a positive integer")?;
                    }
                    other => anyhow::bail!("unexpected GrokRxiv list papers argument `{other}`"),
                }
            }
            Ok(ListKind::Papers {
                field,
                has_review,
                extracted,
                limit,
                json,
            })
        }
        "extracted" => {
            let mut field = None;
            let mut limit = 20;
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--field" => field = Some(next_arg(&mut iter, "--field")?),
                    "--limit" => {
                        limit = next_arg(&mut iter, "--limit")?
                            .parse()
                            .context("--limit must be a positive integer")?;
                    }
                    other => anyhow::bail!("unexpected GrokRxiv list extracted argument `{other}`"),
                }
            }
            Ok(ListKind::Extracted { field, limit, json })
        }
        other => anyhow::bail!("unknown GrokRxiv list target `{other}`"),
    }
}

fn parse_review_extracted_args(args: Vec<String>) -> anyhow::Result<ReviewExtractedArgs> {
    let mut force = false;
    let mut source = None;
    for arg in args {
        if arg == "--force" {
            force = true;
        } else if source.is_none() {
            source = Some(arg);
        } else {
            anyhow::bail!("unexpected review-extracted argument `{arg}`");
        }
    }
    Ok(ReviewExtractedArgs {
        source: source.ok_or_else(|| anyhow::anyhow!("review-extracted requires a source"))?,
        force,
    })
}

fn parse_review_id_action_args(
    args: Vec<String>,
    action: &str,
    force_flag: &str,
) -> anyhow::Result<ReviewIdActionArgs> {
    let mut force = false;
    let mut review_id = None;
    for arg in args {
        if arg == format!("--{force_flag}") {
            force = true;
        } else if review_id.is_none() {
            review_id = Some(parse_uuid_arg(Some(&arg), "review_id")?);
        } else {
            anyhow::bail!("unexpected {action} argument `{arg}`");
        }
    }
    Ok(ReviewIdActionArgs {
        review_id: review_id.ok_or_else(|| anyhow::anyhow!("{action} requires review_id"))?,
        force,
    })
}

fn parse_review_id_notes_args(
    args: Vec<String>,
    action: &str,
    notes_required: bool,
) -> anyhow::Result<ReviewIdNotesArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut notes = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--notes" => notes = Some(next_arg(&mut iter, "--notes")?),
            other => anyhow::bail!("unexpected {action} argument `{other}`"),
        }
    }
    if notes_required && notes.as_deref().unwrap_or("").trim().is_empty() {
        anyhow::bail!("{action} requires --notes");
    }
    Ok(ReviewIdNotesArgs { review_id, notes })
}

fn parse_review_id_reason_args(
    args: Vec<String>,
    action: &str,
) -> anyhow::Result<ReviewIdReasonArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut reason = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--reason" => reason = Some(next_arg(&mut iter, "--reason")?),
            other => anyhow::bail!("unexpected {action} argument `{other}`"),
        }
    }
    Ok(ReviewIdReasonArgs {
        review_id,
        reason: reason.ok_or_else(|| anyhow::anyhow!("{action} requires --reason"))?,
    })
}

fn parse_render_args(args: Vec<String>) -> anyhow::Result<RenderArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut format = RenderFormat::Html;
    let mut out = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--format" => format = parse_render_format(&next_arg(&mut iter, "--format")?)?,
            "--out" => out = Some(PathBuf::from(next_arg(&mut iter, "--out")?)),
            other => anyhow::bail!("unexpected render argument `{other}`"),
        }
    }
    Ok(RenderArgs {
        review_id,
        format,
        out,
    })
}

fn parse_close_args(args: Vec<String>) -> anyhow::Result<CloseArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut reason = None;
    let mut keep_github_pr = false;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--reason" => reason = Some(next_arg(&mut iter, "--reason")?),
            "--keep-github-pr" => keep_github_pr = true,
            other => anyhow::bail!("unexpected close argument `{other}`"),
        }
    }
    Ok(CloseArgs {
        review_id,
        reason: reason.ok_or_else(|| anyhow::anyhow!("close requires --reason"))?,
        keep_github_pr,
    })
}

fn parse_correct_args(args: Vec<String>) -> anyhow::Result<CorrectArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut rationale_md = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--rationale-md" => {
                rationale_md = Some(PathBuf::from(next_arg(&mut iter, "--rationale-md")?))
            }
            other => anyhow::bail!("unexpected correct argument `{other}`"),
        }
    }
    Ok(CorrectArgs {
        review_id,
        rationale_md: rationale_md
            .ok_or_else(|| anyhow::anyhow!("correct requires --rationale-md"))?,
    })
}

fn parse_html_review_args(args: Vec<String>) -> anyhow::Result<HtmlReviewArgs> {
    let mut review_id = None;
    let mut all = false;
    for arg in args {
        if arg == "--all" {
            all = true;
        } else if review_id.is_none() {
            review_id = Some(arg.parse().context("invalid review_id")?);
        } else {
            anyhow::bail!("unexpected html-review argument `{arg}`");
        }
    }
    if !all && review_id.is_none() {
        anyhow::bail!("html-review requires review_id or --all");
    }
    Ok(HtmlReviewArgs { review_id, all })
}

fn parse_feedback_loop_smoke_args(args: Vec<String>) -> anyhow::Result<FeedbackLoopSmokeArgs> {
    let mut iter = args.into_iter();
    let review_id = parse_uuid_arg(iter.next().as_ref(), "review_id")?;
    let mut max_wait_secs = 3600;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--max-wait-secs" => {
                max_wait_secs = next_arg(&mut iter, "--max-wait-secs")?
                    .parse()
                    .context("--max-wait-secs must be a positive integer")?;
            }
            other => anyhow::bail!("unexpected feedback-loop-smoke argument `{other}`"),
        }
    }
    Ok(FeedbackLoopSmokeArgs {
        review_id,
        max_wait_secs,
    })
}

fn parse_batch_create_args(args: Vec<String>) -> anyhow::Result<BatchCommand> {
    let mut iter = args.into_iter();
    let mut category = None;
    let mut month = None;
    let mut daily_limit = 30;
    let mut max_items = None;
    let mut auto_pr = false;
    let mut start_date = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--category" => category = Some(next_arg(&mut iter, "--category")?),
            "--month" => month = Some(next_arg(&mut iter, "--month")?),
            "--daily-limit" => {
                daily_limit = next_arg(&mut iter, "--daily-limit")?
                    .parse()
                    .context("--daily-limit must be a positive integer")?
            }
            "--max-items" => {
                max_items = Some(
                    next_arg(&mut iter, "--max-items")?
                        .parse()
                        .context("--max-items must be a positive integer")?,
                )
            }
            "--auto-pr" => auto_pr = true,
            "--start-date" => {
                start_date = Some(
                    next_arg(&mut iter, "--start-date")?
                        .parse()
                        .context("--start-date must be YYYY-MM-DD")?,
                )
            }
            other => anyhow::bail!("unexpected batch-create argument `{other}`"),
        }
    }
    Ok(BatchCommand::Create {
        category: category.ok_or_else(|| anyhow::anyhow!("batch-create requires --category"))?,
        month: month.ok_or_else(|| anyhow::anyhow!("batch-create requires --month"))?,
        daily_limit,
        max_items,
        auto_pr,
        start_date,
    })
}

fn parse_batch_run_args(args: Vec<String>) -> anyhow::Result<BatchCommand> {
    let mut iter = args.into_iter();
    let batch_id = parse_uuid_arg(iter.next().as_ref(), "batch_id")?;
    let mut today = None;
    let mut limit = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--today" => {
                today = Some(
                    next_arg(&mut iter, "--today")?
                        .parse()
                        .context("--today must be YYYY-MM-DD")?,
                )
            }
            "--limit" => {
                limit = Some(
                    next_arg(&mut iter, "--limit")?
                        .parse()
                        .context("--limit must be a positive integer")?,
                )
            }
            other => anyhow::bail!("unexpected batch-run argument `{other}`"),
        }
    }
    Ok(BatchCommand::Run {
        batch_id,
        today,
        limit,
    })
}

fn parse_optional_limit(args: Vec<String>, default: u32) -> anyhow::Result<u32> {
    let mut iter = args.into_iter();
    let mut limit = default;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--limit" => {
                limit = next_arg(&mut iter, "--limit")?
                    .parse()
                    .context("--limit must be a positive integer")?
            }
            other => anyhow::bail!("unexpected list argument `{other}`"),
        }
    }
    Ok(limit)
}

fn parse_uuid_arg(value: Option<&String>, name: &str) -> anyhow::Result<Uuid> {
    value
        .ok_or_else(|| anyhow::anyhow!("missing {name}"))?
        .parse()
        .with_context(|| format!("invalid {name}"))
}

fn ensure_args_not_empty(args: &[String], message: &str) -> anyhow::Result<()> {
    if args.is_empty() {
        anyhow::bail!("{message}");
    }
    Ok(())
}

fn ensure_args_empty(args: &[String], message: &str) -> anyhow::Result<()> {
    if !args.is_empty() {
        anyhow::bail!("{message}");
    }
    Ok(())
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    iter.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn parse_source_type(value: &str) -> anyhow::Result<SourceType> {
    match value {
        "arxiv" => Ok(SourceType::Arxiv),
        "pdf" => Ok(SourceType::Pdf),
        "tex" => Ok(SourceType::Tex),
        "git" => Ok(SourceType::Git),
        "mixed" => Ok(SourceType::Mixed),
        _ => anyhow::bail!("invalid source type `{value}`"),
    }
}

fn parse_render_format(value: &str) -> anyhow::Result<RenderFormat> {
    match value {
        "html" => Ok(RenderFormat::Html),
        "md" => Ok(RenderFormat::Md),
        "tex" => Ok(RenderFormat::Tex),
        "pdf" => Ok(RenderFormat::Pdf),
        "zip" => Ok(RenderFormat::Zip),
        _ => anyhow::bail!("invalid render format `{value}`"),
    }
}

async fn dag_command(command: DagCommand, json: bool) -> anyhow::Result<()> {
    match command {
        DagCommand::Validate { dag_type } => validate_dag_manifests(dag_type.as_deref(), json),
        DagCommand::Run { dag_type } => run_dag_app_command(&dag_type, json).await,
        DagCommand::AddAgent {
            dag_type,
            role_id,
            kind,
            config,
            after,
            before,
            write,
        } => add_agent_to_dag(
            &dag_type, &role_id, &kind, config, after, before, write, json,
        ),
        DagCommand::RemoveAgent {
            dag_type,
            role_id,
            write,
        } => remove_agent_from_dag(&dag_type, &role_id, write, json),
        DagCommand::AddTool {
            dag_type,
            tool_id,
            executor,
            handler,
            command,
            after,
            before,
            inputs,
            outputs,
            timeout_secs,
            write,
        } => add_tool_to_dag(
            &dag_type,
            &tool_id,
            &executor,
            handler,
            command,
            after,
            before,
            inputs,
            outputs,
            timeout_secs,
            write,
            json,
        ),
        DagCommand::RemoveTool {
            dag_type,
            tool_id,
            write,
        } => remove_tool_from_dag(&dag_type, &tool_id, write, json),
        DagCommand::ScaffoldTool {
            dag_type,
            tool_id,
            handler,
            after,
            before,
            inputs,
            outputs,
            timeout_secs,
            write,
        } => scaffold_tool_for_dag(
            &dag_type,
            &tool_id,
            handler,
            after,
            before,
            inputs,
            outputs,
            timeout_secs,
            write,
            json,
        ),
    }
}

async fn agent_command(command: AgentCommand, json: bool) -> anyhow::Result<()> {
    match command {
        AgentCommand::Place { path } => place_agent(&path, json),
    }
}

async fn run_dag_app_command(dag_type: &str, json: bool) -> anyhow::Result<()> {
    let report =
        crate::dag_apps::run_registered_dag_app(dag_type, agenthero_dag_executor::DagIo::default())
            .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let status = serde_json::to_string(&report.status)?
            .trim_matches('"')
            .to_string();
        println!(
            "ok {} status={} nodes={}",
            report.dag_type,
            status,
            report.nodes.len()
        );
    }
    Ok(())
}

fn validate_dag_manifests(dag_type: Option<&str>, json: bool) -> anyhow::Result<()> {
    let manifests = load_repo_dag_manifests(dag_type)?;
    for manifest in &manifests {
        validate_declared_agent_configs(manifest)?;
        validate_declared_tools(manifest)?;
    }
    if json {
        let rows: Vec<serde_json::Value> = manifests
            .iter()
            .map(|manifest| {
                serde_json::json!({
                    "id": manifest.id.as_str(),
                    "version": manifest.version,
                    "roles": manifest.roles.len(),
                    "nodes": manifest.nodes.len(),
                    "layers": manifest.execution_layers().map(|layers| layers.len()).unwrap_or(0),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "dags": rows }))?
        );
    } else {
        for manifest in &manifests {
            let layer_count = manifest.execution_layers()?.len();
            println!(
                "ok {} version={} roles={} nodes={} layers={}",
                manifest.id.as_str(),
                manifest.version,
                manifest.roles.len(),
                manifest.nodes.len(),
                layer_count
            );
        }
    }
    Ok(())
}

fn validate_declared_tools(manifest: &DagManifest) -> anyhow::Result<()> {
    for tool in &manifest.tools {
        match tool.executor {
            ToolExecutorKind::Rust => {
                let handler = tool.handler.as_deref().unwrap_or(tool.id.as_str());
                if !crate::dag_tools::is_known_rust_tool_handler(handler) {
                    let known = crate::dag_tools::known_rust_tool_handlers().join(", ");
                    anyhow::bail!(
                        "DAG `{}` tool `{}` declares unknown Rust handler `{}`; known handlers: {}",
                        manifest.id,
                        tool.id,
                        handler,
                        known
                    );
                }
            }
            ToolExecutorKind::Cli => {
                if tool
                    .command
                    .as_ref()
                    .map(|command| command.is_empty())
                    .unwrap_or(true)
                {
                    anyhow::bail!(
                        "DAG `{}` CLI tool `{}` must declare command",
                        manifest.id,
                        tool.id
                    );
                }
            }
        }
    }
    Ok(())
}

fn add_agent_to_dag(
    dag_type: &str,
    role_id: &str,
    kind: &str,
    config: Option<String>,
    after: Vec<String>,
    before: Vec<String>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let path = dag_manifest_path(dag_type);
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let kind = parse_agent_kind_arg(kind)?;
    if manifest
        .roles
        .iter()
        .any(|role| role.id.as_str() == role_id)
    {
        anyhow::bail!("DAG `{dag_type}` already has role `{role_id}`");
    }
    if manifest.nodes.iter().any(|node| node.id == role_id) {
        anyhow::bail!("DAG `{dag_type}` already has node `{role_id}`");
    }

    manifest.roles.push(DagRole {
        id: RoleId::new(role_id),
        kind: kind.clone(),
        config: Some(config.unwrap_or_else(|| format!("agents/{dag_type}/{role_id}.yaml"))),
    });
    manifest.nodes.push(DagNode {
        id: role_id.to_string(),
        kind: default_node_kind_for_agent_kind(&kind),
        role: Some(RoleId::new(role_id)),
        tool: None,
        dag_type: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        required: false,
        feeds_meta: false,
        gate: None,
        loop_policy: None,
        branch: None,
        map: None,
        approval: None,
    });
    for source in after {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(source),
            to: OneOrMany::One(role_id.to_string()),
        });
    }
    for target in before {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(role_id.to_string()),
            to: OneOrMany::One(target),
        });
    }
    manifest.validate()?;
    validate_declared_tools(&manifest)?;
    emit_or_write_manifest(&path, &manifest, write, json)
}

fn remove_agent_from_dag(
    dag_type: &str,
    role_id: &str,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let path = dag_manifest_path(dag_type);
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let before_roles = manifest.roles.len();
    manifest.roles.retain(|role| role.id.as_str() != role_id);
    if before_roles == manifest.roles.len() {
        anyhow::bail!("DAG `{dag_type}` has no role `{role_id}`");
    }
    let removed_node_ids: std::collections::HashSet<String> = manifest
        .nodes
        .iter()
        .filter(|node| {
            node.role
                .as_ref()
                .map(|role| role.as_str() == role_id)
                .unwrap_or(false)
        })
        .map(|node| node.id.clone())
        .collect();
    manifest
        .nodes
        .retain(|node| !removed_node_ids.contains(&node.id));
    manifest.edges = manifest
        .edges
        .into_iter()
        .filter_map(|edge| {
            Some(DagEdge {
                from: strip_one_or_many(edge.from, &removed_node_ids)?,
                to: strip_one_or_many(edge.to, &removed_node_ids)?,
            })
        })
        .collect();
    manifest.validate()?;
    validate_declared_tools(&manifest)?;
    emit_or_write_manifest(&path, &manifest, write, json)
}

#[allow(clippy::too_many_arguments)]
fn add_tool_to_dag(
    dag_type: &str,
    tool_id: &str,
    executor: &str,
    handler: Option<String>,
    command: Vec<String>,
    after: Vec<String>,
    before: Vec<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    timeout_secs: Option<u64>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let path = dag_manifest_path(dag_type);
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    if manifest.tools.iter().any(|tool| tool.id == tool_id) {
        anyhow::bail!("DAG `{dag_type}` already has tool `{tool_id}`");
    }
    if manifest.nodes.iter().any(|node| node.id == tool_id) {
        anyhow::bail!("DAG `{dag_type}` already has node `{tool_id}`");
    }
    let executor = parse_tool_executor_arg(executor)?;
    let handler = match executor {
        ToolExecutorKind::Rust => Some(handler.unwrap_or_else(|| tool_id.to_string())),
        ToolExecutorKind::Cli => handler,
    };
    let command = (!command.is_empty()).then_some(command);
    manifest.tools.push(DagTool {
        id: tool_id.to_string(),
        executor,
        handler,
        command,
        timeout_secs,
        input_schema: None,
        output_schema: None,
    });
    manifest.nodes.push(DagNode {
        id: tool_id.to_string(),
        kind: DagNodeKind::Tool,
        role: None,
        tool: Some(tool_id.to_string()),
        dag_type: None,
        inputs,
        outputs,
        required: true,
        feeds_meta: false,
        gate: None,
        loop_policy: None,
        branch: None,
        map: None,
        approval: None,
    });
    for source in after {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(source),
            to: OneOrMany::One(tool_id.to_string()),
        });
    }
    for target in before {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(tool_id.to_string()),
            to: OneOrMany::One(target),
        });
    }
    manifest.validate()?;
    validate_declared_tools(&manifest)?;
    emit_or_write_manifest(&path, &manifest, write, json)
}

fn remove_tool_from_dag(
    dag_type: &str,
    tool_id: &str,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let path = dag_manifest_path(dag_type);
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let before_tools = manifest.tools.len();
    manifest.tools.retain(|tool| tool.id != tool_id);
    if before_tools == manifest.tools.len() {
        anyhow::bail!("DAG `{dag_type}` has no tool `{tool_id}`");
    }
    let removed_node_ids: std::collections::HashSet<String> = manifest
        .nodes
        .iter()
        .filter(|node| node.tool.as_deref() == Some(tool_id))
        .map(|node| node.id.clone())
        .collect();
    manifest
        .nodes
        .retain(|node| !removed_node_ids.contains(&node.id));
    manifest.edges = manifest
        .edges
        .into_iter()
        .filter_map(|edge| {
            Some(DagEdge {
                from: strip_one_or_many(edge.from, &removed_node_ids)?,
                to: strip_one_or_many(edge.to, &removed_node_ids)?,
            })
        })
        .collect();
    manifest.validate()?;
    validate_declared_tools(&manifest)?;
    emit_or_write_manifest(&path, &manifest, write, json)
}

#[allow(clippy::too_many_arguments)]
fn scaffold_tool_for_dag(
    dag_type: &str,
    tool_id: &str,
    handler: Option<String>,
    after: Vec<String>,
    before: Vec<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    timeout_secs: Option<u64>,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    add_tool_to_dag(
        dag_type,
        tool_id,
        "rust",
        handler,
        Vec::new(),
        after,
        before,
        inputs,
        outputs,
        timeout_secs,
        write,
        json,
    )
}

fn parse_agent_kind_arg(raw: &str) -> anyhow::Result<AgentKind> {
    serde_yaml::from_value(serde_yaml::Value::String(raw.to_string()))
        .with_context(|| format!("unknown agent kind `{raw}`"))
}

fn parse_tool_executor_arg(raw: &str) -> anyhow::Result<ToolExecutorKind> {
    serde_yaml::from_value(serde_yaml::Value::String(raw.to_string()))
        .with_context(|| format!("unknown tool executor `{raw}`"))
}

fn default_node_kind_for_agent_kind(kind: &AgentKind) -> DagNodeKind {
    match kind {
        AgentKind::Synthesizer => DagNodeKind::Synthesizer,
        AgentKind::Renderer => DagNodeKind::RenderArtifacts,
        AgentKind::Verifier => DagNodeKind::Verify,
        AgentKind::Extractor
        | AgentKind::Critic
        | AgentKind::TypeTheoryValidator
        | AgentKind::CodeGenerator => DagNodeKind::Agent,
    }
}

fn strip_one_or_many(
    values: OneOrMany,
    needles: &std::collections::HashSet<String>,
) -> Option<OneOrMany> {
    match values {
        OneOrMany::One(value) => (!needles.contains(&value)).then_some(OneOrMany::One(value)),
        OneOrMany::Many(values) => {
            let kept: Vec<String> = values
                .into_iter()
                .filter(|value| !needles.contains(value))
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(OneOrMany::One),
                _ => Some(OneOrMany::Many(kept)),
            }
        }
    }
}

fn emit_or_write_manifest(
    path: &Path,
    manifest: &DagManifest,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let text = serde_yaml::to_string(manifest)?;
    if write {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "updated": path.display().to_string(),
                    "dag_type": manifest.id.as_str(),
                }))?
            );
        } else {
            println!("updated {}", path.display());
        }
    } else {
        println!("{text}");
    }
    Ok(())
}

fn validate_declared_agent_configs(manifest: &DagManifest) -> anyhow::Result<()> {
    let manifest_path = dag_manifest_path(manifest.id.as_str());
    let repo_root = manifest_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    for role in &manifest.roles {
        let Some(config) = role.config.as_deref() else {
            continue;
        };
        let path = resolve_agent_config_path(repo_root, config);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "DAG `{}` role `{}` config {}",
                manifest.id,
                role.id,
                path.display()
            )
        })?;
        let value: serde_yaml::Value = serde_yaml::from_str(&text)
            .with_context(|| format!("parse agent YAML {}", path.display()))?;
        let Some(kind_value) = value.get("kind").cloned() else {
            anyhow::bail!(
                "DAG `{}` role `{}` config {} is missing `kind`",
                manifest.id,
                role.id,
                path.display()
            );
        };
        let actual_kind: AgentKind = serde_yaml::from_value(kind_value)
            .with_context(|| format!("parse `kind` in {}", path.display()))?;
        if actual_kind != role.kind {
            anyhow::bail!(
                "DAG `{}` role `{}` declares kind `{}`, but {} declares `{}`",
                manifest.id,
                role.id,
                role.kind,
                path.display(),
                actual_kind
            );
        }
    }
    Ok(())
}

fn place_agent(path: &Path, json: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read agent YAML {}", path.display()))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("parse agent YAML {}", path.display()))?;
    let kind_value = value
        .get("kind")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent YAML {} is missing `kind`", path.display()))?;
    let kind: AgentKind = serde_yaml::from_value(kind_value)
        .with_context(|| format!("parse `kind` in {}", path.display()))?;
    let manifests = load_repo_dag_manifests(None)?;
    let compatible = DagManifest::compatible_dag_ids(&manifests, kind.clone());

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "kind": kind.to_string(),
                "compatible_dags": compatible,
            }))?
        );
    } else {
        println!("kind={kind}");
        for dag in compatible {
            println!("{dag}");
        }
    }
    Ok(())
}

fn load_repo_dag_manifests(dag_type: Option<&str>) -> anyhow::Result<Vec<DagManifest>> {
    let mut paths = Vec::new();
    if let Some(id) = dag_type {
        paths.push(dag_manifest_path(id));
    } else {
        for app in crate::dag_apps::registered_dag_apps()? {
            paths.push(app.manifest_path);
        }
        paths.sort();
    }

    paths
        .into_iter()
        .map(|path| {
            DagManifest::from_path(&path)
                .with_context(|| format!("validate DAG manifest {}", path.display()))
        })
        .collect()
}

fn dag_manifest_path(dag_type: &str) -> PathBuf {
    if let Ok(path) = std::env::var("AGENTHERO_DAGS_DIR") {
        return PathBuf::from(path).join(format!("{dag_type}.yaml"));
    }
    if let Some(app) = crate::dag_apps::registered_dag_app(dag_type) {
        return app.manifest_path;
    }
    crate::dag_apps::apps_root()
        .join("unknown")
        .join("dags")
        .join(format!("{dag_type}.yaml"))
}

fn resolve_agent_config_path(repo_root: &Path, config: &str) -> PathBuf {
    let path = PathBuf::from(config);
    if path.is_absolute() {
        return path;
    }
    if let Some(agents_dir) = std::env::var_os("AGENTHERO_AGENTS_DIR").map(PathBuf::from) {
        if let Ok(stripped) = path.strip_prefix("agents") {
            return agents_dir.join(stripped);
        }
    }
    repo_root.join(path)
}

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
    eprintln!(
        "`migrate` is handled by `bash agenthero/apps/grokrxiv/infra/supabase/setup.sh` in this checkout."
    );
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

async fn ingest_many(arxiv_ids: &[String], auto_moderate: bool, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());

    if arxiv_ids.len() <= 1 {
        // Single-paper output is kept stable for shell smoke checks.
        for id in arxiv_ids {
            emit_pipeline_header("ingest", id);
            let review_id =
                super::supervisor::run_one_paper_blocking(&supervisor, &state, id).await?;
            crate::cli_status::emit(format!(
                "paper {id}: review_id={review_id} awaiting human moderation"
            ));
            if auto_moderate {
                if let Err(e) = auto_moderate_review(&state, review_id, json).await {
                    tracing::warn!(%review_id, err = %e, "auto-moderate dispatch failed; review left at awaiting_moderation");
                }
            }
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

    // Fan out review DAGs while arXiv fetches remain serialized through the
    // ingest crate's rate limiter.
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
                if auto_moderate {
                    if let Err(e) = auto_moderate_review(&state, review_id, json).await {
                        tracing::warn!(%review_id, err = %e, "auto-moderate dispatch failed; review left at awaiting_moderation");
                    }
                }
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
        let repo_root = data_repo_root()?;
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

fn data_repo_root() -> anyhow::Result<PathBuf> {
    std::env::var("GROKRXIV_DATA_REPO_PATH")
        .map(PathBuf::from)
        .map_err(|_| {
            anyhow::anyhow!(
                "GROKRXIV_DATA_REPO_PATH is required; point it at the grokrxiv-data Git repo"
            )
        })
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
#[derive(Debug)]
struct ReviewLoopPaperMathSourceFiles {
    artifact_sources: Vec<String>,
    body: serde_json::Value,
    equations: serde_json::Value,
    theorem_graph: serde_json::Value,
    semantic_ast: serde_json::Value,
}

#[cfg(feature = "grokrxiv-storage")]
fn load_review_loop_paper_math_source_files(
    repo_root: &Path,
    review_input_path: &Path,
) -> anyhow::Result<ReviewLoopPaperMathSourceFiles> {
    let bytes = std::fs::read(review_input_path)
        .with_context(|| format!("read review_input.json at {}", review_input_path.display()))?;
    let review_input: grokrxiv_storage::ReviewInput = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse review_input.json at {}", review_input_path.display()))?;

    let sections_doc = read_review_json(repo_root, &review_input.sections, "sections.json")?;
    let body_text = read_review_text(repo_root, &review_input.body_markdown, "body.md")?;
    let equations = read_review_json(repo_root, &review_input.equations, "equations.json")?;
    let theorem_graph =
        read_review_json(repo_root, &review_input.theorem_graph, "theorem_graph.json")?;

    let mut artifact_sources = vec![
        format!("review_input:{}", review_input_path.display()),
        "sections.json".to_string(),
        "body.md".to_string(),
        "equations.json".to_string(),
        "theorem_graph.json".to_string(),
    ];
    let mut semantic_ast = serde_json::Value::Null;
    if let Some(uri) = review_input.semantic_ast_uri.as_deref() {
        if !uri.starts_with("supabase://") {
            semantic_ast = read_review_json(repo_root, uri, "semantic_ast.json")?;
            artifact_sources.push("semantic_ast.json".to_string());
        }
    }

    Ok(ReviewLoopPaperMathSourceFiles {
        artifact_sources,
        body: serde_json::json!({
            "artifact": "body.md",
            "text": body_text,
            "sections": sections_doc
                .get("sections")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        }),
        equations,
        theorem_graph,
        semantic_ast,
    })
}

#[cfg(feature = "grokrxiv-storage")]
fn load_review_loop_paper_math_sources_from_data_repo_cache(
    repo_root: &Path,
    arxiv_id: &str,
) -> anyhow::Result<Option<ReviewLoopPaperMathSourceFiles>> {
    let base_id = strip_arxiv_version(arxiv_id).to_string();
    let mut candidates = vec![base_id];
    if candidates.first().map(String::as_str) != Some(arxiv_id) {
        candidates.push(arxiv_id.to_string());
    }

    for candidate in candidates {
        let review_input_path = repo_root
            .join("papers")
            .join(candidate)
            .join("review_input.json");
        if review_input_path.exists() {
            return load_review_loop_paper_math_source_files(repo_root, &review_input_path)
                .map(Some);
        }
    }

    Ok(None)
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
        if status == "failed" {
            failures.push(format!("extraction stage {name} failed"));
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

async fn batch_command(command: BatchCommand, dry_run: bool, json: bool) -> anyhow::Result<()> {
    match command {
        BatchCommand::Create {
            category,
            month,
            daily_limit,
            max_items,
            auto_pr,
            start_date,
        } => {
            batch_create(
                &category,
                &month,
                daily_limit,
                max_items,
                auto_pr,
                start_date,
                dry_run,
                json,
            )
            .await
        }
        BatchCommand::Run {
            batch_id,
            today,
            limit,
        } => batch_run(batch_id, today, limit, dry_run, json).await,
        BatchCommand::Status { batch_id } => batch_status(batch_id, json).await,
        BatchCommand::List { limit } => batch_list(limit, json).await,
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn batch_create(
    category: &str,
    month: &str,
    daily_limit: u32,
    max_items: Option<u32>,
    auto_pr: bool,
    start_date: Option<chrono::NaiveDate>,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    if daily_limit == 0 {
        anyhow::bail!("batch create: --daily-limit must be greater than zero");
    }
    if max_items == Some(0) {
        anyhow::bail!("batch create: --max-items must be greater than zero");
    }
    let (from, until) = crate::batch::parse_month_range(month)?;
    let today = chrono::Utc::now().date_naive();
    if from > today {
        anyhow::bail!("batch create: month `{month}` starts in the future");
    }
    let until = until.min(today);
    let start_date = start_date.unwrap_or(from);
    let daily_limit_usize = daily_limit as usize;
    let config = super::Config::from_env();
    let records = if let Some(max_items) = max_items {
        crate::cli_status::emit(format!(
            "batch create: fetching arXiv list page category={category} month={month} limit={max_items}"
        ));
        grokrxiv_ingest::fetch_list_page(
            category,
            month,
            max_items as usize,
            &config.arxiv_user_agent,
        )
        .await?
    } else {
        crate::cli_status::emit(format!(
            "batch create: fetching arXiv OAI listing category={category} from={from} until={until}"
        ));
        grokrxiv_ingest::fetch_listing(&[category], from, until, &config.arxiv_user_agent).await?
    };
    let options = crate::batch::BatchCreateOptions {
        category: category.to_string(),
        from,
        until,
        daily_limit: daily_limit_usize,
        max_items: max_items.map(|value| value as usize),
        auto_pr,
        start_date,
    };
    let result = if dry_run {
        crate::batch::preview_batch(&options, &records)
    } else {
        let state = super::AppState::from_config(config).await?;
        let pool = state
            .db
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("batch create: DATABASE_URL not configured"))?;
        crate::batch::create_batch(pool, &options, &records).await?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let batch_id = result
            .batch_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "<dry-run>".to_string());
        println!(
            "batch_id={batch_id} category={} from={} until={} discovered={} daily_limit={} scheduled_days={} auto_pr={}",
            result.category,
            result.from,
            result.until,
            result.discovered,
            result.daily_limit,
            result.scheduled_days,
            result.auto_pr
        );
        for item in result.first_items {
            println!(
                "  {:>4} {} {} {}",
                item.position + 1,
                item.scheduled_for,
                item.arxiv_id,
                truncate(&item.title, 80)
            );
        }
    }
    Ok(())
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn batch_create(
    category: &str,
    month: &str,
    daily_limit: u32,
    max_items: Option<u32>,
    auto_pr: bool,
    start_date: Option<chrono::NaiveDate>,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let _ = (
        category,
        month,
        daily_limit,
        max_items,
        auto_pr,
        start_date,
        dry_run,
        json,
    );
    anyhow::bail!("batch create requires --features full (grokrxiv-ingest)")
}

#[derive(Debug, Serialize)]
struct BatchRunOutput {
    batch_id: Uuid,
    today: chrono::NaiveDate,
    dry_run: bool,
    items: Vec<BatchRunItemOutput>,
}

#[derive(Debug, Serialize)]
struct BatchRunItemOutput {
    item_id: Uuid,
    arxiv_id: String,
    state: String,
    paper_id: Option<Uuid>,
    review_id: Option<Uuid>,
    pr_url: Option<String>,
    error: Option<String>,
}

#[cfg(feature = "grokrxiv-ingest")]
async fn batch_run(
    batch_id: Uuid,
    today: Option<chrono::NaiveDate>,
    limit: Option<u32>,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let today = today.unwrap_or_else(|| chrono::Utc::now().date_naive());
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("batch run: DATABASE_URL not configured"))?;
    let batch = crate::batch::load_batch(pool, batch_id).await?;
    let limit = limit.unwrap_or(batch.daily_limit as u32).max(1) as i64;

    if dry_run {
        let due = crate::batch::due_batch_items(pool, batch_id, today, limit).await?;
        let output = BatchRunOutput {
            batch_id,
            today,
            dry_run: true,
            items: due
                .into_iter()
                .map(|item| BatchRunItemOutput {
                    item_id: item.id,
                    arxiv_id: item.arxiv_id,
                    state: "due".to_string(),
                    paper_id: item.paper_id,
                    review_id: item.review_id,
                    pr_url: item.pr_url,
                    error: item.error,
                })
                .collect(),
        };
        print_batch_run_output(&output, json)?;
        return Ok(());
    }

    let supervisor = super::supervisor::Supervisor::spawn(state.clone());
    let due = crate::batch::claim_due_batch_items(pool, batch_id, today, limit).await?;
    let mut outputs = Vec::with_capacity(due.len());
    let mut failures = Vec::new();

    for item in due {
        crate::cli_status::emit(format!(
            "batch {batch_id}: reviewing {} ({}/{})",
            item.arxiv_id,
            item.position + 1,
            batch.daily_limit
        ));
        let mut paper_id = None;
        let mut review_id = None;
        let mut job_id = None;
        let mut pr_url = None;
        let mut state_label = "reviewed".to_string();

        let result = async {
            let new_review_id =
                super::supervisor::run_one_paper_blocking(&supervisor, &state, &item.arxiv_id)
                    .await?;
            review_id = Some(new_review_id);
            paper_id = paper_id_for_review(pool, new_review_id).await.ok();
            if let Some(id) = paper_id {
                job_id = crate::batch::latest_review_job_for_paper(pool, id).await?;
            }
            if batch.auto_pr {
                let pr = open_review_pr_for_gate(&state, new_review_id, json, false).await?;
                pr_url = pr.pr_url;
                state_label = "pr_open".to_string();
            }
            crate::batch::mark_item_succeeded(
                pool,
                item.id,
                paper_id,
                new_review_id,
                job_id,
                pr_url.as_deref(),
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => outputs.push(BatchRunItemOutput {
                item_id: item.id,
                arxiv_id: item.arxiv_id,
                state: state_label,
                paper_id,
                review_id,
                pr_url,
                error: None,
            }),
            Err(e) => {
                let error = e.to_string();
                let _ = crate::batch::mark_item_failed(
                    pool, item.id, paper_id, review_id, job_id, &error,
                )
                .await;
                failures.push(format!("{}: {error}", item.arxiv_id));
                outputs.push(BatchRunItemOutput {
                    item_id: item.id,
                    arxiv_id: item.arxiv_id,
                    state: "failed".to_string(),
                    paper_id,
                    review_id,
                    pr_url,
                    error: Some(error),
                });
            }
        }
    }

    let output = BatchRunOutput {
        batch_id,
        today,
        dry_run: false,
        items: outputs,
    };
    print_batch_run_output(&output, json)?;
    supervisor.shutdown();
    if !failures.is_empty() {
        anyhow::bail!(
            "batch run failed for {} item(s): {}",
            failures.len(),
            failures.join(" | ")
        );
    }
    Ok(())
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn batch_run(
    batch_id: Uuid,
    today: Option<chrono::NaiveDate>,
    limit: Option<u32>,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let _ = (batch_id, today, limit, dry_run, json);
    anyhow::bail!("batch run requires --features full (grokrxiv-ingest)")
}

fn print_batch_run_output(output: &BatchRunOutput, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else if output.items.is_empty() {
        println!("batch_id={} due=0", output.batch_id);
    } else {
        println!(
            "batch_id={} today={} dry_run={} items={}",
            output.batch_id,
            output.today,
            output.dry_run,
            output.items.len()
        );
        for item in &output.items {
            let review = item
                .review_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string());
            let pr = item.pr_url.as_deref().unwrap_or("-");
            println!(
                "  {} {:12} review_id={} pr_url={}",
                item.arxiv_id, item.state, review, pr
            );
            if let Some(error) = item.error.as_deref() {
                eprintln!("    error: {error}");
            }
        }
    }
    Ok(())
}

async fn batch_status(batch_id: Uuid, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("batch status: DATABASE_URL not configured"))?;
    let status = crate::batch::load_batch_status(pool, batch_id).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        print_batch_status(&status);
    }
    Ok(())
}

async fn batch_list(limit: u32, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("batch list: DATABASE_URL not configured"))?;
    let rows = crate::batch::list_batches(pool, limit as i64).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("(no batches)");
    } else {
        println!(
            "{:36}  {:8}  {:10}  {:10}  {:8}  counts",
            "id", "state", "from", "until", "auto_pr"
        );
        for status in rows {
            println!(
                "{}  {:8}  {}  {}  {:8}  {}",
                status.batch.id,
                status.batch.state,
                status.batch.from,
                status.batch.until,
                status.batch.auto_pr,
                format_batch_counts(&status.counts)
            );
        }
    }
    Ok(())
}

fn print_batch_status(status: &crate::batch::BatchStatus) {
    println!(
        "batch_id={} state={} category={} from={} until={} daily_limit={} auto_pr={} counts={}",
        status.batch.id,
        status.batch.state,
        status.batch.category,
        status.batch.from,
        status.batch.until,
        status.batch.daily_limit,
        status.batch.auto_pr,
        format_batch_counts(&status.counts)
    );
    if status.next_items.is_empty() {
        println!("next_items=0");
    } else {
        println!("next_items:");
        for item in &status.next_items {
            println!(
                "  {:>4} {} {:10} {} {}",
                item.position + 1,
                item.scheduled_for,
                item.state,
                item.arxiv_id,
                truncate(&item.title, 72)
            );
        }
    }
}

fn format_batch_counts(counts: &std::collections::BTreeMap<String, i64>) -> String {
    if counts.is_empty() {
        return "empty".to_string();
    }
    counts
        .iter()
        .map(|(state, count)| format!("{state}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

async fn jobs_command(command: JobsCommand, json: bool) -> anyhow::Result<()> {
    match command {
        JobsCommand::List { kind, state, limit } => {
            let config = super::Config::from_env();
            let state_obj = super::AppState::from_config(config).await?;
            let pool = state_obj
                .db
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("jobs list: DATABASE_URL not configured"))?;
            let kind = normalize_job_filter(kind);
            let state = normalize_job_filter(state);
            let rows =
                crate::batch::list_jobs(pool, kind.as_deref(), state.as_deref(), limit as i64)
                    .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("(no jobs)");
            } else {
                println!(
                    "{:36}  {:8}  {:8}  {:7}  ref_id/error",
                    "id", "kind", "state", "attempt"
                );
                for row in rows {
                    let detail = row
                        .error
                        .as_deref()
                        .map(|s| truncate(s, 80))
                        .or_else(|| row.ref_id.map(|id| id.to_string()))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "{}  {:8}  {:8}  {:7}  {}",
                        row.id, row.kind, row.state, row.attempt, detail
                    );
                }
            }
            Ok(())
        }
    }
}

fn normalize_job_filter(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
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
    let citation_summary = citation_verifier_summary(pool, review_id).await;
    let gate = load_publication_gate_context(pool, review_id).await.ok();
    if json {
        let mut value = serde_json::to_value(&row)?;
        if let Some(summary) = citation_summary.as_ref() {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "citation_verifier".to_string(),
                    serde_json::to_value(summary)?,
                );
            }
        }
        if let Some((_, publication_gate, specialist_gate)) = gate.as_ref() {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "publication_gate".to_string(),
                    serde_json::json!({
                        "verdict": publication_gate.verdict,
                        "recommendation": publication_gate.recommendation.clone(),
                        "reason": publication_gate.reason.clone(),
                        "usable_roles": specialist_gate.usable_roles.clone(),
                        "warning_roles": specialist_gate.warning_roles.clone(),
                        "blocked_roles": specialist_gate.blocked_roles.clone(),
                    }),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
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
        if let Some((_, publication_gate, specialist_gate)) = gate.as_ref() {
            println!("gate        = {:?}", publication_gate.verdict);
            println!("gate_reason = {}", truncate(&publication_gate.reason, 220));
            if !specialist_gate.blocked_roles.is_empty() {
                println!("blocked     = {}", specialist_gate.blocked_roles.join(", "));
            }
            if !specialist_gate.warning_roles.is_empty() {
                println!("warnings    = {}", specialist_gate.warning_roles.join(", "));
            }
        }
        if let Some(summary) = citation_summary.as_ref() {
            if summary.checked == 0 {
                let coverage = summary.coverage_status.as_deref().unwrap_or("not_checked");
                println!("citations   = {coverage} (checked=0)");
                if let Some(reason) = summary.reason.as_deref() {
                    println!("citation_reason = {}", truncate(reason, 220));
                }
            } else {
                println!(
                    "citations   = checked={} not_resolved={} needs_review={} unknown={} malformed={} fail_fraction={:.3}",
                    summary.checked,
                    summary.unresolved,
                    summary.unverified,
                    summary.unknown,
                    summary.malformed,
                    summary.unresolved_fraction,
                );
            }
            if !summary.evidence.is_empty() {
                println!("citation checks needing review:");
                for item in &summary.evidence {
                    println!("  - {}", item.to_human_line());
                }
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

async fn review_extracted(source: &str, force: bool, json: bool) -> anyhow::Result<()> {
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
                crate::cli_status::emit(format!(
                    "paper {arxiv_id}: already reviewed; existing status={status}"
                ));
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&existing_review_json(
                            paper_id,
                            &arxiv_id,
                            review_id,
                            &status,
                            pr_url.as_deref(),
                        ))?
                    );
                } else {
                    print!(
                        "{}",
                        existing_review_text(
                            paper_id,
                            &arxiv_id,
                            review_id,
                            &status,
                            pr_url.as_deref(),
                        )
                    );
                }
                return Ok(());
            }
        }
        crate::cli_status::emit(format!(
            "paper {arxiv_id}: reviewing cached extraction for `{}`",
            truncate(&title, 80)
        ));
        let review_id = super::supervisor::run_review_for_paper_blocking(&state, paper_id).await?;
        let pr = open_review_pr_for_gate(&state, review_id, json, false).await?;
        let pr_url = review_pr_dispatch_pr_url(&pr)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "arxiv_id": arxiv_id,
                    "paper_id": paper_id,
                    "review_id": review_id,
                    "pr_url": pr_url,
                    "gate_verdict": pr.gate_verdict,
                    "recommendation": pr.recommendation,
                    "pr_kind": pr.kind.as_str(),
                }))?
            );
        } else {
            println!(
                "arxiv_id={arxiv_id} paper_id={paper_id} review_id={review_id} pr_url={}",
                pr_url
            );
        }
        Ok(())
    }
    #[cfg(not(all(feature = "grokrxiv-ingest", feature = "grokrxiv-storage")))]
    {
        let _ = (source, force, json);
        anyhow::bail!(
            "review-extracted requires --features full (grokrxiv-ingest + grokrxiv-storage)"
        )
    }
}

fn existing_review_json(
    paper_id: Uuid,
    arxiv_id: &str,
    review_id: Uuid,
    review_status: &str,
    pr_url: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "status": "already_reviewed",
        "arxiv_id": arxiv_id,
        "paper_id": paper_id,
        "review_id": review_id,
        "review_status": review_status,
        "pr_url": pr_url,
        "show_command": format!("agh app run grokrxiv show {review_id}"),
        "force_command": format!("agh app run grokrxiv review-extracted --force {arxiv_id}"),
    })
}

fn existing_review_text(
    paper_id: Uuid,
    arxiv_id: &str,
    review_id: Uuid,
    review_status: &str,
    pr_url: Option<&str>,
) -> String {
    let mut out = format!(
        "already_reviewed=true\narxiv_id={arxiv_id}\npaper_id={paper_id}\nreview_id={review_id}\nreview_status={review_status}\n"
    );
    if let Some(pr_url) = pr_url {
        out.push_str(&format!("pr_url={pr_url}\n"));
    }
    out.push_str(&format!(
        "show_command=agh app run grokrxiv show {review_id}\n"
    ));
    out.push_str(&format!(
        "force_command=agh app run grokrxiv review-extracted --force {arxiv_id}\n"
    ));
    out
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
        let lookup_arxiv_id = strip_arxiv_version(&arxiv_id).to_string();
        sqlx::query_as(
            "select p.id, p.arxiv_id, p.title, pa.extraction_status, pa.git_path \
                 from papers p left join paper_assets pa on pa.paper_id = p.id \
                 where p.arxiv_id = $1",
        )
        .bind(lookup_arxiv_id)
        .fetch_optional(pool)
        .await?
    } else {
        anyhow::bail!("review-extracted: `{source}` is not a paper UUID, arXiv id, or arXiv URL");
    };

    let Some((paper_id, arxiv_id, title, status, git_path)) = row else {
        anyhow::bail!(
            "review-extracted: no paper row for `{source}`; run `agh app run grokrxiv extract {source}` first"
        );
    };
    if status.as_deref() != Some("ready") || git_path.is_none() {
        anyhow::bail!(
            "review-extracted: paper {arxiv_id} is not extracted yet (status={}); run `agh app run grokrxiv extract {arxiv_id}` first",
            status.as_deref().unwrap_or("pending")
        );
    }
    Ok((paper_id, arxiv_id, title))
}

/// Source resolution for `agh app run grokrxiv review <source>`.
#[derive(Debug, Clone)]
enum ResolvedSource {
    /// arXiv id (already normalised).
    Arxiv(String),
    /// Local file path. Kind is best-guess from the extension.
    LocalFile(std::path::PathBuf, SourceType, bool),
    /// Git repository source. Corpus expansion can attach an explicit
    /// manuscript path and group id per resolved paper.
    GitRepo {
        repo: String,
        paper_path: Option<PathBuf>,
        corpus_id: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
struct ReviewSourceOptions {
    rev: Option<String>,
    paper_path: Option<PathBuf>,
    title: Option<String>,
    field: Option<String>,
    corpus: bool,
    scan_root: Option<PathBuf>,
    limit: Option<usize>,
    include: Vec<String>,
    exclude: Vec<String>,
    loop_enabled: bool,
    debug_output: bool,
    no_external_actions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReviewLoopStage {
    id: String,
    kind: String,
    dag_type: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReviewLoopCorpusContext {
    id: String,
    tier: String,
    source: String,
    expected_recommendation: Option<String>,
}

fn review_loop_stage_plan() -> anyhow::Result<Vec<ReviewLoopStage>> {
    let manifest_path = agent_config::dag_manifest_path("review-loop");
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|e| anyhow::anyhow!("validate {}: {e}", manifest_path.display()))?;
    Ok(manifest
        .nodes
        .iter()
        .map(|node| ReviewLoopStage {
            id: node.id.clone(),
            kind: node.kind.to_string(),
            dag_type: node.dag_type.clone(),
            inputs: node.inputs.clone(),
            outputs: node.outputs.clone(),
            required: node.required,
        })
        .collect())
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
    let version_suffix = parts.next();
    let (a, b) = base.split_once('.')?;
    if a.len() < 4 || a.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    if b.len() < 4 || b.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    if let Some(suffix) = version_suffix {
        if suffix.is_empty() || suffix.chars().any(|c| !c.is_ascii_digit()) {
            return None;
        }
        Some(format!("{base}v{suffix}"))
    } else {
        Some(base.to_string())
    }
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

fn looks_like_git_source(source: &str) -> bool {
    let trimmed = source.trim();
    if trimmed.starts_with("git@") || trimmed.ends_with(".git") {
        return true;
    }
    if trimmed.starts_with("https://github.com/")
        || trimmed.starts_with("http://github.com/")
        || trimmed.starts_with("https://gitlab.com/")
        || trimmed.starts_with("http://gitlab.com/")
    {
        return true;
    }
    let path = std::path::Path::new(trimmed);
    path.is_dir() && path.join(".git").is_dir()
}

fn app_relative_local_source_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let candidate = crate::dag_apps::app_root("grokrxiv").join(path);
    candidate.is_file().then_some(candidate)
}

#[cfg(feature = "grokrxiv-ingest")]
fn local_source_format(kind: SourceType) -> Option<grokrxiv_ingest::LocalSourceFormat> {
    match kind {
        SourceType::Pdf => Some(grokrxiv_ingest::LocalSourceFormat::Pdf),
        SourceType::Tex => Some(grokrxiv_ingest::LocalSourceFormat::Tex),
        SourceType::Arxiv | SourceType::Git | SourceType::Mixed => None,
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
            SourceType::Arxiv | SourceType::Git | SourceType::Mixed => ".bin",
        };
        let mut path = std::env::temp_dir();
        path.push(format!(
            "grokrxiv-stdin-{}{ext}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::write(&path, &buf).await?;
        return Ok(vec![ResolvedSource::LocalFile(path, kind, true)]);
    }
    if matches!(type_hint, Some(SourceType::Git)) {
        return Ok(vec![ResolvedSource::GitRepo {
            repo: source.to_string(),
            paper_path: None,
            corpus_id: None,
        }]);
    }
    if let Some(id) = parse_arxiv_source(source) {
        return Ok(vec![ResolvedSource::Arxiv(id)]);
    }
    let path = std::path::PathBuf::from(source);
    if path.is_file() {
        let kind = type_hint.unwrap_or_else(|| guess_local_kind(&path));
        return Ok(vec![ResolvedSource::LocalFile(path, kind, false)]);
    }
    if let Some(app_relative_path) = app_relative_local_source_path(&path) {
        let kind = type_hint.unwrap_or_else(|| guess_local_kind(&app_relative_path));
        return Ok(vec![ResolvedSource::LocalFile(
            app_relative_path,
            kind,
            false,
        )]);
    }
    if looks_like_git_source(source) {
        return Ok(vec![ResolvedSource::GitRepo {
            repo: source.to_string(),
            paper_path: None,
            corpus_id: None,
        }]);
    }
    anyhow::bail!("could not resolve source `{source}` (not an arXiv id/URL, local .tex/.pdf file, or git repository)")
}

/// Canonical end-to-end entry point — `agh app run grokrxiv review <source>`.
async fn review_source(
    source: &str,
    type_hint: Option<SourceType>,
    options: ReviewSourceOptions,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    if options.debug_output {
        cli_status::set_enabled(true);
    }
    let resolved = resolve_source(source, type_hint).await?;
    let resolved = expand_corpus_sources(resolved, &options).await?;
    if dry_run {
        let loop_stages = if options.loop_enabled {
            Some(review_loop_stage_plan()?)
        } else {
            None
        };
        if resolved.len() == 1 {
            emit_pipeline_header("review", resolved_source_label(&resolved[0]).as_str());
            cli_status::emit_stage(
                1,
                1,
                "Plan",
                cli_status::StatusMark::Ok,
                "dry run; no pipeline work started",
            );
        }
        let plan: Vec<serde_json::Value> = resolved
            .iter()
            .map(|s| match s {
                ResolvedSource::Arxiv(id) => serde_json::json!({"kind": "arxiv", "id": id}),
                ResolvedSource::LocalFile(p, k, cleanup_after_use) => serde_json::json!({
                    "kind": "local",
                    "path": p.display().to_string(),
                    "type": format!("{k:?}"),
                    "cleanup_after_use": cleanup_after_use,
                }),
                ResolvedSource::GitRepo {
                    repo,
                    paper_path,
                    corpus_id,
                } => serde_json::json!({
                    "kind": "git_repo",
                    "repo": repo,
                    "rev": options.rev.as_deref(),
                    "paper_path": paper_path
                        .as_ref()
                        .or(options.paper_path.as_ref())
                        .map(|p| p.display().to_string()),
                    "corpus_id": corpus_id,
                }),
            })
            .collect();
        if json {
            let mut output = serde_json::json!({
                "plan": plan,
                "loop": {
                    "enabled": options.loop_enabled,
                    "dag_type": if options.loop_enabled { Some("review-loop") } else { None::<&str> },
                },
                "debug": {
                    "enabled": options.debug_output,
                },
                "external_actions": {
                    "enabled": !options.no_external_actions,
                }
            });
            if let Some(stages) = loop_stages.as_ref() {
                output["loop"]["stages"] = serde_json::to_value(stages)?;
            }
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("dry-run plan:");
            for p in plan {
                println!("  {}", p);
            }
            if let Some(stages) = loop_stages {
                println!("review-loop stages:");
                for stage in stages {
                    let dag_call = stage
                        .dag_type
                        .as_deref()
                        .map(|dag| format!(" dag_call={dag}"))
                        .unwrap_or_default();
                    println!("  {} ({}){}", stage.id, stage.kind, dag_call);
                }
            }
            if options.no_external_actions {
                println!("external actions: disabled");
            }
        }
        return Ok(());
    }

    if resolved.is_empty() {
        anyhow::bail!("no reviewable sources resolved from `{source}`");
    }

    #[cfg(feature = "grokrxiv-ingest")]
    {
        review_resolved_sources(&resolved, &options, json).await
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        let _ = (resolved, options, json);
        anyhow::bail!("review requires --features full (grokrxiv-ingest)")
    }
}

async fn expand_corpus_sources(
    resolved: Vec<ResolvedSource>,
    options: &ReviewSourceOptions,
) -> anyhow::Result<Vec<ResolvedSource>> {
    if !options.corpus {
        return Ok(resolved);
    }
    if options.paper_path.is_some() {
        anyhow::bail!("--corpus cannot be combined with --paper-path; use --scan-root/--include/--exclude instead");
    }
    let mut expanded = Vec::new();
    for source in resolved {
        match source {
            ResolvedSource::GitRepo { repo, .. } => {
                let scan_options = grokrxiv_ingest::CorpusScanOptions {
                    scan_root: options.scan_root.clone(),
                    include: options.include.clone(),
                    exclude: options.exclude.clone(),
                    limit: options.limit,
                };
                let candidates = grokrxiv_ingest::scan_git_repo_corpus(
                    &repo,
                    options.rev.as_deref(),
                    &scan_options,
                )
                .await?;
                let corpus_id =
                    corpus_id_for(&repo, options.rev.as_deref(), options.scan_root.as_ref());
                crate::cli_status::emit(format!(
                    "corpus {corpus_id}: discovered {} manuscript(s)",
                    candidates.len()
                ));
                for candidate in candidates {
                    expanded.push(ResolvedSource::GitRepo {
                        repo: repo.clone(),
                        paper_path: Some(candidate.path),
                        corpus_id: Some(corpus_id.clone()),
                    });
                }
            }
            _ => anyhow::bail!("--corpus only supports git repository sources"),
        }
    }
    Ok(expanded)
}

fn corpus_id_for(repo: &str, rev: Option<&str>, scan_root: Option<&PathBuf>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(repo.as_bytes());
    hasher.update(b"\0");
    hasher.update(rev.unwrap_or("HEAD").as_bytes());
    hasher.update(b"\0");
    if let Some(scan_root) = scan_root {
        hasher.update(scan_root.display().to_string().as_bytes());
    }
    let hash = hex::encode(hasher.finalize());
    format!("git-corpus-{}", &hash[..12])
}

#[cfg(feature = "grokrxiv-ingest")]
async fn review_resolved_sources(
    resolved: &[ResolvedSource],
    options: &ReviewSourceOptions,
    json: bool,
) -> anyhow::Result<()> {
    let _cleanup = LocalSourceCleanup::new(resolved);
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());
    let _supervisor_shutdown = SupervisorShutdownOnDrop(supervisor.clone());
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("review: DATABASE_URL not configured"))?;

    let mut results = Vec::with_capacity(resolved.len());
    for source in resolved {
        match source {
            ResolvedSource::Arxiv(id) => {
                emit_pipeline_header("review", id);
                let review_id =
                    super::supervisor::run_one_paper_blocking(&supervisor, &state, id).await?;
                crate::cli_status::emit(format!(
                    "paper {id}: review_id={review_id} running publication policy"
                ));
                let (pr, loop_outcome) = open_review_pr_after_optional_loop(
                    &state,
                    review_id,
                    options.loop_enabled,
                    options.debug_output,
                    !options.no_external_actions,
                    json,
                )
                .await?;
                let paper_id = paper_id_for_review(pool, review_id).await.ok();
                let mut envelope =
                    review_result_envelope(pool, review_id, "arxiv", id, paper_id).await?;
                if let Some(loop_outcome) = loop_outcome.as_ref() {
                    envelope = review_result_envelope_with_loop(envelope, loop_outcome);
                }
                let envelope = review_result_envelope_with_pr(envelope, &pr);
                if !json {
                    println!(
                        "source_kind=arxiv source_id={id} paper_id={} review_id={review_id} {}",
                        envelope
                            .get("paper_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unknown>"),
                        review_pr_dispatch_cli_summary(&pr)
                    );
                }
                results.push(envelope);
            }
            ResolvedSource::LocalFile(path, kind, _) => {
                let spec = grokrxiv_ingest::ReviewSourceSpec::LocalFile {
                    path: path.clone(),
                    format: local_source_format(*kind),
                    title: options.title.clone(),
                    authors: Vec::new(),
                    field: options.field.clone(),
                };
                let (paper_id, review_id, source_kind, source_id) =
                    review_prepared_source(&state, spec).await?;
                let (pr, loop_outcome) = open_review_pr_after_optional_loop(
                    &state,
                    review_id,
                    options.loop_enabled,
                    options.debug_output,
                    !options.no_external_actions,
                    json,
                )
                .await?;
                if !json {
                    println!(
                        "source_kind={source_kind} source_id={source_id} paper_id={paper_id} review_id={review_id} {}",
                        review_pr_dispatch_cli_summary(&pr)
                    );
                }
                let mut envelope = review_result_envelope(
                    pool,
                    review_id,
                    &source_kind,
                    &source_id,
                    Some(paper_id),
                )
                .await?;
                if let Some(loop_outcome) = loop_outcome.as_ref() {
                    envelope = review_result_envelope_with_loop(envelope, loop_outcome);
                }
                let envelope = review_result_envelope_with_pr(envelope, &pr);
                results.push(envelope);
            }
            ResolvedSource::GitRepo {
                repo,
                paper_path,
                corpus_id,
            } => {
                let spec = grokrxiv_ingest::ReviewSourceSpec::GitRepo {
                    repo: repo.clone(),
                    rev: options.rev.clone(),
                    paper_path: paper_path.clone().or_else(|| options.paper_path.clone()),
                    title: options.title.clone(),
                    authors: Vec::new(),
                    field: options.field.clone(),
                    corpus_id: corpus_id.clone(),
                };
                let (paper_id, review_id, source_kind, source_id) =
                    review_prepared_source(&state, spec).await?;
                let (pr, loop_outcome) = open_review_pr_after_optional_loop(
                    &state,
                    review_id,
                    options.loop_enabled,
                    options.debug_output,
                    !options.no_external_actions,
                    json,
                )
                .await?;
                if !json {
                    println!(
                        "source_kind={source_kind} source_id={source_id} paper_id={paper_id} review_id={review_id} {}",
                        review_pr_dispatch_cli_summary(&pr)
                    );
                }
                let mut envelope = review_result_envelope(
                    pool,
                    review_id,
                    &source_kind,
                    &source_id,
                    Some(paper_id),
                )
                .await?;
                if let Some(loop_outcome) = loop_outcome.as_ref() {
                    envelope = review_result_envelope_with_loop(envelope, loop_outcome);
                }
                let envelope = review_result_envelope_with_pr(envelope, &pr);
                results.push(envelope);
            }
        }
    }

    if json {
        if results.len() == 1 {
            println!("{}", serde_json::to_string_pretty(&results[0])?);
        } else {
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
    }
    Ok(())
}

#[cfg(feature = "grokrxiv-ingest")]
struct LocalSourceCleanup {
    paths: Vec<PathBuf>,
}

#[cfg(feature = "grokrxiv-ingest")]
impl LocalSourceCleanup {
    fn new(resolved: &[ResolvedSource]) -> Self {
        let paths = resolved
            .iter()
            .filter_map(|source| match source {
                ResolvedSource::LocalFile(path, _, true) => Some(path.clone()),
                _ => None,
            })
            .collect();
        Self { paths }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
impl Drop for LocalSourceCleanup {
    fn drop(&mut self) {
        for path in self.paths.drain(..) {
            if let Err(err) = std::fs::remove_file(&path) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        err = %err,
                        "failed to remove stdin review temp file"
                    );
                }
            }
        }
    }
}

#[cfg(feature = "grokrxiv-ingest")]
struct SupervisorShutdownOnDrop(super::supervisor::Supervisor);

#[cfg(feature = "grokrxiv-ingest")]
impl Drop for SupervisorShutdownOnDrop {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

fn resolved_source_label(source: &ResolvedSource) -> String {
    match source {
        ResolvedSource::Arxiv(id) => id.clone(),
        ResolvedSource::LocalFile(path, _, _) => path.display().to_string(),
        ResolvedSource::GitRepo {
            repo, paper_path, ..
        } => paper_path
            .as_ref()
            .map(|path| format!("{repo}:{}", path.display()))
            .unwrap_or_else(|| repo.clone()),
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn review_prepared_source(
    state: &super::AppState,
    spec: grokrxiv_ingest::ReviewSourceSpec,
) -> anyhow::Result<(Uuid, Uuid, String, String)> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("review: DATABASE_URL not configured"))?;
    let prepared = grokrxiv_ingest::prepare_review_source(spec).await?;
    let source_kind = source_kind_db(prepared.identity.source_kind).to_string();
    let source_id = prepared.identity.source_id.clone();
    let display_label = prepared.identity.display_label.clone();
    let canonical_uri = prepared.identity.canonical_uri.clone();
    let arxiv_id = prepared.identity.arxiv_id.clone();
    let content_hash = prepared.identity.content_hash.clone();
    let source_metadata = serde_json::json!({
        "display_label": display_label,
        "canonical_uri": canonical_uri,
        "arxiv_id": arxiv_id,
        "adapter": prepared.source_metadata,
    });
    let source = crate::db::PaperSourceMetadata {
        source_kind: source_kind.clone(),
        source_id: source_id.clone(),
        source_uri: Some(canonical_uri),
        source_hash: Some(content_hash),
        source_metadata,
    };
    let paper_id =
        crate::db::upsert_paper_with_source(pool, &prepared.extract, None, &source).await?;
    crate::cli_status::emit(format!(
        "paper {source_id}: prepared {source_kind}; persisted paper_id={paper_id}; starting review DAG"
    ));
    let review_id =
        super::supervisor::run_review_for_extract_blocking(state, paper_id, prepared.extract)
            .await?;
    Ok((paper_id, review_id, source_kind, source_id))
}

#[cfg(feature = "grokrxiv-ingest")]
fn source_kind_db(kind: grokrxiv_ingest::SourceKind) -> &'static str {
    match kind {
        grokrxiv_ingest::SourceKind::Arxiv => "arxiv",
        grokrxiv_ingest::SourceKind::LocalFile => "local_file",
        grokrxiv_ingest::SourceKind::GitRepo => "git_repo",
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn paper_id_for_review(pool: &sqlx::PgPool, review_id: Uuid) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("select paper_id from reviews where id = $1")
        .bind(review_id)
        .fetch_one(pool)
        .await
}

async fn load_review_loop_paper_math_sources(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
) -> anyhow::Result<serde_json::Value> {
    let mut artifact_sources = Vec::new();
    let mut warnings = Vec::new();
    let mut body = serde_json::json!({
        "artifact": "review_inputs.artifact",
        "sections": [],
    });
    let mut equations = serde_json::json!({
        "artifact": "equations.json",
        "equations": [],
        "reason": "not_loaded",
    });
    let mut theorem_graph = serde_json::json!({
        "artifact": "theorem_graph.json",
        "nodes": [],
        "reason": "not_loaded",
    });
    let mut semantic_ast = serde_json::Value::Null;

    if let Some(artifact) = crate::db::load_latest_review_input_artifact(pool, paper_id).await? {
        artifact_sources.push("review_inputs.artifact".to_string());
        body = serde_json::json!({
            "artifact": "review_inputs.artifact",
            "sections": artifact
                .get("sections")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        });
    }

    #[cfg(feature = "grokrxiv-storage")]
    {
        let mut loaded_tier1_sources = false;
        let mut repo_root_for_fallback = None;

        match data_repo_root() {
            Ok(repo_root) => {
                repo_root_for_fallback = Some(repo_root.clone());
                if let Some(assets) = crate::db::read_paper_assets(pool, paper_id).await? {
                    if matches!(assets.extraction_status, crate::db::ExtractionStatus::Ready) {
                        if let Some(git_path) = assets.git_path.as_deref() {
                            let review_input_path =
                                repo_root.join(git_path).join("review_input.json");
                            match load_review_loop_paper_math_source_files(
                                &repo_root,
                                &review_input_path,
                            ) {
                                Ok(files) => {
                                    artifact_sources.extend(files.artifact_sources);
                                    body = files.body;
                                    equations = files.equations;
                                    theorem_graph = files.theorem_graph;
                                    semantic_ast = files.semantic_ast;
                                    loaded_tier1_sources = true;
                                }
                                Err(err) => warnings.push(format!(
                                    "review_input.json not loaded at {}: {err:#}",
                                    review_input_path.display()
                                )),
                            }
                        } else {
                            warnings.push("paper_assets ready but git_path is missing".to_string());
                        }
                    }
                }
            }
            Err(err) => warnings.push(format!("GROKRXIV_DATA_REPO_PATH unavailable: {err:#}")),
        }

        if !loaded_tier1_sources {
            if let Some(repo_root) = repo_root_for_fallback.as_deref() {
                match crate::db::load_paper_review_seed(pool, paper_id).await {
                    Ok(seed) => match load_review_loop_paper_math_sources_from_data_repo_cache(
                        repo_root,
                        &seed.arxiv_id,
                    ) {
                        Ok(Some(files)) => {
                            artifact_sources.extend(files.artifact_sources);
                            body = files.body;
                            equations = files.equations;
                            theorem_graph = files.theorem_graph;
                            semantic_ast = files.semantic_ast;
                        }
                        Ok(None) => warnings.push(format!(
                            "data repo cache has no review_input.json for {}",
                            seed.arxiv_id
                        )),
                        Err(err) => warnings.push(format!(
                            "data repo cache review_input.json not loaded: {err:#}"
                        )),
                    },
                    Err(err) => warnings.push(format!(
                        "paper seed not loaded for data repo cache fallback: {err:#}"
                    )),
                }
            }
        }
    }

    Ok(serde_json::json!({
        "schema_version": "1.0.0",
        "paper_id": paper_id,
        "source": "paper_extract_artifacts",
        "artifact_sources": artifact_sources,
        "warnings": warnings,
        "body": body,
        "equations": equations,
        "theorem_graph": theorem_graph,
        "semantic_ast": semantic_ast,
    }))
}

#[cfg(feature = "grokrxiv-ingest")]
async fn review_result_envelope(
    pool: &sqlx::PgPool,
    review_id: Uuid,
    source_kind: &str,
    source_id: &str,
    paper_id: Option<Uuid>,
) -> anyhow::Result<serde_json::Value> {
    let status: String = sqlx::query_scalar("select status from reviews where id = $1")
        .bind(review_id)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|_| "awaiting_moderation".to_string());
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
    Ok(serde_json::json!({
        "source_kind": source_kind,
        "source_id": source_id,
        "paper_id": paper_id.map(|id| id.to_string()),
        "review_id": review_id,
        "status": status,
        "agents": agents_json,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewPrDispatchKind {
    Publication,
    RevisionNeeded,
}

impl ReviewPrDispatchKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Publication => "publication",
            Self::RevisionNeeded => "revision_needed",
        }
    }
}

#[derive(Debug, Clone)]
struct ReviewPrDispatchOutcome {
    pr_url: Option<String>,
    gate_verdict: crate::review_gate::GateVerdict,
    recommendation: String,
    kind: ReviewPrDispatchKind,
    external_actions_enabled: bool,
}

fn review_pr_dispatch_kind(gate: &crate::review_gate::PublicationGate) -> ReviewPrDispatchKind {
    if gate.verdict == crate::review_gate::GateVerdict::Pass {
        ReviewPrDispatchKind::Publication
    } else {
        ReviewPrDispatchKind::RevisionNeeded
    }
}

fn review_result_envelope_with_pr(
    mut envelope: serde_json::Value,
    pr: &ReviewPrDispatchOutcome,
) -> serde_json::Value {
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("pr_url".to_string(), serde_json::json!(pr.pr_url.clone()));
        obj.insert(
            "gate_verdict".to_string(),
            serde_json::json!(pr.gate_verdict),
        );
        obj.insert(
            "recommendation".to_string(),
            serde_json::json!(pr.recommendation.clone()),
        );
        obj.insert("pr_kind".to_string(), serde_json::json!(pr.kind.as_str()));
        obj.insert(
            "external_actions_enabled".to_string(),
            serde_json::json!(pr.external_actions_enabled),
        );
    }
    envelope
}

fn review_pr_dispatch_skipped_by_policy(
    gate: &crate::review_gate::PublicationGate,
) -> ReviewPrDispatchOutcome {
    ReviewPrDispatchOutcome {
        pr_url: None,
        gate_verdict: gate.verdict,
        recommendation: gate.recommendation.clone(),
        kind: review_pr_dispatch_kind(gate),
        external_actions_enabled: false,
    }
}

fn review_pr_dispatch_skipped_for_loop_halt() -> ReviewPrDispatchOutcome {
    ReviewPrDispatchOutcome {
        pr_url: None,
        gate_verdict: crate::review_gate::GateVerdict::Fail,
        recommendation: "human_escalation_required".to_string(),
        kind: ReviewPrDispatchKind::RevisionNeeded,
        external_actions_enabled: false,
    }
}

fn review_loop_external_actions_allowed(
    external_actions_enabled: bool,
    loop_outcome: Option<&ReviewLoopOutcome>,
) -> bool {
    external_actions_enabled && !loop_outcome.is_some_and(review_loop_outcome_halted)
}

fn review_loop_outcome_halted(outcome: &ReviewLoopOutcome) -> bool {
    outcome.halted
        || outcome.deterministic_status == "halted"
        || outcome
            .report
            .get("halted")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        || outcome
            .report
            .get("halted_by_never_event")
            .and_then(|value| value.as_str())
            .is_some()
}

fn review_pr_dispatch_cli_summary(pr: &ReviewPrDispatchOutcome) -> String {
    match pr.pr_url.as_deref() {
        Some(pr_url) => format!("pr_url={pr_url}"),
        None => format!("external_actions=disabled pr_kind={}", pr.kind.as_str()),
    }
}

fn review_pr_dispatch_pr_url(pr: &ReviewPrDispatchOutcome) -> anyhow::Result<&str> {
    pr.pr_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "review PR dispatch did not produce a PR URL because external actions were disabled"
        )
    })
}

fn review_result_envelope_with_loop(
    mut envelope: serde_json::Value,
    loop_outcome: &ReviewLoopOutcome,
) -> serde_json::Value {
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert(
            "review_loop".to_string(),
            serde_json::json!({
                "dag_type": "review-loop",
                "status": loop_outcome.deterministic_status,
                "publisher_ready": loop_outcome.publisher_ready,
                "halted": loop_outcome.halted,
                "blocking_issues": loop_outcome.blocking_issues,
                "artifact_dir": loop_outcome.artifact_dir,
                "report_path": loop_outcome.report_path,
            }),
        );
    }
    envelope
}

#[derive(Debug, Clone, Serialize)]
struct ReviewLoopOutcome {
    publisher_ready: bool,
    deterministic_status: String,
    halted: bool,
    blocking_issues: Vec<String>,
    artifact_dir: String,
    report_path: String,
    report: serde_json::Value,
}

fn review_loop_corpus_contexts_from_yaml(
    corpus_yaml: &str,
) -> anyhow::Result<Vec<ReviewLoopCorpusContext>> {
    let corpus: serde_yaml::Value =
        serde_yaml::from_str(corpus_yaml).context("parse GrokRxiv golden corpus")?;
    let entries = corpus
        .get("entries")
        .and_then(|value| value.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("corpus.yaml missing entries[]"))?;
    let mut contexts = Vec::new();
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::anyhow!("corpus entry missing id"))?;
        let tier = entry
            .get("tier")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::anyhow!("corpus entry `{id}` missing tier"))?;
        let source = entry
            .get("source")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow::anyhow!("corpus entry `{id}` missing source"))?;
        let expected_recommendation = entry
            .get("expected")
            .and_then(|value| value.get("recommendation"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        contexts.push(ReviewLoopCorpusContext {
            id: id.to_string(),
            tier: tier.to_string(),
            source: source.to_string(),
            expected_recommendation,
        });
    }
    Ok(contexts)
}

fn review_loop_n5_false_proof_halt(
    corpus_context: &ReviewLoopCorpusContext,
    theorem_map: &serde_json::Value,
) -> Option<serde_json::Value> {
    if !matches!(corpus_context.tier.as_str(), "C" | "G") {
        return None;
    }

    let proved_entries = theorem_map
        .get("entries")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry.get("kind").and_then(|value| value.as_str()) == Some("theorem_formalization")
                && entry.get("status").and_then(|value| value.as_str()) == Some("PROVED")
        })
        .cloned()
        .collect::<Vec<_>>();

    let top_status = theorem_map
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("UNKNOWN");
    if top_status != "PROVED" && proved_entries.is_empty() {
        return None;
    }

    Some(serde_json::json!({
        "schema_version": "1.0.0",
        "status": "halted",
        "never_event": "N5_fake_proof",
        "action": "halt_and_escalate",
        "reason": "Lean reported PROVED for a Tier C/G flawed or false claim. Halt the corpus loop and escalate to a human reviewer.",
        "corpus": {
            "id": corpus_context.id,
            "tier": corpus_context.tier,
            "source": corpus_context.source,
        },
        "lean_verdict": "PROVED",
        "theorem_map_status": top_status,
        "proved_entries": proved_entries,
        "evidence": {
            "theorem_map": "review_loop/lean/theorem_map.json",
            "lean_results": "review_loop/lean/results.json",
            "semantic_adequacy": "review_loop/semantic_adequacy.json",
        },
        "operator_instruction": "Stop the loop. Do not continue fixing autonomously. Attach this dossier and the Lean artifacts to the escalation."
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReviewLoopPublicationGatePolicy {
    publisher_ready: bool,
    integrity_ready: bool,
    blocking_issue: Option<String>,
    status: String,
}

fn review_loop_publication_gate_policy(
    corpus_context: Option<&ReviewLoopCorpusContext>,
    publication_gate: &crate::review_gate::PublicationGate,
) -> ReviewLoopPublicationGatePolicy {
    if publication_gate.verdict == crate::review_gate::GateVerdict::Pass {
        return ReviewLoopPublicationGatePolicy {
            publisher_ready: true,
            integrity_ready: true,
            blocking_issue: None,
            status: "publisher_ready".to_string(),
        };
    }

    if corpus_context.and_then(|context| context.expected_recommendation.as_deref())
        == Some("honest")
        && publication_gate.verdict == crate::review_gate::GateVerdict::Fail
        && matches!(
            publication_gate.recommendation.as_str(),
            "minor_revision" | "major_revision" | "reject"
        )
        && publication_gate.reason.contains("not `accept`")
    {
        return ReviewLoopPublicationGatePolicy {
            publisher_ready: false,
            integrity_ready: true,
            blocking_issue: None,
            status: "honest_non_publishing_recommendation".to_string(),
        };
    }

    ReviewLoopPublicationGatePolicy {
        publisher_ready: false,
        integrity_ready: false,
        blocking_issue: Some(publication_gate.reason.clone()),
        status: format!("{:?}", publication_gate.verdict).to_ascii_lowercase(),
    }
}

fn normalized_corpus_source(value: &str) -> String {
    let mut text = value.trim().trim_start_matches("./").replace('\\', "/");
    while text.contains("//") && !text.starts_with("http://") && !text.starts_with("https://") {
        text = text.replace("//", "/");
    }
    let lower = text.to_ascii_lowercase();
    let source = lower
        .strip_prefix("https://arxiv.org/abs/")
        .or_else(|| lower.strip_prefix("http://arxiv.org/abs/"))
        .or_else(|| lower.strip_prefix("arxiv.org/abs/"))
        .map(|id| format!("arxiv:{id}"))
        .unwrap_or(lower);
    if let Some(id) = source.strip_prefix("arxiv:") {
        return format!("arxiv:{}", strip_arxiv_version(id));
    }
    source
}

fn strip_arxiv_version(id: &str) -> &str {
    let Some((base, suffix)) = id.rsplit_once('v') else {
        return id;
    };
    if suffix.chars().all(|ch| ch.is_ascii_digit()) && parse_arxiv_source(base).is_some() {
        base
    } else {
        id
    }
}

fn source_matches_corpus_entry(candidate: &str, corpus_source: &str) -> bool {
    let candidate = normalized_corpus_source(candidate);
    let expected = normalized_corpus_source(corpus_source);
    candidate == expected
        || expected
            .strip_suffix('/')
            .is_some_and(|prefix| candidate.starts_with(&format!("{prefix}/")))
}

fn add_review_loop_source_candidate(candidates: &mut BTreeSet<String>, value: Option<&str>) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    candidates.insert(value.to_string());
    let path = Path::new(value);
    if let Some(path_text) = path.to_str() {
        if let Ok(app_root) = crate::dag_apps::app_root("grokrxiv").canonicalize() {
            if let Ok(stripped) = path.strip_prefix(&app_root) {
                if let Some(rel) = stripped.to_str() {
                    candidates.insert(rel.replace('\\', "/"));
                    if let Some(parent) = stripped.parent().and_then(|parent| parent.to_str()) {
                        candidates.insert(format!("{}/", parent.replace('\\', "/")));
                    }
                }
            }
        }
        if path_text.starts_with("file://") {
            add_review_loop_source_candidate(candidates, Some(&path_text[7..]));
        }
    }
}

fn review_loop_corpus_context_for_candidates(
    contexts: &[ReviewLoopCorpusContext],
    candidates: &BTreeSet<String>,
) -> Option<ReviewLoopCorpusContext> {
    contexts.iter().find_map(|context| {
        candidates
            .iter()
            .any(|candidate| source_matches_corpus_entry(candidate, &context.source))
            .then(|| context.clone())
    })
}

async fn load_review_loop_corpus_context(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
) -> anyhow::Result<Option<ReviewLoopCorpusContext>> {
    let row: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
    )> = sqlx::query_as(
        "select coalesce(source_kind, 'arxiv'), arxiv_id, source_id, source_uri, source_metadata \
         from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    let Some((source_kind, arxiv_id, source_id, source_uri, source_metadata)) = row else {
        return Ok(None);
    };

    let mut candidates = BTreeSet::new();
    if source_kind == "arxiv" {
        add_review_loop_source_candidate(&mut candidates, Some(&format!("arxiv:{arxiv_id}")));
    }
    add_review_loop_source_candidate(&mut candidates, Some(&arxiv_id));
    add_review_loop_source_candidate(&mut candidates, source_id.as_deref());
    add_review_loop_source_candidate(&mut candidates, source_uri.as_deref());
    add_review_loop_source_candidate(
        &mut candidates,
        source_metadata
            .get("display_label")
            .and_then(|value| value.as_str()),
    );
    add_review_loop_source_candidate(
        &mut candidates,
        source_metadata
            .get("canonical_uri")
            .and_then(|value| value.as_str()),
    );
    let adapter = source_metadata
        .get("adapter")
        .unwrap_or(&serde_json::Value::Null);
    add_review_loop_source_candidate(
        &mut candidates,
        adapter.get("path").and_then(|value| value.as_str()),
    );
    add_review_loop_source_candidate(
        &mut candidates,
        adapter.get("paper_path").and_then(|value| value.as_str()),
    );

    let corpus_path = crate::dag_apps::app_root("grokrxiv")
        .join("evals")
        .join("corpus.yaml");
    let corpus_yaml = match tokio::fs::read_to_string(&corpus_path).await {
        Ok(body) => body,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", corpus_path.display())),
    };
    let contexts = review_loop_corpus_contexts_from_yaml(&corpus_yaml)?;
    Ok(review_loop_corpus_context_for_candidates(
        &contexts,
        &candidates,
    ))
}

#[derive(Debug, Clone, Serialize)]
struct CommandRunReport {
    command: Vec<String>,
    status: String,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
}

#[derive(Debug, Clone)]
struct ReviewFixCodeTask {
    target_id: &'static str,
    language: &'static str,
    filename: &'static str,
    author_role: &'static str,
    reviewer_role: &'static str,
    fixer_role: &'static str,
    compile_program: &'static str,
    compile_args: Vec<String>,
    compile_timeout_secs: u64,
    forbidden_terms: Vec<&'static str>,
    max_attempts: usize,
}

#[derive(Debug, Clone)]
struct ReviewLoopGitHarness {
    path: PathBuf,
    branch: String,
}

impl ReviewLoopGitHarness {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path.display().to_string(),
            "branch": self.branch.clone(),
        })
    }
}

fn compact_review_fix_code_base_artifact(
    task: &ReviewFixCodeTask,
    base_artifact: serde_json::Value,
) -> serde_json::Value {
    if task.target_id != "haskell" {
        return base_artifact;
    }

    let mut compact = base_artifact;
    if let Some(obj) = compact.as_object_mut() {
        if let Some(semantic_ir) = obj.get("semantic_ir").cloned() {
            obj.insert(
                "semantic_ir".to_string(),
                compact_haskell_semantic_ir_for_code_author(&semantic_ir),
            );
        }
        if let Some(paper_math_sources) = obj.get("paper_math_sources").cloned() {
            obj.insert(
                "paper_math_sources".to_string(),
                summarize_review_loop_paper_math_sources(&paper_math_sources),
            );
        }
        if let Some(claims) = obj.get("claims").cloned() {
            obj.insert("claims".to_string(), summarize_review_loop_claims(&claims));
        }
        if let Some(knowledge_graph) = obj.get("knowledge_graph").cloned() {
            obj.insert(
                "knowledge_graph".to_string(),
                summarize_review_loop_knowledge_graph(&knowledge_graph),
            );
        }
        obj.insert(
            "haskell_semantic_contract".to_string(),
            serde_json::json!({
                "canonical_formal_sources": [
                    "semantic_ir.theorem_candidates",
                    "semantic_ir.definitions",
                    "semantic_ir.assumptions"
                ],
                "empty_theorem_candidates_expected_output": {
                    "theoremTargets": [],
                    "claims": [],
                    "allProofObligations": []
                },
                "omitted_sources_must_not_be_modeled": [
                    "claims",
                    "knowledge_graph",
                    "semantic_ir.nonformal_review_claims",
                    "semantic_ir.supporting_equations",
                    "paper_math_sources"
                ]
            }),
        );
    }
    compact
}

fn compact_haskell_semantic_ir_for_code_author(
    semantic_ir: &serde_json::Value,
) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    for key in [
        "schema_version",
        "source",
        "review_id",
        "formalization_policy",
        "knowledge_graph_summary",
        "limitations",
        "theorem_candidates",
        "definitions",
        "assumptions",
    ] {
        if let Some(value) = semantic_ir.get(key) {
            compact.insert(key.to_string(), value.clone());
        }
    }

    let supporting_equation_count = semantic_ir
        .get("supporting_equations")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    compact.insert("supporting_equations".to_string(), serde_json::json!([]));
    compact.insert(
        "supporting_equations_summary".to_string(),
        serde_json::json!({
            "count": supporting_equation_count,
            "artifact_ref": "review_loop/semantic_ir.json#/supporting_equations",
            "omitted_from_code_author_payload": true,
            "reason": "supporting equations are context, not Lean theorem targets",
        }),
    );

    let nonformal_claim_count = semantic_ir
        .get("nonformal_review_claims")
        .and_then(|value| value.as_array())
        .map(Vec::len)
        .unwrap_or(0);
    compact.insert("nonformal_review_claims".to_string(), serde_json::json!([]));
    compact.insert(
        "nonformal_review_claims_summary".to_string(),
        serde_json::json!({
            "count": nonformal_claim_count,
            "artifact_ref": "review_loop/semantic_ir.json#/nonformal_review_claims",
            "omitted_from_code_author_payload": true,
            "reason": "nonformal review claims must not become Haskell or Lean theorem obligations",
        }),
    );

    let paper_math_sources = semantic_ir
        .get("paper_math_sources")
        .map(summarize_review_loop_paper_math_sources)
        .unwrap_or_else(|| {
            serde_json::json!({
                "artifact_ref": "review_loop/paper_math_sources.json",
                "omitted_from_code_author_payload": true,
            })
        });
    compact.insert("paper_math_sources".to_string(), paper_math_sources);

    serde_json::Value::Object(compact)
}

fn summarize_review_loop_paper_math_sources(value: &serde_json::Value) -> serde_json::Value {
    let theorem_nodes = value
        .pointer("/theorem_graph/nodes")
        .and_then(|nodes| nodes.as_array())
        .map(Vec::len)
        .or_else(|| {
            value
                .pointer("/theorem_graph/theorem_graph")
                .and_then(|nodes| nodes.as_array())
                .map(Vec::len)
        })
        .unwrap_or(0);
    serde_json::json!({
        "artifact_ref": "review_loop/paper_math_sources.json",
        "omitted_from_code_author_payload": true,
        "theorem_nodes": theorem_nodes,
        "equations": value
            .pointer("/equations/equations")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
        "artifact_sources": value
            .get("artifact_sources")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
        "warnings": value
            .get("warnings")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
    })
}

fn summarize_review_loop_claims(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "artifact_ref": "review_loop/claims.json",
        "omitted_from_code_author_payload": true,
        "must_not_be_modeled_as_haskell_claims": true,
        "reason": "review claims are audit/review evidence; only semantic_ir theorem_candidates become Haskell ClaimIR values",
        "claims": value
            .get("claims")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
    })
}

fn summarize_review_loop_knowledge_graph(value: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "artifact_ref": "review_loop/knowledge_graph.json",
        "omitted_from_code_author_payload": true,
        "must_not_be_modeled_as_haskell_claims": true,
        "reason": "knowledge graph nodes may include nonformal review claims; the canonical formal theorem targets are semantic_ir.theorem_candidates only",
        "nodes": value
            .get("nodes")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
        "edges": value
            .get("edges")
            .and_then(|items| items.as_array())
            .map(Vec::len)
            .unwrap_or(0),
    })
}

fn deterministic_haskell_semantic_model_agent_run(
    role: &str,
    task: &ReviewFixCodeTask,
    base_artifact: &serde_json::Value,
) -> Option<AgentRun> {
    if task.target_id != "haskell" {
        return None;
    }
    let code = deterministic_haskell_semantic_model_code(base_artifact);
    Some(AgentRun {
        role: role.to_string(),
        runner: AgentRunnerKind::Cli,
        model: "deterministic-haskell-semantic-model".to_string(),
        output: serde_json::json!({
            "language": task.language,
            "filename": task.filename,
            "code": code,
            "notes": [
                "generated locally from compact semantic_ir theorem candidates to avoid semantic-author runner timeout"
            ],
            "confidence": 1.0,
        }),
        raw_output: Some(
            "generated locally from compact semantic_ir theorem candidates".to_string(),
        ),
        tokens_in: None,
        tokens_out: None,
        latency_ms: 0,
        cache_hit: true,
        sandbox_ref: None,
        verifier_status: None,
        verifier_notes: None,
    })
}

fn deterministic_haskell_semantic_model_code(base_artifact: &serde_json::Value) -> String {
    let semantic_ir = base_artifact
        .get("semantic_ir")
        .unwrap_or(&serde_json::Value::Null);
    let theorem_candidates = semantic_ir
        .get("theorem_candidates")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let definitions = semantic_ir
        .get("definitions")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let assumptions = semantic_ir
        .get("assumptions")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let limitations = semantic_ir
        .get("limitations")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = String::from(
        "module SemanticModel where\n\n\
data SourceSpan = SourceSpan { sourceArtifact :: String, sourceClaimId :: String, sourcePaperSourceId :: String, sourceSectionId :: String, sourceTextExcerpt :: String } deriving (Eq, Show)\n\n\
data MathType = UnknownType String | PropType | CustomType String deriving (Eq, Show)\n\n\
data Term = UnknownTerm String | Var String | RawTerm String SourceSpan deriving (Eq, Show)\n\n\
data Proposition = SemanticGap SourceSpan String | UninterpretedPredicate String [Term] SourceSpan | Equals Term Term | Implies Proposition Proposition | And [Proposition] deriving (Eq, Show)\n\n\
data Binder = Binder { binderName :: String, binderType :: MathType, binderSpan :: SourceSpan } deriving (Eq, Show)\n\n\
data Definition = Definition { definitionId :: String, definitionStatement :: String, definitionSpan :: SourceSpan } deriving (Eq, Show)\n\n\
data Assumption = Assumption { assumptionId :: String, assumptionStatement :: String, assumptionSpan :: SourceSpan } deriving (Eq, Show)\n\n\
data Limitation = Limitation { limitationId :: String, limitationKind :: String, limitationStatement :: String, limitationSpan :: SourceSpan } deriving (Eq, Show)\n\n\
data LeanTarget = LeanTarget { targetDeclaration :: String, targetExpectedShape :: String, targetSource :: SourceSpan } deriving (Eq, Show)\n\n\
data SemanticCategory = SemCatEquivalence | SemCatPlainTheorem | SemCatInvariantPreservation | SemCatOther String deriving (Eq, Show)\n\n\
data TheoremKind = KindEquivalence | KindTheorem | KindInvariant | KindEquation | KindOther String deriving (Eq, Show)\n\n\
data FormalizationClass = FormalMath | InformalProse | FCOther String deriving (Eq, Show)\n\n\
data TranscriptionStatus = StatusTranscribed | StatusPartial | StatusUntranscribed | StatusOther String deriving (Eq, Show)\n\n\
data TheoremIR = TheoremIR { theoremId :: String, theoremStatement :: String, theoremSpan :: SourceSpan, theoremBinders :: [Binder], theoremAssumptions :: [Proposition], theoremConclusion :: Proposition, theoremTarget :: LeanTarget, theoremKind :: TheoremKind, theoremSemanticCategory :: SemanticCategory, theoremFormalizationClass :: FormalizationClass, theoremTranscriptionStatus :: TranscriptionStatus } deriving (Eq, Show)\n\n\
data ClaimIR = ClaimIR { claimId :: String, claimRawText :: String, claimSource :: SourceSpan, claimSemanticCategory :: SemanticCategory, claimTheorem :: Maybe TheoremIR } deriving (Eq, Show)\n\n\
data ProofObligation = ProofObligation { obligationId :: String, obligationStatement :: Proposition, obligationSource :: SourceSpan, obligationLean :: LeanTarget } deriving (Eq, Show)\n\n",
    );

    out.push_str("definitions :: [Definition]\ndefinitions =\n");
    out.push_str(&haskell_list(
        definitions
            .iter()
            .enumerate()
            .map(|(idx, definition)| {
                format!(
                    "Definition {} {} {}",
                    haskell_string_literal(&json_string_or_fallback(
                        definition,
                        &["id", "name"],
                        &format!("definition_{}", idx + 1),
                    )),
                    haskell_string_literal(&json_string_or_fallback(
                        definition,
                        &["statement", "text", "label"],
                        "unknown definition",
                    )),
                    haskell_source_span_literal(definition, &format!("definition_{}", idx + 1))
                )
            })
            .collect::<Vec<_>>(),
    ));
    out.push_str("\n\n");

    out.push_str("globalAssumptions :: [Assumption]\nglobalAssumptions =\n");
    out.push_str(&haskell_list(
        assumptions
            .iter()
            .enumerate()
            .map(|(idx, assumption)| {
                format!(
                    "Assumption {} {} {}",
                    haskell_string_literal(&json_string_or_fallback(
                        assumption,
                        &["id", "name"],
                        &format!("assumption_{}", idx + 1),
                    )),
                    haskell_string_literal(&json_string_or_fallback(
                        assumption,
                        &["statement", "text", "label"],
                        "unknown assumption",
                    )),
                    haskell_source_span_literal(assumption, &format!("assumption_{}", idx + 1))
                )
            })
            .collect::<Vec<_>>(),
    ));
    out.push_str("\n\n");

    out.push_str("limitations :: [Limitation]\nlimitations =\n");
    out.push_str(&haskell_list(
        limitations
            .iter()
            .enumerate()
            .map(|(idx, limitation)| {
                format!(
                    "Limitation {} {} {} {}",
                    haskell_string_literal(&json_string_or_fallback(
                        limitation,
                        &["id", "limitation_id"],
                        &format!("limitation_{}", idx + 1),
                    )),
                    haskell_string_literal(&json_string_or_fallback(
                        limitation,
                        &["kind"],
                        "semantic_gap",
                    )),
                    haskell_string_literal(&json_string_or_fallback(
                        limitation,
                        &["statement", "message"],
                        "semantic limitation",
                    )),
                    haskell_source_span_literal(limitation, &format!("limitation_{}", idx + 1))
                )
            })
            .collect::<Vec<_>>(),
    ));
    out.push_str("\n\n");

    let theorem_names = theorem_candidates
        .iter()
        .enumerate()
        .map(|(idx, theorem)| {
            let lean_decl = theorem
                .get("formalization_target")
                .and_then(|target| target.get("lean_declaration"))
                .and_then(|value| value.as_str())
                .or_else(|| theorem.get("id").and_then(|value| value.as_str()))
                .unwrap_or("theorem_target");
            format!(
                "theorem_{}_{}",
                idx + 1,
                haskell_identifier_suffix(lean_decl)
            )
        })
        .collect::<Vec<_>>();

    for (idx, theorem) in theorem_candidates.iter().enumerate() {
        let name = &theorem_names[idx];
        let theorem_id = json_string_or_fallback(theorem, &["id"], &format!("theorem_{}", idx + 1));
        let statement = json_string_or_fallback(theorem, &["statement", "text"], "unknown theorem");
        let lean_decl = theorem
            .get("formalization_target")
            .and_then(|target| target.get("lean_declaration"))
            .and_then(|value| value.as_str())
            .unwrap_or(&theorem_id);
        let expected_shape = theorem
            .get("formalization_target")
            .and_then(|target| target.get("expected_shape"))
            .and_then(|value| value.as_str())
            .unwrap_or("theorem");
        let theorem_kind =
            haskell_theorem_kind_literal(&json_string_or_fallback(theorem, &["kind"], "other"));
        let semantic_category = haskell_semantic_category_literal(&json_string_or_fallback(
            theorem,
            &["semantic_category"],
            "other",
        ));
        let formalization_class_raw =
            json_string_or_fallback(theorem, &["formalization_class"], "formal_math");
        let formalization_class = haskell_formalization_class_literal(&formalization_class_raw);
        let transcription_status_raw = theorem
            .pointer("/typed_transcription/status")
            .or_else(|| theorem.get("transcription_status"))
            .and_then(|value| value.as_str())
            .unwrap_or_else(|| {
                if theorem.pointer("/theorem_ir/conclusion/kind")
                    == Some(&serde_json::Value::String("unknown_prop".to_string()))
                {
                    "partial"
                } else {
                    "transcribed"
                }
            });
        let transcription_status = haskell_transcription_status_literal(transcription_status_raw);
        let span = haskell_source_span_literal(theorem, &theorem_id);
        let conclusion = theorem
            .pointer("/theorem_ir/conclusion")
            .or_else(|| theorem.pointer("/typed_transcription/conclusion"))
            .map(|value| haskell_proposition_literal(value, "span"))
            .unwrap_or_else(|| {
                format!(
                    "SemanticGap span {}",
                    haskell_string_literal(&format!(
                        "typed conclusion unavailable for {lean_decl}"
                    ))
                )
            });
        let theorem_assumptions = haskell_inline_list(
            theorem
                .pointer("/theorem_ir/assumptions")
                .or_else(|| theorem.pointer("/typed_transcription/assumptions"))
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|value| haskell_proposition_literal(value, "span"))
                .collect::<Vec<_>>(),
        );
        let theorem_binders = haskell_inline_list(
            theorem
                .pointer("/theorem_ir/binders")
                .or_else(|| theorem.pointer("/typed_transcription/binders"))
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(binder_idx, value)| {
                    format!(
                        "Binder {} (UnknownType \"not structurally typed in Phase 0\") span",
                        haskell_string_literal(&json_string_or_fallback(
                            value,
                            &["name", "id"],
                            &format!("binder_{}", binder_idx + 1),
                        ))
                    )
                })
                .collect::<Vec<_>>(),
        );
        out.push_str(&format!("{name} :: TheoremIR\n"));
        out.push_str(&format!("{name} =\n"));
        out.push_str(&format!("  let span = {span}\n"));
        out.push_str(&format!("      conclusion = {conclusion}\n"));
        out.push_str(&format!(
            "      target = LeanTarget {} {} span\n",
            haskell_string_literal(lean_decl),
            haskell_string_literal(expected_shape)
        ));
        out.push_str("  in TheoremIR\n");
        out.push_str(&format!(
            "       {{ theoremId = {}\n",
            haskell_string_literal(&theorem_id)
        ));
        out.push_str(&format!(
            "       , theoremStatement = {}\n",
            haskell_string_literal(&statement)
        ));
        out.push_str("       , theoremSpan = span\n");
        out.push_str(&format!("       , theoremBinders = {theorem_binders}\n"));
        out.push_str(&format!(
            "       , theoremAssumptions = {theorem_assumptions}\n"
        ));
        out.push_str("       , theoremConclusion = conclusion\n");
        out.push_str("       , theoremTarget = target\n");
        out.push_str(&format!("       , theoremKind = {theorem_kind}\n"));
        out.push_str(&format!(
            "       , theoremSemanticCategory = {semantic_category}\n"
        ));
        out.push_str(&format!(
            "       , theoremFormalizationClass = {formalization_class}\n"
        ));
        out.push_str(&format!(
            "       , theoremTranscriptionStatus = {transcription_status}\n"
        ));
        out.push_str("       }\n\n");
    }

    out.push_str("theoremTargets :: [TheoremIR]\ntheoremTargets =\n");
    out.push_str(&haskell_list(theorem_names.clone()));
    out.push_str("\n\nclaims :: [ClaimIR]\nclaims =\n");
    out.push_str(&haskell_list(
        theorem_names
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let semantic_category = haskell_semantic_category_literal(
                    &json_string_or_fallback(
                        &theorem_candidates[idx],
                        &["semantic_category", "kind"],
                        "other",
                    ),
                );
                let semantic_category_arg = haskell_constructor_arg(&semantic_category);
                format!(
                    "ClaimIR (theoremId {name}) (theoremStatement {name}) (theoremSpan {name}) {semantic_category_arg} (Just {name})"
                )
            })
            .collect::<Vec<_>>(),
    ));
    out.push_str("\n\ncategoryToObligations :: ClaimIR -> [ProofObligation]\n");
    out.push_str("categoryToObligations = claimToObligations\n\n");
    out.push_str("isProofReadyConclusion :: Proposition -> Bool\n");
    out.push_str("isProofReadyConclusion (SemanticGap _ _) = False\n");
    out.push_str("isProofReadyConclusion _ = True\n\n");
    out.push_str("isProofReadyTheorem :: TheoremIR -> Bool\n");
    out.push_str("isProofReadyTheorem theorem =\n");
    out.push_str("  theoremTranscriptionStatus theorem == StatusTranscribed\n");
    out.push_str("    && isProofReadyConclusion (theoremConclusion theorem)\n\n");
    out.push_str("claimToObligations :: ClaimIR -> [ProofObligation]\n");
    out.push_str("claimToObligations claim =\n");
    out.push_str("  case claimTheorem claim of\n");
    out.push_str("    Nothing -> []\n");
    out.push_str("    Just theorem ->\n");
    out.push_str("      case theoremFormalizationClass theorem of\n");
    out.push_str("        FormalMath | isProofReadyTheorem theorem ->\n");
    out.push_str("          [ ProofObligation\n");
    out.push_str("              (theoremId theorem)\n");
    out.push_str("              (theoremConclusion theorem)\n");
    out.push_str("              (theoremSpan theorem)\n");
    out.push_str("              (theoremTarget theorem)\n");
    out.push_str("          ]\n");
    out.push_str("        _ -> []\n\n");
    out.push_str("obligationToLean :: ProofObligation -> LeanTarget\n");
    out.push_str("obligationToLean = obligationLean\n\n");
    out.push_str("allProofObligations :: [ProofObligation]\n");
    out.push_str("allProofObligations = concatMap categoryToObligations claims\n");
    out
}

fn haskell_list(items: Vec<String>) -> String {
    if items.is_empty() {
        "  []".to_string()
    } else {
        format!("  [ {}\n  ]", items.join("\n  , "))
    }
}

fn haskell_inline_list(items: Vec<String>) -> String {
    if items.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

fn haskell_theorem_kind_literal(raw: &str) -> String {
    match haskell_normalized_tag(raw).as_str() {
        "equivalence" => "KindEquivalence".to_string(),
        "theorem" | "plain_theorem" => "KindTheorem".to_string(),
        "invariant" | "invariant_preservation" => "KindInvariant".to_string(),
        "equation" | "construction" => "KindEquation".to_string(),
        _ => format!("KindOther {}", haskell_string_literal(raw)),
    }
}

fn haskell_semantic_category_literal(raw: &str) -> String {
    match haskell_normalized_tag(raw).as_str() {
        "equivalence" => "SemCatEquivalence".to_string(),
        "plain_theorem" | "theorem" => "SemCatPlainTheorem".to_string(),
        "invariant_preservation" | "invariant" => "SemCatInvariantPreservation".to_string(),
        _ => format!("SemCatOther {}", haskell_string_literal(raw)),
    }
}

fn haskell_formalization_class_literal(raw: &str) -> String {
    match haskell_normalized_tag(raw).as_str() {
        "formal_math" => "FormalMath".to_string(),
        "informal_prose" => "InformalProse".to_string(),
        _ => format!("FCOther {}", haskell_string_literal(raw)),
    }
}

fn haskell_transcription_status_literal(raw: &str) -> String {
    match haskell_normalized_tag(raw).as_str() {
        "transcribed" => "StatusTranscribed".to_string(),
        "partial" => "StatusPartial".to_string(),
        "untranscribed" | "unknown" => "StatusUntranscribed".to_string(),
        _ => format!("StatusOther {}", haskell_string_literal(raw)),
    }
}

fn haskell_constructor_arg(value: &str) -> String {
    if value.contains(char::is_whitespace) {
        format!("({value})")
    } else {
        value.to_string()
    }
}

fn haskell_normalized_tag(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('-', "_")
}

fn haskell_proposition_literal(value: &serde_json::Value, span_expr: &str) -> String {
    match value.get("kind").and_then(|kind| kind.as_str()) {
        Some("equals") => format!(
            "Equals ({}) ({})",
            haskell_term_literal(
                value.get("lhs").unwrap_or(&serde_json::Value::Null),
                span_expr
            ),
            haskell_term_literal(
                value.get("rhs").unwrap_or(&serde_json::Value::Null),
                span_expr
            )
        ),
        Some("implies") => format!(
            "Implies ({}) ({})",
            haskell_proposition_literal(
                value
                    .get("premise")
                    .or_else(|| value.get("lhs"))
                    .unwrap_or(&serde_json::Value::Null),
                span_expr
            ),
            haskell_proposition_literal(
                value
                    .get("conclusion")
                    .or_else(|| value.get("rhs"))
                    .unwrap_or(&serde_json::Value::Null),
                span_expr
            )
        ),
        Some("and") => {
            let parts = value
                .get("parts")
                .or_else(|| value.get("items"))
                .and_then(|items| items.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|item| haskell_proposition_literal(item, span_expr))
                .collect::<Vec<_>>();
            format!("And {}", haskell_inline_list(parts))
        }
        Some("unknown_prop") => format!(
            "SemanticGap {span_expr} {}",
            haskell_string_literal(&json_string_or_fallback(
                value,
                &["text", "reason", "statement"],
                "unknown_prop"
            ))
        ),
        Some(kind) => format!(
            "UninterpretedPredicate {} [] {span_expr}",
            haskell_string_literal(kind)
        ),
        None => format!(
            "SemanticGap {span_expr} {}",
            haskell_string_literal("proposition not structurally typed in Phase 0")
        ),
    }
}

fn haskell_term_literal(value: &serde_json::Value, span_expr: &str) -> String {
    match value.get("kind").and_then(|kind| kind.as_str()) {
        Some("var") => format!(
            "Var {}",
            haskell_string_literal(&json_string_or_fallback(value, &["name", "id"], "unknown"))
        ),
        Some("raw") => format!(
            "RawTerm {} {span_expr}",
            haskell_string_literal(&json_string_or_fallback(
                value,
                &["text", "name"],
                "raw term"
            ))
        ),
        Some(kind) => format!(
            "RawTerm {} {span_expr}",
            haskell_string_literal(&format!(
                "{kind}: {}",
                json_string_or_fallback(value, &["name", "text", "id"], "unknown term")
            ))
        ),
        None => format!(
            "UnknownTerm {}",
            haskell_string_literal("term not structurally typed in Phase 0")
        ),
    }
}

fn haskell_identifier_suffix(raw: &str) -> String {
    let mut suffix = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while suffix.contains("__") {
        suffix = suffix.replace("__", "_");
    }
    suffix = suffix.trim_matches('_').to_string();
    if suffix.is_empty() {
        "target".to_string()
    } else if suffix.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("target_{suffix}")
    } else {
        suffix
    }
}

fn json_string_or_fallback(value: &serde_json::Value, keys: &[&str], fallback: &str) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(|item| item.as_str()))
        .unwrap_or(fallback)
        .to_string()
}

fn haskell_source_span_literal(value: &serde_json::Value, fallback_claim: &str) -> String {
    let span = value.get("source_span").unwrap_or(value);
    let artifact = span
        .get("artifact")
        .and_then(|value| value.as_str())
        .unwrap_or("review_loop/semantic_ir.json");
    let claim_id = span
        .get("claim_id")
        .or_else(|| span.get("claimId"))
        .or_else(|| value.get("id"))
        .and_then(|value| value.as_str())
        .unwrap_or(fallback_claim);
    let paper_source_id = span
        .get("paper_source_id")
        .or_else(|| span.get("paperSourceId"))
        .or_else(|| span.get("source_id"))
        .and_then(|value| value.as_str())
        .unwrap_or(claim_id);
    let section_id = span
        .get("section_id")
        .or_else(|| span.get("sectionId"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let text_excerpt = span
        .get("text_excerpt")
        .or_else(|| span.get("textExcerpt"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    format!(
        "(SourceSpan {} {} {} {} {})",
        haskell_string_literal(artifact),
        haskell_string_literal(claim_id),
        haskell_string_literal(paper_source_id),
        haskell_string_literal(section_id),
        haskell_string_literal(text_excerpt)
    )
}

fn haskell_string_literal(raw: &str) -> String {
    let mut out = String::from("\"");
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push(' '),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

async fn recovered_agent_run_from_code_file(
    role: &str,
    task: &ReviewFixCodeTask,
    final_path: &Path,
    started_at: std::time::SystemTime,
    runner_error: &str,
) -> anyhow::Result<Option<AgentRun>> {
    let metadata = match tokio::fs::metadata(final_path).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("stat {}", final_path.display()));
        }
    };
    if metadata.len() == 0 {
        return Ok(None);
    }
    if let Ok(modified_at) = metadata.modified() {
        let cutoff = started_at
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or(started_at);
        if modified_at < cutoff {
            return Ok(None);
        }
    }
    let code = tokio::fs::read_to_string(final_path)
        .await
        .with_context(|| format!("read {}", final_path.display()))?;
    if code.trim().is_empty() {
        return Ok(None);
    }

    let note = format!(
        "recovered from on-disk artifact after runner failure: {}",
        truncate(runner_error, 260)
    );
    Ok(Some(AgentRun {
        role: role.to_string(),
        runner: AgentRunnerKind::Cli,
        model: "recovered-on-disk-artifact".to_string(),
        output: serde_json::json!({
            "language": task.language,
            "filename": task.filename,
            "code": code,
            "notes": [note],
            "confidence": 0.0,
        }),
        raw_output: Some(format!(
            "recovered from on-disk artifact after runner failure at {}",
            final_path.display()
        )),
        tokens_in: None,
        tokens_out: None,
        latency_ms: 0,
        cache_hit: true,
        sandbox_ref: None,
        verifier_status: None,
        verifier_notes: None,
    }))
}

async fn run_review_fix_code_loop(
    state: &super::AppState,
    paper_id: Uuid,
    review_id: Uuid,
    task: ReviewFixCodeTask,
    base_artifact: serde_json::Value,
    workdir: &Path,
    final_path: &Path,
    debug_output: bool,
) -> serde_json::Value {
    let mut attempts = Vec::new();
    let mut previous_code: Option<String> = None;
    let mut previous_compile: Option<serde_json::Value> = None;
    let mut previous_review: Option<serde_json::Value> = None;
    let mut final_status = "fail".to_string();
    let base_artifact = compact_review_fix_code_base_artifact(&task, base_artifact);
    let harness =
        match prepare_review_loop_git_harness(review_id, &task, base_artifact.clone(), workdir)
            .await
        {
            Ok(harness) => harness,
            Err(err) => {
                return serde_json::json!({
                    "stage": format!("{}_review_fix_code", task.target_id),
                    "target": task.target_id,
                    "language": task.language,
                    "filename": task.filename,
                    "author_role": task.author_role,
                    "reviewer_role": task.reviewer_role,
                    "fixer_role": task.fixer_role,
                    "max_attempts": task.max_attempts,
                    "attempts": [{
                        "attempt": 0,
                        "status": "fail",
                        "harness_error": format!("{err:#}"),
                    }],
                    "agent_output_audit_summary": {
                        "total": 0,
                        "accepted": 0,
                        "rejected": 0,
                        "by_role": {}
                    },
                    "status": "fail",
                    "final_path": final_path.display().to_string(),
                    "harness": {
                        "path": workdir.display().to_string(),
                        "branch": review_loop_harness_branch(review_id, task.target_id),
                    },
                });
            }
        };
    let artifact_root = workdir.parent().unwrap_or(workdir).to_path_buf();

    for attempt in 1..=task.max_attempts {
        let round_dir = workdir.join(format!("round_{attempt}"));
        if let Err(err) = tokio::fs::create_dir_all(&round_dir).await {
            attempts.push(serde_json::json!({
                "attempt": attempt,
                "status": "fail",
                "error": format!("create round dir {}: {err}", round_dir.display()),
            }));
            break;
        }

        let role = if attempt == 1 {
            task.author_role
        } else {
            task.fixer_role
        };
        let agent_artifact = serde_json::json!({
            "phase": "generate",
            "target": task.target_id,
            "language": task.language,
            "filename": task.filename,
            "attempt": attempt,
            "max_attempts": task.max_attempts,
            "base": base_artifact,
            "previous_code": previous_code,
            "previous_compile": previous_compile,
            "previous_codex_review": previous_review,
            "harness": harness.as_json(),
        });
        if debug_output {
            cli_status::emit_detail(
                role,
                cli_status::StatusMark::Run,
                &format!(
                    "attempt={attempt} target={} branch={} path={}",
                    task.target_id,
                    harness.branch,
                    harness.path.display()
                ),
            );
        }
        let mut generation_recovery: Option<serde_json::Value> = None;
        let generation_run = if attempt == 1 {
            match deterministic_haskell_semantic_model_agent_run(role, &task, &base_artifact) {
                Some(run) => {
                    generation_recovery = Some(serde_json::json!({
                        "status": "deterministic_local_author",
                        "reason": "generated Haskell scaffold locally from compact semantic_ir theorem candidates",
                    }));
                    run
                }
                None => {
                    let generation_started_at = std::time::SystemTime::now();
                    match run_review_loop_agent(
                        state,
                        paper_id,
                        review_id,
                        role,
                        agent_artifact.clone(),
                        review_loop_code_system_prompt(&task, role, attempt),
                        review_loop_code_user_prompt(&task, role, attempt),
                        Some(&harness.path),
                    )
                    .await
                    {
                        Ok(run) => run,
                        Err(err) => {
                            match recovered_agent_run_from_code_file(
                                role,
                                &task,
                                final_path,
                                generation_started_at,
                                &format!("{err:#}"),
                            )
                            .await
                            {
                                Ok(Some(run)) => {
                                    generation_recovery = Some(serde_json::json!({
                                        "status": "recovered_from_file",
                                        "reason": format!("{err:#}"),
                                        "path": final_path.display().to_string(),
                                    }));
                                    run
                                }
                                Ok(None) => {
                                    let audit = write_review_loop_agent_output_audit(
                            &artifact_root,
                            &task,
                            attempt,
                            role,
                            "generate",
                            &agent_artifact,
                            None,
                            None,
                            None,
                            "rejected",
                            &format!("{err:#}"),
                        )
                        .await
                        .unwrap_or_else(|audit_err| {
                            serde_json::json!({
                                "role": role,
                                "phase": "generate",
                                "attempt": attempt,
                                "decision": {
                                    "status": "rejected",
                                    "reason": format!("agent failed: {err:#}; audit write failed: {audit_err:#}")
                                }
                            })
                        });
                                    attempts.push(serde_json::json!({
                                        "attempt": attempt,
                                        "status": "fail",
                                        "author_role": role,
                                        "author_error": format!("{err:#}"),
                                        "agent_output_audits": [audit],
                                    }));
                                    break;
                                }
                                Err(recovery_err) => {
                                    let decision_reason =
                            format!("{err:#}; on-disk artifact recovery failed: {recovery_err:#}");
                                    let audit = write_review_loop_agent_output_audit(
                            &artifact_root,
                            &task,
                            attempt,
                            role,
                            "generate",
                            &agent_artifact,
                            None,
                            None,
                            None,
                            "rejected",
                            &decision_reason,
                        )
                        .await
                        .unwrap_or_else(|audit_err| {
                            serde_json::json!({
                                "role": role,
                                "phase": "generate",
                                "attempt": attempt,
                                "decision": {
                                    "status": "rejected",
                                    "reason": format!("agent failed: {err:#}; recovery failed: {recovery_err:#}; audit write failed: {audit_err:#}")
                                }
                            })
                        });
                                    attempts.push(serde_json::json!({
                                        "attempt": attempt,
                                        "status": "fail",
                                        "author_role": role,
                                        "author_error": format!("{err:#}"),
                                        "recovery_error": format!("{recovery_err:#}"),
                                        "agent_output_audits": [audit],
                                    }));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        } else {
            let generation_started_at = std::time::SystemTime::now();
            match run_review_loop_agent(
                state,
                paper_id,
                review_id,
                role,
                agent_artifact.clone(),
                review_loop_code_system_prompt(&task, role, attempt),
                review_loop_code_user_prompt(&task, role, attempt),
                Some(&harness.path),
            )
            .await
            {
                Ok(run) => run,
                Err(err) => {
                    match recovered_agent_run_from_code_file(
                        role,
                        &task,
                        final_path,
                        generation_started_at,
                        &format!("{err:#}"),
                    )
                    .await
                    {
                        Ok(Some(run)) => {
                            generation_recovery = Some(serde_json::json!({
                                "status": "recovered_from_file",
                                "reason": format!("{err:#}"),
                                "path": final_path.display().to_string(),
                            }));
                            run
                        }
                        Ok(None) => {
                            let audit = write_review_loop_agent_output_audit(
                                    &artifact_root,
                                    &task,
                                    attempt,
                                    role,
                                    "generate",
                                    &agent_artifact,
                                    None,
                                    None,
                                    None,
                                    "rejected",
                                    &format!("{err:#}"),
                                )
                                .await
                                .unwrap_or_else(|audit_err| {
                                    serde_json::json!({
                                        "role": role,
                                        "phase": "generate",
                                        "attempt": attempt,
                                        "decision": {
                                            "status": "rejected",
                                            "reason": format!("agent failed: {err:#}; audit write failed: {audit_err:#}")
                                        }
                                    })
                                });
                            attempts.push(serde_json::json!({
                                "attempt": attempt,
                                "status": "fail",
                                "author_role": role,
                                "author_error": format!("{err:#}"),
                                "agent_output_audits": [audit],
                            }));
                            break;
                        }
                        Err(recovery_err) => {
                            let decision_reason = format!(
                                "{err:#}; on-disk artifact recovery failed: {recovery_err:#}"
                            );
                            let audit = write_review_loop_agent_output_audit(
                                    &artifact_root,
                                    &task,
                                    attempt,
                                    role,
                                    "generate",
                                    &agent_artifact,
                                    None,
                                    None,
                                    None,
                                    "rejected",
                                    &decision_reason,
                                )
                                .await
                                .unwrap_or_else(|audit_err| {
                                    serde_json::json!({
                                        "role": role,
                                        "phase": "generate",
                                        "attempt": attempt,
                                        "decision": {
                                            "status": "rejected",
                                            "reason": format!("agent failed: {err:#}; recovery failed: {recovery_err:#}; audit write failed: {audit_err:#}")
                                        }
                                    })
                                });
                            attempts.push(serde_json::json!({
                                "attempt": attempt,
                                "status": "fail",
                                "author_role": role,
                                "author_error": format!("{err:#}"),
                                "recovery_error": format!("{recovery_err:#}"),
                                "agent_output_audits": [audit],
                            }));
                            break;
                        }
                    }
                }
            }
        };
        let generation_path = round_dir.join("generation.json");
        let _ = write_loop_json(&generation_path, &generation_run.output).await;
        let code = match code_from_agent_run(&generation_run, &task) {
            Ok(code) => code,
            Err(err) => {
                let audit = write_review_loop_agent_output_audit(
                    &artifact_root,
                    &task,
                    attempt,
                    role,
                    "generate",
                    &agent_artifact,
                    Some(&generation_run),
                    None,
                    None,
                    "rejected",
                    &format!("{err:#}"),
                )
                .await
                .unwrap_or_else(|audit_err| {
                    serde_json::json!({
                        "role": role,
                        "phase": "generate",
                        "attempt": attempt,
                        "decision": {
                            "status": "rejected",
                            "reason": format!("generated code extraction failed: {err:#}; audit write failed: {audit_err:#}")
                        }
                    })
                });
                attempts.push(serde_json::json!({
                    "attempt": attempt,
                    "status": "fail",
                    "author_role": role,
                    "generation": generation_run.output,
                    "author_error": format!("{err:#}"),
                    "agent_output_audits": [audit],
                }));
                break;
            }
        };
        if let Err(err) = write_review_loop_code_file(final_path, &code).await {
            let audit = write_review_loop_agent_output_audit(
                &artifact_root,
                &task,
                attempt,
                role,
                "generate",
                &agent_artifact,
                Some(&generation_run),
                None,
                None,
                "rejected",
                &format!("write {}: {err}", final_path.display()),
            )
            .await
            .unwrap_or_else(|audit_err| {
                serde_json::json!({
                    "role": role,
                    "phase": "generate",
                    "attempt": attempt,
                    "decision": {
                        "status": "rejected",
                        "reason": format!("generated code write failed: {err}; audit write failed: {audit_err:#}")
                    }
                })
            });
            attempts.push(serde_json::json!({
                "attempt": attempt,
                "status": "fail",
                "author_role": role,
                "generation": generation_run.output,
                "write_error": format!("write {}: {err}", final_path.display()),
                "agent_output_audits": [audit],
            }));
            break;
        }
        let round_source_path = round_dir.join(task.filename);
        let _ = write_review_loop_code_file(&round_source_path, &code).await;

        let semantic_issues =
            grokrxiv_review_loop::validate_generated_code(task.target_id, &code, &base_artifact);
        let semantic_validation = serde_json::json!({
            "status": if semantic_issues.is_empty() { "pass" } else { "fail" },
            "issues": semantic_issues,
        });
        let _ = write_loop_json(
            &round_dir.join("semantic_validation.json"),
            &semantic_validation,
        )
        .await;

        let forbidden = forbidden_terms_in_code(&code, &task.forbidden_terms);
        let compile_run = if forbidden.is_empty() {
            let compile_args = task
                .compile_args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            run_loop_command(
                task.compile_program,
                &compile_args,
                workdir,
                std::time::Duration::from_secs(task.compile_timeout_secs),
            )
            .await
        } else {
            CommandRunReport {
                command: std::iter::once(task.compile_program.to_string())
                    .chain(task.compile_args.iter().cloned())
                    .collect(),
                status: "fail".to_string(),
                exit_code: None,
                stdout: String::new(),
                stderr: format!(
                    "generated code contains forbidden terms: {}",
                    forbidden.join(", ")
                ),
                duration_ms: 0,
            }
        };
        let compile_value =
            serde_json::to_value(&compile_run).unwrap_or_else(|_| serde_json::json!({}));
        let _ = write_loop_json(&round_dir.join("compile.json"), &compile_value).await;

        let reviewer_artifact = serde_json::json!({
            "phase": "review",
            "target": task.target_id,
            "language": task.language,
            "filename": task.filename,
            "attempt": attempt,
            "max_attempts": task.max_attempts,
            "code": code,
            "compile": compile_value,
            "semantic_validation": semantic_validation,
            "forbidden_terms": forbidden,
            "base": base_artifact,
            "harness": harness.as_json(),
        });
        let (review_output, review_run, review_error) = match run_review_loop_agent(
            state,
            paper_id,
            review_id,
            task.reviewer_role,
            reviewer_artifact.clone(),
            review_loop_code_system_prompt(&task, task.reviewer_role, attempt),
            review_loop_code_user_prompt(&task, task.reviewer_role, attempt),
            Some(&harness.path),
        )
        .await
        {
            Ok(run) => (run.output.clone(), Some(run), None),
            Err(err) => (
                serde_json::json!({
                    "status": "fail",
                    "issues": [
                        {
                            "severity": "blocking",
                            "message": format!("Codex review agent failed: {err:#}"),
                            "line": null
                        }
                    ],
                    "summary": "Codex review agent failed.",
                    "confidence": 1.0
                }),
                None,
                Some(format!("{err:#}")),
            ),
        };
        let _ = write_loop_json(&round_dir.join("codex_review.json"), &review_output).await;

        let compile_pass = compile_run.status == "pass" && forbidden.is_empty();
        let semantic_pass = semantic_validation["status"] == "pass";
        let review_pass = code_review_passed(&review_output);
        let attempt_status = if compile_pass && semantic_pass && review_pass {
            "pass"
        } else {
            "fail"
        };
        let generation_decision_reason = if attempt_status == "pass" {
            if generation_recovery.is_some() {
                "generated artifact recovered from on-disk file after runner failure and accepted by schema, semantic validator, compiler, and reviewer".to_string()
            } else {
                "generated artifact accepted by schema, semantic validator, compiler, and reviewer"
                    .to_string()
            }
        } else {
            review_fix_attempt_rejection_reason(&semantic_validation, &compile_run, &review_output)
        };
        let generation_audit = write_review_loop_agent_output_audit(
            &artifact_root,
            &task,
            attempt,
            role,
            "generate",
            &agent_artifact,
            Some(&generation_run),
            Some(&semantic_validation),
            Some(&compile_value),
            if attempt_status == "pass" {
                "accepted"
            } else {
                "rejected"
            },
            &generation_decision_reason,
        )
        .await
        .unwrap_or_else(|audit_err| {
            serde_json::json!({
                "role": role,
                "phase": "generate",
                "attempt": attempt,
                "decision": {
                    "status": "rejected",
                    "reason": format!("audit write failed: {audit_err:#}")
                }
            })
        });
        let reviewer_decision_reason = review_error.unwrap_or_else(|| {
            format!(
                "reviewer output schema-valid with status={}",
                review_output
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
            )
        });
        let reviewer_audit = write_review_loop_agent_output_audit(
            &artifact_root,
            &task,
            attempt,
            task.reviewer_role,
            "review",
            &reviewer_artifact,
            review_run.as_ref(),
            Some(&semantic_validation),
            Some(&compile_value),
            if review_run.is_some() {
                "accepted"
            } else {
                "rejected"
            },
            &reviewer_decision_reason,
        )
        .await
        .unwrap_or_else(|audit_err| {
            serde_json::json!({
                "role": task.reviewer_role,
                "phase": "review",
                "attempt": attempt,
                "decision": {
                    "status": "rejected",
                    "reason": format!("audit write failed: {audit_err:#}")
                }
            })
        });
        let agent_output_audits = vec![generation_audit, reviewer_audit];
        let git_evidence =
            record_review_loop_harness_attempt(&harness, task.target_id, attempt).await;
        attempts.push(serde_json::json!({
            "attempt": attempt,
            "status": attempt_status,
            "author_role": role,
            "reviewer_role": task.reviewer_role,
            "generation_recovery": generation_recovery,
            "source_path": round_source_path.display().to_string(),
            "final_path": final_path.display().to_string(),
            "harness": harness.as_json(),
            "git": git_evidence,
            "generation": generation_run.output,
            "semantic_validation": semantic_validation,
            "compile": compile_run,
            "compile_timeout_secs": task.compile_timeout_secs,
            "codex_review": review_output,
            "agent_output_audits": agent_output_audits,
        }));

        if attempt_status == "pass" {
            final_status = "pass".to_string();
            break;
        }
        previous_code = Some(
            tokio::fs::read_to_string(final_path)
                .await
                .unwrap_or_default(),
        );
        previous_compile = attempts
            .last()
            .and_then(|attempt| attempt.get("compile").cloned());
        previous_review = attempts
            .last()
            .and_then(|attempt| attempt.get("codex_review").cloned());
    }

    let agent_output_audit_summary = review_fix_loop_agent_output_audit_summary(
        &serde_json::json!({ "attempts": attempts.clone() }),
    );

    serde_json::json!({
        "stage": format!("{}_review_fix_code", task.target_id),
        "target": task.target_id,
        "language": task.language,
        "filename": task.filename,
        "author_role": task.author_role,
        "reviewer_role": task.reviewer_role,
        "fixer_role": task.fixer_role,
        "compile_timeout_secs": task.compile_timeout_secs,
        "max_attempts": task.max_attempts,
        "attempts": attempts,
        "agent_output_audit_summary": agent_output_audit_summary,
        "status": final_status,
        "final_path": final_path.display().to_string(),
        "harness": harness.as_json(),
    })
}

async fn prepare_review_loop_git_harness(
    review_id: Uuid,
    task: &ReviewFixCodeTask,
    base_artifact: serde_json::Value,
    workdir: &Path,
) -> anyhow::Result<ReviewLoopGitHarness> {
    tokio::fs::create_dir_all(workdir)
        .await
        .with_context(|| format!("create review-loop harness {}", workdir.display()))?;
    let branch = review_loop_harness_branch(review_id, task.target_id);
    let harness = ReviewLoopGitHarness {
        path: workdir.to_path_buf(),
        branch,
    };

    let harness_readme = format!(
        "# GrokRxiv Review-Loop Code Harness\n\n\
         Review ID: `{review_id}`\n\
         Target: `{target}`\n\
         Language: `{language}`\n\
         Required file: `{filename}`\n\
         Branch: `{branch}`\n\n\
         This directory is the full working harness for the code generator, \
         compiler/verifier, Codex reviewer, and fix loop. Work only inside this \
         directory. Do not inspect parent directories or the GrokRxiv repository \
         checkout. Generated code is itself a review artifact and will be \
         committed here by the orchestrator after each round.\n",
        target = task.target_id,
        language = task.language,
        filename = task.filename,
        branch = harness.branch,
    );
    tokio::fs::write(workdir.join("GROKRXIV_HARNESS.md"), harness_readme)
        .await
        .with_context(|| format!("write harness readme {}", workdir.display()))?;
    write_loop_json(&workdir.join("harness_task_input.json"), &base_artifact).await?;
    prepare_review_loop_project_harness(task, workdir).await?;

    let mut setup_reports = Vec::new();
    let init = run_review_loop_git_command(workdir, vec!["init".to_string()]).await;
    ensure_review_loop_git_pass(&init, "git init")?;
    setup_reports.push(command_report_json(&init));

    for (key, value) in [
        ("user.name", "GrokRxiv Review Loop"),
        ("user.email", "review-loop@grokrxiv.local"),
    ] {
        let report = run_review_loop_git_command(
            workdir,
            vec!["config".to_string(), key.to_string(), value.to_string()],
        )
        .await;
        ensure_review_loop_git_pass(&report, &format!("git config {key}"))?;
        setup_reports.push(command_report_json(&report));
    }

    let checkout = run_review_loop_git_command(
        workdir,
        vec![
            "checkout".to_string(),
            "-B".to_string(),
            harness.branch.clone(),
        ],
    )
    .await;
    ensure_review_loop_git_pass(&checkout, "git checkout harness branch")?;
    setup_reports.push(command_report_json(&checkout));

    let baseline_commit =
        commit_review_loop_harness(&harness, "baseline review-loop harness".to_string()).await;
    let report = serde_json::json!({
        "path": harness.path.display().to_string(),
        "branch": harness.branch.clone(),
        "setup": setup_reports,
        "baseline_commit": baseline_commit,
    });
    write_loop_json(&workdir.join("harness.json"), &report).await?;
    Ok(harness)
}

async fn prepare_review_loop_project_harness(
    task: &ReviewFixCodeTask,
    workdir: &Path,
) -> anyhow::Result<()> {
    if task.target_id == "lean" {
        tokio::fs::write(
            workdir.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\n\npackage grokrxiv_review_loop\n\nlean_lib GrokRxiv\n",
        )
        .await
        .with_context(|| format!("write Lean lakefile in {}", workdir.display()))?;
        tokio::fs::write(workdir.join("lean-toolchain"), "leanprover/lean4:v4.30.0\n")
            .await
            .with_context(|| format!("write Lean toolchain in {}", workdir.display()))?;
    }
    Ok(())
}

fn review_loop_harness_branch(review_id: Uuid, target_id: &str) -> String {
    let review_id = review_id.simple().to_string();
    let short = review_id.get(0..12).unwrap_or(&review_id);
    format!("review-loop/{target_id}/{short}")
}

async fn record_review_loop_harness_attempt(
    harness: &ReviewLoopGitHarness,
    target_id: &str,
    attempt: usize,
) -> serde_json::Value {
    let status_before = run_review_loop_git_command(
        &harness.path,
        vec!["status".to_string(), "--short".to_string()],
    )
    .await;
    let commit = commit_review_loop_harness(
        harness,
        format!("review-loop {target_id} attempt {attempt}"),
    )
    .await;
    let status_after = run_review_loop_git_command(
        &harness.path,
        vec!["status".to_string(), "--short".to_string()],
    )
    .await;
    serde_json::json!({
        "branch": harness.branch.clone(),
        "path": harness.path.display().to_string(),
        "status_before": command_report_json(&status_before),
        "commit": commit,
        "status_after": command_report_json(&status_after),
    })
}

async fn commit_review_loop_harness(
    harness: &ReviewLoopGitHarness,
    message: String,
) -> serde_json::Value {
    let add =
        run_review_loop_git_command(&harness.path, vec!["add".to_string(), ".".to_string()]).await;
    let diff_cached = run_review_loop_git_command(
        &harness.path,
        vec![
            "diff".to_string(),
            "--cached".to_string(),
            "--stat".to_string(),
        ],
    )
    .await;
    let mut commit = run_review_loop_git_command(
        &harness.path,
        vec!["commit".to_string(), "-m".to_string(), message],
    )
    .await;
    if commit.status != "pass" {
        let output = format!("{}\n{}", commit.stdout, commit.stderr);
        if output.contains("nothing to commit") || output.contains("no changes added to commit") {
            commit.status = "no_changes".to_string();
        }
    }
    let head = run_review_loop_git_command(
        &harness.path,
        vec![
            "rev-parse".to_string(),
            "--short".to_string(),
            "HEAD".to_string(),
        ],
    )
    .await;
    serde_json::json!({
        "add": command_report_json(&add),
        "cached_diff_stat": command_report_json(&diff_cached),
        "commit": command_report_json(&commit),
        "head": command_report_json(&head),
    })
}

async fn run_review_loop_git_command(workdir: &Path, args: Vec<String>) -> CommandRunReport {
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_loop_command(
        "git",
        &arg_refs,
        workdir,
        std::time::Duration::from_secs(20),
    )
    .await
}

fn ensure_review_loop_git_pass(report: &CommandRunReport, action: &str) -> anyhow::Result<()> {
    if report.status == "pass" {
        return Ok(());
    }
    anyhow::bail!(
        "{action} failed: {}",
        truncate(
            &format!("{} {}", report.stdout.trim(), report.stderr.trim()),
            600
        )
    )
}

fn command_report_json(report: &CommandRunReport) -> serde_json::Value {
    serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({}))
}

async fn run_review_loop_agent(
    state: &super::AppState,
    paper_id: Uuid,
    review_id: Uuid,
    role: &str,
    artifact: serde_json::Value,
    system_prompt: String,
    user_prompt: String,
    source_bundle_path: Option<&Path>,
) -> anyhow::Result<AgentRun> {
    let agent = state
        .agents
        .get(role)
        .ok_or_else(|| anyhow::anyhow!("review-loop agent role `{role}` is not configured"))?;
    let runner_kind = agent.spec().runner;
    let runner = state
        .runners
        .get(&runner_kind)
        .ok_or_else(|| anyhow::anyhow!("review-loop runner `{runner_kind:?}` is not registered"))?;
    let input = AgentInput {
        context: crate::agents::grokrxiv_agent_context(paper_id, review_id),
        role: role.to_string(),
        content_hash_material: artifact.clone(),
        artifact,
        system_prompt,
        user_prompt,
        source_bundle_path: source_bundle_path.map(|path| path.display().to_string()),
    };
    agent.run(runner.as_ref(), input).await
}

fn review_loop_code_system_prompt(task: &ReviewFixCodeTask, role: &str, attempt: usize) -> String {
    format!(
        "You are GrokRxiv role `{role}` in the review-loop code verification path. \
         Target={target} language={language} file={filename} attempt={attempt}. \
         Follow schema.json exactly and return one JSON object only.",
        target = task.target_id,
        language = task.language,
        filename = task.filename,
    )
}

fn review_loop_code_user_prompt(task: &ReviewFixCodeTask, role: &str, attempt: usize) -> String {
    let action = if role == task.reviewer_role {
        "Review the generated code, compiler diagnostics, and paper-derived task evidence."
    } else if attempt == 1 {
        "Generate the complete requested code artifact from the paper-derived review evidence."
    } else {
        "Fix the complete code artifact using the compiler diagnostics and Codex review from the prior round."
    };
    format!(
        "{action}\n\n\
         Required file: {filename}\n\
         Language: {language}\n\
         Compile command: {compile}\n\
         Compile timeout seconds: {compile_timeout_secs}\n\
         Max attempts: {max_attempts}\n\n\
         The canonical task input is in review_input.json. Return strict JSON \
         matching schema.json. Do not include markdown fences or prose outside JSON.",
        filename = task.filename,
        language = task.language,
        compile = std::iter::once(task.compile_program)
            .chain(task.compile_args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        compile_timeout_secs = task.compile_timeout_secs,
        max_attempts = task.max_attempts,
    )
}

fn code_from_agent_run(run: &AgentRun, task: &ReviewFixCodeTask) -> anyhow::Result<String> {
    let language = run
        .output
        .get("language")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if language != task.language {
        anyhow::bail!(
            "role `{}` returned language `{language}`, expected `{}`",
            run.role,
            task.language
        );
    }
    let filename = run
        .output
        .get("filename")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if !filename.ends_with(task.filename) && filename != task.filename {
        anyhow::bail!(
            "role `{}` returned filename `{filename}`, expected `{}`",
            run.role,
            task.filename
        );
    }
    let code = run
        .output
        .get("code")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if code.trim().is_empty() {
        anyhow::bail!("role `{}` returned empty code", run.role);
    }
    Ok(code)
}

async fn write_review_loop_code_file(path: &Path, code: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, code).await?;
    Ok(())
}

fn forbidden_terms_in_code(code: &str, terms: &[&'static str]) -> Vec<&'static str> {
    terms
        .iter()
        .copied()
        .filter(|term| code.contains(term))
        .collect()
}

fn code_review_passed(review: &serde_json::Value) -> bool {
    if review.get("status").and_then(|value| value.as_str()) != Some("pass") {
        return false;
    }
    !review
        .get("issues")
        .and_then(|value| value.as_array())
        .map(|issues| {
            issues.iter().any(|issue| {
                issue.get("severity").and_then(|value| value.as_str()) == Some("blocking")
            })
        })
        .unwrap_or(false)
}

fn review_fix_code_node_id(target_id: &str) -> &'static str {
    match target_id {
        "haskell" => "haskell_review_fix_code",
        "lean" => "lean_review_fix_code",
        "pr" => "pr_fixer",
        _ => "review_fix_code",
    }
}

fn review_loop_agent_output_schema(role: &str) -> (&'static str, serde_json::Value) {
    if role.ends_with("_reviewer") || role.contains("code_reviewer") {
        (
            "schemas/review_loop_code_review.schema.json",
            serde_json::from_str(include_str!(
                "../../../schemas/review_loop_code_review.schema.json"
            ))
            .expect("review-loop code review schema is valid JSON"),
        )
    } else {
        (
            "schemas/review_loop_code_artifact.schema.json",
            serde_json::from_str(include_str!(
                "../../../schemas/review_loop_code_artifact.schema.json"
            ))
            .expect("review-loop code artifact schema is valid JSON"),
        )
    }
}

async fn write_review_loop_agent_output_audit(
    artifact_root: &Path,
    task: &ReviewFixCodeTask,
    attempt: usize,
    role: &str,
    phase: &str,
    input: &serde_json::Value,
    run: Option<&AgentRun>,
    semantic_validation: Option<&serde_json::Value>,
    tool_validation: Option<&serde_json::Value>,
    decision_status: &str,
    decision_reason: &str,
) -> anyhow::Result<serde_json::Value> {
    let node = review_fix_code_node_id(task.target_id);
    let rel_dir = format!("agent_outputs/{node}/round_{attempt}/{role}");
    let dir = artifact_root.join(&rel_dir);
    tokio::fs::create_dir_all(&dir).await?;

    let (schema_path, schema_json) = review_loop_agent_output_schema(role);
    let output = run
        .map(|run| run.output.clone())
        .unwrap_or(serde_json::Value::Null);
    let raw_stdout = run
        .and_then(|run| run.raw_output.as_deref())
        .unwrap_or_default();
    let raw_stderr = if run.is_some() { "" } else { decision_reason };
    let schema_validation = serde_json::json!({
        "status": if run.is_some() { "pass" } else { "fail" },
        "schema_path": schema_path,
        "reason": if run.is_some() {
            "runner returned JSON already validated against role output schema"
        } else {
            decision_reason
        }
    });
    let semantic_validation = semantic_validation
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"status": "not_applicable"}));
    let tool_validation = tool_validation
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"status": "not_applicable"}));
    let decision = serde_json::json!({
        "status": decision_status,
        "reason": decision_reason,
    });

    write_loop_json(&dir.join("input.json"), input).await?;
    write_loop_json(&dir.join("schema.json"), &schema_json).await?;
    tokio::fs::write(dir.join("raw_stdout.txt"), raw_stdout).await?;
    tokio::fs::write(dir.join("raw_stderr.txt"), raw_stderr).await?;
    write_loop_json(&dir.join("output.json"), &output).await?;
    write_loop_json(&dir.join("schema_validation.json"), &schema_validation).await?;
    write_loop_json(&dir.join("semantic_validation.json"), &semantic_validation).await?;
    write_loop_json(&dir.join("tool_validation.json"), &tool_validation).await?;
    write_loop_json(&dir.join("decision.json"), &decision).await?;

    Ok(serde_json::json!({
        "role": role,
        "phase": phase,
        "attempt": attempt,
        "runner": run.map(|run| format!("{:?}", run.runner).to_ascii_lowercase()),
        "model": run.map(|run| run.model.clone()),
        "tokens_in": run.and_then(|run| run.tokens_in),
        "tokens_out": run.and_then(|run| run.tokens_out),
        "latency_ms": run.map(|run| run.latency_ms),
        "schema_path": schema_path,
        "artifact_dir": format!("review_loop/{rel_dir}"),
        "artifacts": {
            "input": format!("review_loop/{rel_dir}/input.json"),
            "schema": format!("review_loop/{rel_dir}/schema.json"),
            "raw_stdout": format!("review_loop/{rel_dir}/raw_stdout.txt"),
            "raw_stderr": format!("review_loop/{rel_dir}/raw_stderr.txt"),
            "output": format!("review_loop/{rel_dir}/output.json"),
            "schema_validation": format!("review_loop/{rel_dir}/schema_validation.json"),
            "semantic_validation": format!("review_loop/{rel_dir}/semantic_validation.json"),
            "tool_validation": format!("review_loop/{rel_dir}/tool_validation.json"),
            "decision": format!("review_loop/{rel_dir}/decision.json")
        },
        "schema_validation": schema_validation,
        "semantic_validation": semantic_validation,
        "tool_validation": tool_validation,
        "decision": decision,
    }))
}

fn review_fix_loop_agent_output_audit_summary(results: &serde_json::Value) -> serde_json::Value {
    let mut total = 0_i64;
    let mut accepted = 0_i64;
    let mut rejected = 0_i64;
    let mut by_role = serde_json::Map::new();

    for audit in results
        .get("attempts")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .flat_map(|attempt| {
            attempt
                .get("agent_output_audits")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
        })
    {
        total += 1;
        let role = audit
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let status = audit
            .get("decision")
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            .unwrap_or("rejected");
        let role_entry = by_role.entry(role.to_string()).or_insert_with(|| {
            serde_json::json!({
                "total": 0,
                "accepted": 0,
                "rejected": 0,
            })
        });
        if let Some(entry) = role_entry.as_object_mut() {
            let current_total = entry
                .get("total")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            entry.insert("total".to_string(), serde_json::json!(current_total + 1));
            if status == "accepted" {
                let current = entry
                    .get("accepted")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                entry.insert("accepted".to_string(), serde_json::json!(current + 1));
            } else {
                let current = entry
                    .get("rejected")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0);
                entry.insert("rejected".to_string(), serde_json::json!(current + 1));
            }
        }
        if status == "accepted" {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }

    serde_json::json!({
        "total": total,
        "accepted": accepted,
        "rejected": rejected,
        "by_role": by_role,
    })
}

fn review_fix_attempt_rejection_reason(
    semantic_validation: &serde_json::Value,
    compile_run: &CommandRunReport,
    review_output: &serde_json::Value,
) -> String {
    if let Some(issue) = semantic_validation
        .get("issues")
        .and_then(|value| value.as_array())
        .and_then(|issues| issues.first())
        .and_then(|issue| issue.as_str())
    {
        return truncate(issue, 260);
    }
    if compile_run.status != "pass" {
        let detail = if compile_run.stderr.trim().is_empty() {
            compile_run.stdout.as_str()
        } else {
            compile_run.stderr.as_str()
        };
        return truncate(&detail.replace('\n', " "), 260);
    }
    if let Some(issue) = review_output
        .get("issues")
        .and_then(|value| value.as_array())
        .and_then(|issues| issues.first())
        .and_then(|issue| issue.get("message"))
        .and_then(|message| message.as_str())
    {
        return truncate(issue, 260);
    }
    "generated artifact did not pass all deterministic and reviewer gates".to_string()
}

fn review_fix_loop_summary(results: &serde_json::Value) -> String {
    let attempts = results
        .get("attempts")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if results.get("status").and_then(|value| value.as_str()) == Some("pass") {
        return format!("attempts={}", attempts.len());
    }
    if let Some(verdict) = results.get("verdict").and_then(|value| value.as_str()) {
        let status = results
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("fail");
        let proof_status = results
            .get("proof_status")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let reason = results
            .get("skip_reason")
            .and_then(|value| value.as_str())
            .unwrap_or("review-fix-code loop did not prove the target");
        return truncate(
            &format!(
                "status={status} verdict={verdict} proof_status={proof_status} reason={reason}"
            ),
            260,
        );
    }
    for attempt in attempts.iter().rev() {
        if let Some(error) = attempt.get("author_error").and_then(|value| value.as_str()) {
            return truncate(error, 260);
        }
        if let Some(error) = attempt.get("write_error").and_then(|value| value.as_str()) {
            return truncate(error, 260);
        }
        if let Some(issue) = attempt
            .get("semantic_validation")
            .and_then(|validation| validation.get("issues"))
            .and_then(|issues| issues.as_array())
            .and_then(|issues| issues.first())
            .and_then(|issue| issue.as_str())
        {
            return truncate(issue, 260);
        }
        if let Some(issue) = attempt
            .get("codex_review")
            .and_then(|review| review.get("issues"))
            .and_then(|issues| issues.as_array())
            .and_then(|issues| issues.first())
            .and_then(|issue| issue.get("message"))
            .and_then(|message| message.as_str())
        {
            return truncate(issue, 260);
        }
        if let Some(stderr) = attempt
            .get("compile")
            .and_then(|compile| compile.get("stderr"))
            .and_then(|stderr| stderr.as_str())
            .filter(|stderr| !stderr.trim().is_empty())
        {
            return truncate(&stderr.replace('\n', " "), 260);
        }
    }
    "review-fix-code loop failed".to_string()
}

async fn run_review_loop_for_review(
    state: &super::AppState,
    review_id: Uuid,
    debug_output: bool,
) -> anyhow::Result<ReviewLoopOutcome> {
    use grokrxiv_schemas::VerifierStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("review-loop: DATABASE_URL not configured"))?;
    let stages = review_loop_stage_plan()?;
    let artifact_dir = crate::artifacts::review_artifact_dir(review_id).join("review_loop");
    tokio::fs::create_dir_all(&artifact_dir).await?;
    let mut bundle_skip_reasons = BTreeMap::new();
    let paper_id = paper_id_for_review(pool, review_id)
        .await
        .context("review-loop: load paper_id for review")?;

    crate::cli_status::emit_stage(
        6,
        6,
        "Review loop",
        cli_status::StatusMark::Run,
        "semantic/proof/citation/fix policy",
    );
    if debug_output {
        cli_status::emit_detail(
            "review_loop",
            cli_status::StatusMark::Run,
            &format!("artifact_dir={}", artifact_dir.display()),
        );
    }

    let agent_rows: Vec<(
        String,
        String,
        Option<String>,
        serde_json::Value,
        Option<serde_json::Value>,
    )> = sqlx::query_as(
        "select role, coalesce(dag_type, 'paper-review'), verifier_status, output, verifier_notes \
         from review_agents \
         where review_id = $1 and dag_type = 'paper-review' \
         order by role, created_at desc",
    )
    .bind(review_id)
    .fetch_all(pool)
    .await?;

    let claims = extract_review_loop_claims(&agent_rows);
    let claims_value = serde_json::json!({
        "review_id": review_id,
        "claims": claims,
    });
    write_loop_json(&artifact_dir.join("claims.json"), &claims_value).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "claim_extractor",
        claims_value.clone(),
        VerifierStatus::Pass,
        serde_json::json!({"artifact_path": "review_loop/claims.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "claim_extractor",
        true,
        "review_loop/claims.json",
        debug_output,
        &format!(
            "claims={}",
            claims_value["claims"].as_array().map(Vec::len).unwrap_or(0)
        ),
    );

    let paper_math_sources = load_review_loop_paper_math_sources(pool, paper_id).await?;
    write_loop_json(
        &artifact_dir.join("paper_math_sources.json"),
        &paper_math_sources,
    )
    .await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "paper_math_source_collector",
        paper_math_sources.clone(),
        VerifierStatus::Pass,
        serde_json::json!({"artifact_path": "review_loop/paper_math_sources.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "paper_math_source_collector",
        true,
        "review_loop/paper_math_sources.json",
        debug_output,
        &format!(
            "theorem_nodes={} equations={} sources={} warnings={}",
            paper_math_sources["theorem_graph"]["nodes"]
                .as_array()
                .map(Vec::len)
                .or_else(|| {
                    paper_math_sources["theorem_graph"]["theorem_graph"]
                        .as_array()
                        .map(Vec::len)
                })
                .unwrap_or(0),
            paper_math_sources["equations"]["equations"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0),
            paper_math_sources["artifact_sources"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0),
            paper_math_sources["warnings"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
        ),
    );

    let knowledge_graph = build_review_loop_knowledge_graph(&claims_value);
    write_loop_json(&artifact_dir.join("knowledge_graph.json"), &knowledge_graph).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "knowledge_graph_builder",
        knowledge_graph.clone(),
        VerifierStatus::Pass,
        serde_json::json!({"artifact_path": "review_loop/knowledge_graph.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "knowledge_graph_builder",
        true,
        "review_loop/knowledge_graph.json",
        debug_output,
        &format!(
            "nodes={} edges={}",
            knowledge_graph["nodes"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0),
            knowledge_graph["edges"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
        ),
    );

    let haskell_dir = artifact_dir.join("haskell");
    tokio::fs::create_dir_all(&haskell_dir).await?;
    let semantic_ir = grokrxiv_review_loop::build_semantic_ir_from_paper_math(
        review_id,
        &paper_math_sources,
        &claims_value,
        &knowledge_graph,
    );
    write_loop_json(&artifact_dir.join("semantic_ir.json"), &semantic_ir).await?;
    let theorem_candidate_count = semantic_ir["theorem_candidates"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let definition_count = semantic_ir["definitions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let assumption_count = semantic_ir["assumptions"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let semantic_model = serde_json::json!({
        "schema_version": "1.0.0",
        "review_id": review_id,
        "semantic_ir": "review_loop/semantic_ir.json",
        "paper_math_sources": "review_loop/paper_math_sources.json",
        "theorem_candidate_count": theorem_candidate_count,
        "definition_count": definition_count,
        "assumption_count": assumption_count,
        "haskell_module": "review_loop/haskell/SemanticModel.hs",
        "author_role": "haskell_semantic_author",
        "reviewer_role": "haskell_code_reviewer",
        "fixer_role": "haskell_code_fixer",
    });
    write_loop_json(&artifact_dir.join("semantic_model.json"), &semantic_model).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "semantic_category_mapper",
        semantic_model.clone(),
        VerifierStatus::Pass,
        serde_json::json!({
            "artifacts": [
                "review_loop/paper_math_sources.json",
                "review_loop/semantic_ir.json",
                "review_loop/semantic_model.json",
                "review_loop/haskell/SemanticModel.hs"
            ]
        }),
    )
    .await?;
    emit_review_loop_node_debug(
        "semantic_category_mapper",
        true,
        "review_loop/semantic_model.json",
        debug_output,
        &format!("theorem_candidates={theorem_candidate_count} definitions={definition_count} assumptions={assumption_count}"),
    );

    let haskell_results = run_review_fix_code_loop(
        state,
        paper_id,
        review_id,
        ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        },
        serde_json::json!({
            "review_id": review_id,
            "task": "Generate a typed Haskell mathematical transcription IR model from GrokRxiv paper-derived evidence.",
            "requirements": [
                "Create module SemanticModel.",
                "Use only pure Haskell definitions.",
                "Treat review_loop/semantic_ir.json as the canonical typed mathematical IR contract.",
                "The Haskell file is a checked consumer/round-trip artifact derived from canonical IR JSON; do not invent theorem statements outside that IR.",
                "Define SourceSpan, MathType, Term, Proposition, Binder, Definition, Assumption, TheoremIR, ClaimIR, ProofObligation, and LeanTarget types.",
                "Represent paper-derived formal mathematical statements, assumptions, definitions, source spans, and Lean declaration targets from semantic_ir theorem_candidates/definitions/assumptions; use paper_math_sources only as provenance/count context in this compact code-author payload.",
                "If semantic_ir.theorem_candidates is empty, emit empty theoremTargets, claims, and proof obligations while preserving semantic_ir.limitations; do not backfill from claims or knowledge_graph summaries.",
                "Treat semantic categories as annotations over typed mathematical transcription, not as replacements for the math.",
                "Do not turn summary, novelty, citation, reviewer recommendation, or publisher-readiness claims into Lean obligations.",
                "Do not model this as review roles, category counts, claimCount, or publisherReadyLowerBound.",
                "Include categoryToObligations, claimToObligations, and obligationToLean mapping functions.",
                "The file must compile with ghc -fno-code SemanticModel.hs."
            ],
            "claims": claims_value,
            "paper_math_sources": paper_math_sources,
            "knowledge_graph": knowledge_graph,
            "semantic_ir": semantic_ir,
            "semantic_model": semantic_model,
        }),
        &haskell_dir,
        &haskell_dir.join("SemanticModel.hs"),
        debug_output,
    )
    .await;
    let haskell_pass = haskell_results["status"] == "pass";
    write_loop_json(&haskell_dir.join("results.json"), &haskell_results).await?;
    write_loop_json(&haskell_dir.join("fix_rounds.json"), &haskell_results).await?;
    if !haskell_dir.join("SemanticModel.hs").is_file() {
        bundle_skip_reasons.insert(
            "review_loop/haskell/SemanticModel.hs".to_string(),
            review_fix_loop_summary(&haskell_results),
        );
    }
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "haskell_review_fix_code",
        haskell_results.clone(),
        if haskell_pass {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/haskell/results.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "haskell_review_fix_code",
        haskell_pass,
        "review_loop/haskell/results.json",
        debug_output,
        &review_fix_loop_summary(&haskell_results),
    );

    let proof_obligations =
        grokrxiv_review_loop::build_proof_obligations(review_id, &semantic_ir, &haskell_results);
    write_loop_json(
        &artifact_dir.join("proof_obligations.json"),
        &proof_obligations,
    )
    .await?;
    let lean_targets = grokrxiv_review_loop::build_lean_targets(&proof_obligations);
    write_loop_json(&artifact_dir.join("lean_targets.json"), &lean_targets).await?;
    let proof_obligations_ready =
        grokrxiv_review_loop::proof_obligations_require_lean(&proof_obligations);
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "proof_obligation_generator",
        proof_obligations.clone(),
        if proof_obligations_ready {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/proof_obligations.json"}),
    )
    .await?;
    let proof_obligation_debug = if proof_obligations_ready {
        format!(
            "theorem_obligations={}",
            proof_obligations["obligations"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
        )
    } else {
        proof_obligations["obligations"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("statement"))
            .and_then(|value| value.as_str())
            .unwrap_or("formalization blocked")
            .to_string()
    };
    emit_review_loop_node_debug(
        "proof_obligation_generator",
        proof_obligations_ready,
        "review_loop/proof_obligations.json",
        debug_output,
        &proof_obligation_debug,
    );

    let lean_dir = artifact_dir.join("lean");
    let lean_src_dir = lean_dir.join("GrokRxiv");
    tokio::fs::create_dir_all(&lean_src_dir).await?;
    let lean_task = ReviewFixCodeTask {
        target_id: "lean",
        language: "lean",
        filename: "GrokRxiv/Proofs.lean",
        author_role: "lean_proof_author",
        reviewer_role: "lean_code_reviewer",
        fixer_role: "lean_code_fixer",
        compile_program: "lake",
        compile_args: vec![
            "env".to_string(),
            "lean".to_string(),
            "GrokRxiv/Proofs.lean".to_string(),
        ],
        compile_timeout_secs: 1800,
        forbidden_terms: vec!["sorry", "admit", "axiom"],
        max_attempts: 2,
    };
    let lean_final_path = lean_src_dir.join("Proofs.lean");
    let lean_results = if proof_obligations_ready {
        run_review_fix_code_loop(
            state,
            paper_id,
            review_id,
            lean_task.clone(),
            serde_json::json!({
                "review_id": review_id,
                "task": "Complete proofs for GrokRxiv mathematical Lean targets deterministically emitted from the typed IR.",
                "requirements": [
                    "Write the complete file GrokRxiv/Proofs.lean.",
                    "Use the lean_skeleton strings in review_loop/lean_targets.json as the theorem statement source of truth.",
                    "You may replace proof bodies only; do not alter theorem names, binders, assumptions, or conclusions.",
                    "Do not use sorry, admit, or axiom.",
                    "The file must verify with lake env lean GrokRxiv/Proofs.lean.",
                    "For every theorem_formalization obligation, declare the exact lean_declaration name.",
                    "Do not prove claim counts, review statuses, semantic labels, or metadata in place of the mathematical theorem target.",
                    "If a theorem cannot be formalized honestly, emit code that fails review rather than masking the gap."
                ],
                "proof_obligations": proof_obligations,
                "lean_targets": lean_targets,
                "semantic_ir": semantic_ir,
                "semantic_model": semantic_model,
                "haskell_results": haskell_results,
            }),
            &lean_dir,
            &lean_final_path,
            debug_output,
        )
        .await
    } else {
        let reason = proof_obligations["obligations"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("statement"))
            .and_then(|value| value.as_str())
            .unwrap_or(
                "Lean formalization skipped because upstream semantic obligations are blocked.",
            );
        let _ = write_review_loop_code_file(
            &lean_final_path,
            &format!("/- Lean formalization skipped: {reason} -/\n"),
        )
        .await;
        skipped_review_fix_code_results(&lean_task, &lean_final_path, reason)
    };
    let lean_results = annotate_lean_review_fix_code_results(lean_results, &proof_obligations);
    let lean_pass = lean_results["status"] == "pass";
    write_loop_json(&lean_dir.join("results.json"), &lean_results).await?;
    write_loop_json(&lean_dir.join("fix_rounds.json"), &lean_results).await?;
    if !lean_final_path.is_file() {
        bundle_skip_reasons.insert(
            "review_loop/lean/GrokRxiv/Proofs.lean".to_string(),
            review_fix_loop_summary(&lean_results),
        );
    }
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "lean_review_fix_code",
        lean_results.clone(),
        if lean_pass {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/lean/results.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "lean_review_fix_code",
        lean_pass,
        "review_loop/lean/results.json",
        debug_output,
        &review_fix_loop_summary(&lean_results),
    );
    let theorem_map = grokrxiv_review_loop::build_theorem_map(&proof_obligations, &lean_results);
    write_loop_json(&lean_dir.join("theorem_map.json"), &theorem_map).await?;
    write_loop_json(&lean_dir.join("verification_report.json"), &theorem_map).await?;
    let semantic_adequacy =
        grokrxiv_review_loop::build_semantic_adequacy(&semantic_ir, &theorem_map);
    write_loop_json(
        &artifact_dir.join("semantic_adequacy.json"),
        &semantic_adequacy,
    )
    .await?;
    let semantic_adequacy_pass = semantic_adequacy["status"] == "pass";
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "semantic_adequacy_checker",
        semantic_adequacy.clone(),
        if semantic_adequacy_pass {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/semantic_adequacy.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "semantic_adequacy_checker",
        semantic_adequacy_pass,
        "review_loop/semantic_adequacy.json",
        debug_output,
        semantic_adequacy["verdicts"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("verdict"))
            .and_then(|value| value.as_str())
            .unwrap_or("no theorem adequacy verdicts"),
    );

    let corpus_context = load_review_loop_corpus_context(pool, paper_id).await?;
    if let Some(corpus_context) = corpus_context.as_ref() {
        if let Some(dossier) = review_loop_n5_false_proof_halt(corpus_context, &theorem_map) {
            return halt_review_loop_for_n5(
                pool,
                review_id,
                &stages,
                &artifact_dir,
                dossier,
                theorem_map,
                semantic_adequacy,
                lean_results,
                debug_output,
            )
            .await;
        }
    }

    let citation_summary = citation_verifier_summary(pool, review_id).await;
    let citation_report = serde_json::json!({
        "stage": "citation_validation",
        "dag_type": "citation-validation",
        "source": "paper-review citation verifier evidence plus declared review-loop DAG call",
        "status": citation_summary
            .as_ref()
            .and_then(|s| s.verifier_status.as_deref())
            .unwrap_or("fail"),
        "checked": citation_summary.as_ref().map(|s| s.checked).unwrap_or(0),
        "unresolved": citation_summary.as_ref().map(|s| s.unresolved).unwrap_or(0),
        "unverified": citation_summary.as_ref().map(|s| s.unverified).unwrap_or(0),
        "transient_unknown": citation_summary.as_ref().map(|s| s.unknown).unwrap_or(0),
        "malformed": citation_summary.as_ref().map(|s| s.malformed).unwrap_or(0),
        "unresolved_fraction": citation_summary
            .as_ref()
            .map(|s| s.unresolved_fraction)
            .unwrap_or(1.0),
        "evidence": citation_summary
            .as_ref()
            .map(|s| serde_json::to_value(&s.evidence).unwrap_or_else(|_| serde_json::json!([])))
            .unwrap_or_else(|| serde_json::json!([])),
        "artifact_hint": citation_summary
            .as_ref()
            .map(|s| s.artifact_hint.clone())
            .unwrap_or_else(|| "paper-review citation verifier evidence missing".to_string()),
    });
    write_loop_json(
        &artifact_dir.join("citation_validation_report.json"),
        &citation_report,
    )
    .await?;
    let citation_adjudication = serde_json::json!({
        "stage": "citation_validation_adjudication",
        "status": "skipped",
        "skip_reason": "Review-loop citation adjudication DAG output is not wired yet; using paper-review citation verifier evidence.",
        "source_artifact": "review_loop/citation_validation_report.json",
    });
    write_loop_json(
        &artifact_dir.join("citation_validation_adjudication.json"),
        &citation_adjudication,
    )
    .await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "citation_validation",
        citation_report.clone(),
        if citation_report["status"] == "pass" || citation_report["status"] == "warn" {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/citation_validation_report.json"}),
    )
    .await?;
    let citation_pass = citation_report["status"] == "pass" || citation_report["status"] == "warn";
    emit_review_loop_node_debug(
        "citation_validation",
        citation_pass,
        "review_loop/citation_validation_report.json",
        debug_output,
        &format!(
            "status={} unresolved={} unknown={}",
            citation_report["status"].as_str().unwrap_or("unknown"),
            citation_report["unresolved"].as_u64().unwrap_or(0),
            citation_report["transient_unknown"].as_u64().unwrap_or(0)
        ),
    );

    let pr_fixes =
        run_review_loop_pr_fixer(state, paper_id, review_id, &artifact_dir, debug_output).await?;
    let pr_fixer_pass = pr_fixes
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|status| status == "pass");
    let pr_skip_reason = pr_fixes
        .get("issues")
        .and_then(|value| value.as_array())
        .and_then(|issues| issues.first())
        .and_then(|issue| issue.as_str())
        .unwrap_or("PR fixer did not produce the declared artifact")
        .to_string();
    if !artifact_dir.join("fixed/review.tex").is_file() {
        bundle_skip_reasons.insert(
            "review_loop/fixed/review.tex".to_string(),
            pr_skip_reason.clone(),
        );
    }
    if !artifact_dir.join("fixed/review.pdf").is_file() {
        bundle_skip_reasons.insert("review_loop/fixed/review.pdf".to_string(), pr_skip_reason);
    }
    for (output, reason) in review_loop_bundle_skip_reasons(&citation_report, &pr_fixes) {
        bundle_skip_reasons.entry(output).or_insert(reason);
    }
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "pr_fixer",
        pr_fixes.clone(),
        if pr_fixer_pass {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/pr_fixes.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "pr_fixer",
        pr_fixer_pass,
        "review_loop/pr_fixes.json",
        debug_output,
        pr_fixes
            .get("issues")
            .and_then(|value| value.as_array())
            .and_then(|issues| issues.first())
            .and_then(|issue| issue.as_str())
            .unwrap_or("fixed artifacts checked"),
    );

    let pr_review_results = serde_json::json!({
        "stage": "pr_review_fix_code",
        "max_attempts": 2,
        "status": if pr_fixer_pass { "pass" } else { "fail" },
        "attempts": pr_fixes
            .get("compile_review_loop")
            .and_then(|value| value.get("attempts"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
        "agent_output_audit_summary": review_fix_loop_agent_output_audit_summary(
            pr_fixes
                .get("compile_review_loop")
                .unwrap_or(&serde_json::Value::Null),
        ),
        "reviewer_role": "pr_artifact_reviewer",
    });
    let pr_review_dir = artifact_dir.join("pr_review");
    tokio::fs::create_dir_all(&pr_review_dir).await?;
    write_loop_json(&pr_review_dir.join("results.json"), &pr_review_results).await?;
    write_loop_json(&pr_review_dir.join("fix_rounds.json"), &pr_review_results).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "pr_review_fix_code",
        pr_review_results.clone(),
        if pr_fixer_pass {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/pr_review/results.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "pr_review_fix_code",
        pr_fixer_pass,
        "review_loop/pr_review/results.json",
        debug_output,
        if pr_fixer_pass {
            "formatting review passed"
        } else {
            "formatting review failed"
        },
    );

    let pre_policy_stages = stages
        .iter()
        .filter(|stage| {
            !matches!(
                stage.id.as_str(),
                "policy_gate" | "review_loop_report" | "publish_decision"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let bundle_completeness = review_loop_bundle_completeness_report(
        &pre_policy_stages,
        &artifact_dir,
        &bundle_skip_reasons,
    )
    .await?;
    write_loop_json(
        &artifact_dir.join("bundle_completeness.json"),
        &bundle_completeness,
    )
    .await?;
    let bundle_completeness_pass = bundle_completeness["status"] == "pass";

    let (_, publication_gate, _) = load_publication_gate_context(pool, review_id).await?;
    let publication_policy =
        review_loop_publication_gate_policy(corpus_context.as_ref(), &publication_gate);
    let mut blocking_issues = Vec::new();
    if let Some(issue) = publication_policy.blocking_issue.clone() {
        blocking_issues.push(issue);
    }
    if !bundle_completeness_pass {
        let issue = bundle_completeness["failures"]
            .as_array()
            .and_then(|failures| failures.first())
            .and_then(|failure| failure.as_str())
            .unwrap_or("Review-loop bundle is missing declared artifacts.");
        blocking_issues.push(format!("Review-loop bundle completeness failed: {issue}"));
    }
    if !haskell_pass {
        blocking_issues.push("Haskell semantic model did not compile cleanly.".to_string());
    }
    if theorem_candidate_count == 0 {
        blocking_issues.push(
            "Semantic IR did not extract theorem candidates for Lean formalization.".to_string(),
        );
    }
    if !lean_pass {
        blocking_issues.push("Lean proof obligations did not verify cleanly.".to_string());
    }
    if !semantic_adequacy_pass {
        blocking_issues.push(
            "Semantic adequacy check found unproved or overclaimed theorem statements.".to_string(),
        );
    }
    if citation_report["status"] == "fail" {
        blocking_issues
            .push("Citation-validation evidence failed deterministic policy.".to_string());
    }
    if !pr_fixer_pass {
        blocking_issues
            .push("PR fixer did not produce compile-reviewed corrected artifacts.".to_string());
    }
    let integrity_ready = publication_policy.integrity_ready && blocking_issues.is_empty();
    let publisher_ready = publication_policy.publisher_ready && integrity_ready;
    let deterministic_status = if integrity_ready { "pass" } else { "fail" }.to_string();
    let policy_gate = serde_json::json!({
        "deterministic_status": deterministic_status,
        "integrity_ready": integrity_ready,
        "publisher_ready": publisher_ready,
        "recommendation_policy": {
            "status": publication_policy.status,
            "expected_recommendation": corpus_context
                .as_ref()
                .and_then(|context| context.expected_recommendation.as_deref()),
            "actual_recommendation": publication_gate.recommendation,
            "publisher_ready": publication_policy.publisher_ready,
            "integrity_ready": publication_policy.integrity_ready,
        },
        "score_thresholds": {
            "haskell_compile": "pass",
            "lean_theorem_formalization": "proved",
            "semantic_adequacy": "all_theorem_claims_match_proved_lean_declarations",
            "citation_validation": "pass_or_warn",
            "pr_artifacts": "fixed_tex_and_pdf_present",
            "artifact_bundle": "all_declared_outputs_present_or_explicitly_skipped",
            "recommendation_policy": "publisher_ready_accept_or_corpus_expected_honest"
        },
        "blocking_issues": blocking_issues,
        "component_status": {
            "publication_gate": format!("{:?}", publication_gate.verdict).to_ascii_lowercase(),
            "recommendation_policy": publication_policy.status,
            "bundle_completeness": bundle_completeness["status"],
            "haskell": if haskell_pass { "pass" } else { "fail" },
            "lean": if lean_pass { "pass" } else { "fail" },
            "semantic_adequacy": semantic_adequacy["status"],
            "citation_validation": citation_report["status"],
            "pr_fixer": pr_fixes["status"],
        },
        "publishability_vector": {
            "formal": if lean_pass { "proved" } else { "failed" },
            "semantic_adequacy": semantic_adequacy["status"],
            "citation": citation_report["status"],
            "reproducibility": "not_run",
            "integrity": if haskell_pass { "pass" } else { "fail" },
            "safety": if pr_fixer_pass { "pass" } else { "fail" },
        },
        "release_tier": {
            "tier": if publisher_ready { "formally_verified" } else { "in_review" },
            "lifecycle_state": if publisher_ready { "published" } else { "needs_update" },
        }
    });
    write_loop_json(&artifact_dir.join("policy_gate.json"), &policy_gate).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "policy_gate",
        policy_gate.clone(),
        if integrity_ready {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/policy_gate.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "policy_gate",
        integrity_ready,
        "review_loop/policy_gate.json",
        debug_output,
        blocking_issues
            .first()
            .map(String::as_str)
            .unwrap_or("deterministic policy passed"),
    );

    let report = serde_json::json!({
        "review_id": review_id,
        "dag_type": "review-loop",
        "deterministic_status": deterministic_status,
        "publisher_ready": publisher_ready,
        "blocking_issues": policy_gate["blocking_issues"],
        "fix_attempts": {
            "haskell": haskell_results["attempts"],
            "lean": lean_results["attempts"],
            "pr_review": pr_review_results["attempts"],
        },
        "artifact_paths": {
            "claims": "review_loop/claims.json",
            "paper_math_sources": "review_loop/paper_math_sources.json",
            "knowledge_graph": "review_loop/knowledge_graph.json",
            "semantic_ir": "review_loop/semantic_ir.json",
            "semantic_model": "review_loop/semantic_model.json",
            "haskell": "review_loop/haskell/results.json",
            "haskell_harness": "review_loop/haskell/harness.json",
            "lean": "review_loop/lean/results.json",
            "lean_targets": "review_loop/lean_targets.json",
            "lean_harness": "review_loop/lean/harness.json",
            "lean_theorem_map": "review_loop/lean/theorem_map.json",
            "lean_verification_report": "review_loop/lean/verification_report.json",
            "semantic_adequacy": "review_loop/semantic_adequacy.json",
            "proof_obligations": "review_loop/proof_obligations.json",
            "citation_validation": "review_loop/citation_validation_report.json",
            "citation_adjudication": "review_loop/citation_validation_adjudication.json",
            "pr_fixes": "review_loop/pr_fixes.json",
            "pr_harness": "review_loop/fixed/harness.json",
            "agent_outputs": "review_loop/agent_outputs",
            "policy_gate": "review_loop/policy_gate.json",
            "bundle_completeness": "review_loop/bundle_completeness.json",
        },
        "agent_output_audits": {
            "haskell": haskell_results["agent_output_audit_summary"],
            "lean": lean_results["agent_output_audit_summary"],
            "pr": pr_fixes
                .get("compile_review_loop")
                .and_then(|value| value.get("agent_output_audit_summary"))
                .cloned()
                .unwrap_or_else(|| review_fix_loop_agent_output_audit_summary(&serde_json::Value::Null)),
            "pr_review": pr_review_results["agent_output_audit_summary"],
        },
        "theorem_formalization": theorem_map,
        "semantic_adequacy": semantic_adequacy,
        "bundle_completeness": bundle_completeness,
        "publishability_vector": policy_gate["publishability_vector"],
        "release_tier": policy_gate["release_tier"],
        "pr_evidence": pr_fixes,
        "publish_decision": {
            "publisher_ready": publisher_ready,
            "action": if publisher_ready { "publication_pr" } else { "revision_needed_pr" },
            "auto_publish": publisher_ready,
        }
    });
    write_loop_json(&artifact_dir.join("review_loop_report.json"), &report).await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "review_loop_report",
        report.clone(),
        if integrity_ready {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({
            "artifact_path": "review_loop/review_loop_report.json",
            "bundle_completeness": "review_loop/bundle_completeness.json"
        }),
    )
    .await?;
    emit_review_loop_node_debug(
        "review_loop_report",
        integrity_ready,
        "review_loop/review_loop_report.json",
        debug_output,
        if bundle_completeness_pass {
            "final loop report persisted"
        } else {
            "declared artifact bundle has missing outputs without skip_reason"
        },
    );

    let publish_decision = report["publish_decision"].clone();
    write_loop_json(
        &artifact_dir.join("publish_decision.json"),
        &publish_decision,
    )
    .await?;
    record_review_loop_node(
        pool,
        review_id,
        &stages,
        "publish_decision",
        publish_decision,
        if integrity_ready {
            VerifierStatus::Pass
        } else {
            VerifierStatus::Fail
        },
        serde_json::json!({"artifact_path": "review_loop/publish_decision.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "publish_decision",
        integrity_ready,
        "review_loop/publish_decision.json",
        debug_output,
        if publisher_ready {
            "auto-publish allowed"
        } else if integrity_ready {
            "honest non-publishing verdict; left in review"
        } else {
            "left in review with blocking issues"
        },
    );

    let outcome = ReviewLoopOutcome {
        publisher_ready,
        deterministic_status,
        halted: false,
        blocking_issues: policy_gate["blocking_issues"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        artifact_dir: artifact_dir.display().to_string(),
        report_path: artifact_dir
            .join("review_loop_report.json")
            .display()
            .to_string(),
        report,
    };
    apply_review_loop_meta_summary(pool, review_id, &outcome).await?;
    crate::cli_status::emit_stage(
        6,
        6,
        "Review loop",
        if outcome.publisher_ready {
            cli_status::StatusMark::Ok
        } else {
            cli_status::StatusMark::Fail
        },
        &format!("deterministic_status={}", outcome.deterministic_status),
    );
    Ok(outcome)
}

async fn halt_review_loop_for_n5(
    pool: &sqlx::PgPool,
    review_id: Uuid,
    stages: &[ReviewLoopStage],
    artifact_dir: &Path,
    dossier: serde_json::Value,
    theorem_map: serde_json::Value,
    semantic_adequacy: serde_json::Value,
    lean_results: serde_json::Value,
    debug_output: bool,
) -> anyhow::Result<ReviewLoopOutcome> {
    use grokrxiv_schemas::VerifierStatus;

    write_loop_json(&artifact_dir.join("never_event_dossier.json"), &dossier).await?;
    let blocking_issue = dossier
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("N5 fake proof never-event triggered.")
        .to_string();
    let policy_gate = serde_json::json!({
        "deterministic_status": "halted",
        "publisher_ready": false,
        "halted": true,
        "halted_by_never_event": "N5_fake_proof",
        "never_events": [dossier.clone()],
        "blocking_issues": [blocking_issue.clone()],
        "component_status": {
            "lean": "false_proof_halt",
            "semantic_adequacy": semantic_adequacy["status"],
            "citation_validation": "not_run",
            "pr_fixer": "not_run",
        },
        "publishability_vector": {
            "formal": "false_proof_halt",
            "semantic_adequacy": semantic_adequacy["status"],
            "citation": "not_run",
            "reproducibility": "not_run",
            "integrity": "halted",
            "safety": "halted",
        },
        "release_tier": {
            "tier": "in_review",
            "lifecycle_state": "human_escalation_required",
        }
    });
    write_loop_json(&artifact_dir.join("policy_gate.json"), &policy_gate).await?;
    record_review_loop_node(
        pool,
        review_id,
        stages,
        "policy_gate",
        policy_gate.clone(),
        VerifierStatus::Fail,
        serde_json::json!({
            "artifact_path": "review_loop/policy_gate.json",
            "never_event_dossier": "review_loop/never_event_dossier.json"
        }),
    )
    .await?;
    emit_review_loop_node_debug(
        "policy_gate",
        false,
        "review_loop/policy_gate.json",
        debug_output,
        "N5 fake proof halt; human escalation required",
    );

    let publish_decision = serde_json::json!({
        "publisher_ready": false,
        "action": "human_escalation_required",
        "auto_publish": false,
        "halted_by_never_event": "N5_fake_proof",
    });
    write_loop_json(
        &artifact_dir.join("publish_decision.json"),
        &publish_decision,
    )
    .await?;

    let report = serde_json::json!({
        "review_id": review_id,
        "dag_type": "review-loop",
        "deterministic_status": "halted",
        "publisher_ready": false,
        "halted": true,
        "halted_by_never_event": "N5_fake_proof",
        "never_events": [dossier],
        "blocking_issues": policy_gate["blocking_issues"],
        "artifact_paths": {
            "lean": "review_loop/lean/results.json",
            "lean_theorem_map": "review_loop/lean/theorem_map.json",
            "lean_verification_report": "review_loop/lean/verification_report.json",
            "semantic_adequacy": "review_loop/semantic_adequacy.json",
            "policy_gate": "review_loop/policy_gate.json",
            "never_event_dossier": "review_loop/never_event_dossier.json",
            "publish_decision": "review_loop/publish_decision.json",
        },
        "fix_attempts": {
            "lean": lean_results["attempts"],
        },
        "theorem_formalization": theorem_map,
        "semantic_adequacy": semantic_adequacy,
        "publishability_vector": policy_gate["publishability_vector"],
        "release_tier": policy_gate["release_tier"],
        "publish_decision": publish_decision,
    });
    write_loop_json(&artifact_dir.join("review_loop_report.json"), &report).await?;
    record_review_loop_node(
        pool,
        review_id,
        stages,
        "review_loop_report",
        report.clone(),
        VerifierStatus::Fail,
        serde_json::json!({
            "artifact_path": "review_loop/review_loop_report.json",
            "never_event_dossier": "review_loop/never_event_dossier.json"
        }),
    )
    .await?;
    emit_review_loop_node_debug(
        "review_loop_report",
        false,
        "review_loop/review_loop_report.json",
        debug_output,
        "N5 halt report persisted",
    );

    record_review_loop_node(
        pool,
        review_id,
        stages,
        "publish_decision",
        publish_decision,
        VerifierStatus::Fail,
        serde_json::json!({"artifact_path": "review_loop/publish_decision.json"}),
    )
    .await?;
    emit_review_loop_node_debug(
        "publish_decision",
        false,
        "review_loop/publish_decision.json",
        debug_output,
        "human escalation required; no publishing action allowed",
    );

    let outcome = ReviewLoopOutcome {
        publisher_ready: false,
        deterministic_status: "halted".to_string(),
        halted: true,
        blocking_issues: vec![blocking_issue],
        artifact_dir: artifact_dir.display().to_string(),
        report_path: artifact_dir
            .join("review_loop_report.json")
            .display()
            .to_string(),
        report,
    };
    apply_review_loop_meta_summary(pool, review_id, &outcome).await?;
    crate::cli_status::emit_stage(
        6,
        6,
        "Review loop",
        cli_status::StatusMark::Fail,
        "deterministic_status=halted never_event=N5_fake_proof",
    );
    Ok(outcome)
}

fn extract_review_loop_claims(
    agent_rows: &[(
        String,
        String,
        Option<String>,
        serde_json::Value,
        Option<serde_json::Value>,
    )],
) -> Vec<serde_json::Value> {
    let mut claims = Vec::new();
    for (role, _dag_type, verifier_status, output, _notes) in agent_rows {
        collect_claim_strings(role, output, &mut claims, verifier_status.as_deref());
    }
    claims
        .into_iter()
        .enumerate()
        .map(|(idx, mut value)| {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "id".to_string(),
                    serde_json::json!(format!("claim_{}", idx + 1)),
                );
            }
            value
        })
        .collect()
}

fn collect_claim_strings(
    role: &str,
    value: &serde_json::Value,
    out: &mut Vec<serde_json::Value>,
    verifier_status: Option<&str>,
) {
    match value {
        serde_json::Value::String(text) if !text.trim().is_empty() => {
            out.push(serde_json::json!({
                "role": role,
                "text": truncate(text.trim(), 500),
                "verifier_status": verifier_status,
            }));
        }
        serde_json::Value::Array(items) => {
            for item in items.iter().take(12) {
                collect_claim_strings(role, item, out, verifier_status);
            }
        }
        serde_json::Value::Object(map) => {
            for key in [
                "tldr",
                "plain_language_summary",
                "overall_correctness",
                "verdict",
                "summary",
                "recommendation",
                "relation",
                "delta",
                "notes",
                "explanation",
            ] {
                if let Some(v) = map.get(key) {
                    collect_claim_strings(role, v, out, verifier_status);
                }
            }
            for key in [
                "claims",
                "key_contributions",
                "related_work",
                "missing_prior_art",
                "concerns",
                "strengths",
                "weaknesses",
                "questions",
                "entries",
            ] {
                if let Some(v) = map.get(key) {
                    collect_claim_strings(role, v, out, verifier_status);
                }
            }
        }
        _ => {}
    }
}

fn build_review_loop_knowledge_graph(claims_value: &serde_json::Value) -> serde_json::Value {
    let claims = claims_value
        .get("claims")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for claim in claims {
        let id = claim.get("id").and_then(|v| v.as_str()).unwrap_or("claim");
        let role = claim
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        nodes.push(serde_json::json!({
            "id": id,
            "kind": "claim",
            "label": truncate(claim.get("text").and_then(|v| v.as_str()).unwrap_or("claim"), 120),
            "role": role,
        }));
        let role_id = format!("role:{role}");
        if !nodes.iter().any(|node| node["id"] == role_id) {
            nodes.push(serde_json::json!({
                "id": role_id,
                "kind": "review_role",
                "label": role,
            }));
        }
        edges.push(serde_json::json!({
            "from": role_id,
            "to": id,
            "relation": "emits_claim",
        }));
    }
    serde_json::json!({
        "nodes": nodes,
        "edges": edges,
    })
}

fn skipped_review_fix_code_results(
    task: &ReviewFixCodeTask,
    final_path: &Path,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "stage": format!("{}_review_fix_code", task.target_id),
        "target": task.target_id,
        "language": task.language,
        "filename": task.filename,
        "author_role": task.author_role,
        "reviewer_role": task.reviewer_role,
        "fixer_role": task.fixer_role,
        "max_attempts": task.max_attempts,
        "attempts": [
            {
                "attempt": 0,
                "status": "fail",
                "skipped": true,
                "semantic_validation": {
                    "status": "fail",
                    "issues": [reason]
                }
            }
        ],
        "agent_output_audit_summary": {
            "total": 0,
            "accepted": 0,
            "rejected": 0,
            "by_role": {}
        },
        "status": "fail",
        "skipped": true,
        "skip_reason": reason,
        "final_path": final_path.display().to_string(),
    })
}

fn annotate_lean_review_fix_code_results(
    mut results: serde_json::Value,
    proof_obligations: &serde_json::Value,
) -> serde_json::Value {
    let theorem_map = grokrxiv_review_loop::build_theorem_map(proof_obligations, &results);
    let proof_status = theorem_map
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("FAILED");
    let verdict = if proof_status == "PROVED" {
        "PROVED"
    } else {
        "NOT_PROVED"
    };
    if let Some(object) = results.as_object_mut() {
        object.insert("verdict".to_string(), serde_json::json!(verdict));
        object.insert("proof_status".to_string(), serde_json::json!(proof_status));
        object.insert(
            "entries".to_string(),
            theorem_map
                .get("entries")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        );
    }
    results
}

fn emit_review_loop_node_debug(
    node_id: &str,
    pass: bool,
    artifact_path: &str,
    debug_output: bool,
    detail: &str,
) {
    if pass && !debug_output {
        return;
    }
    let mark = if pass {
        cli_status::StatusMark::Ok
    } else {
        cli_status::StatusMark::Fail
    };
    let detail = if detail.trim().is_empty() {
        format!("artifact={artifact_path}")
    } else {
        format!("artifact={artifact_path} {}", truncate(detail.trim(), 260))
    };
    cli_status::emit_detail(node_id, mark, &detail);
}

async fn run_loop_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout_dur: std::time::Duration,
) -> CommandRunReport {
    let started = std::time::Instant::now();
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let command_vec = std::iter::once(program.to_string())
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect::<Vec<_>>();
    match tokio::time::timeout(timeout_dur, command.output()).await {
        Err(_) => CommandRunReport {
            command: command_vec,
            status: "timeout".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: format!("command exceeded {}s", timeout_dur.as_secs()),
            duration_ms: started.elapsed().as_millis(),
        },
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => CommandRunReport {
            command: command_vec,
            status: "unavailable".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: format!("{program} not found"),
            duration_ms: started.elapsed().as_millis(),
        },
        Ok(Err(err)) => CommandRunReport {
            command: command_vec,
            status: "fail".to_string(),
            exit_code: None,
            stdout: String::new(),
            stderr: err.to_string(),
            duration_ms: started.elapsed().as_millis(),
        },
        Ok(Ok(output)) => CommandRunReport {
            command: command_vec,
            status: if output.status.success() {
                "pass".to_string()
            } else {
                "fail".to_string()
            },
            exit_code: output.status.code(),
            stdout: truncate(&String::from_utf8_lossy(&output.stdout), 4000),
            stderr: truncate(&String::from_utf8_lossy(&output.stderr), 4000),
            duration_ms: started.elapsed().as_millis(),
        },
    }
}

async fn run_review_loop_pr_fixer(
    state: &super::AppState,
    paper_id: Uuid,
    review_id: Uuid,
    artifact_dir: &Path,
    debug_output: bool,
) -> anyhow::Result<serde_json::Value> {
    let fixed_dir = artifact_dir.join("fixed");
    tokio::fs::create_dir_all(&fixed_dir).await?;
    let render_dir = crate::artifacts::review_artifact_dir(review_id);
    let source_tex = render_dir.join("review.tex");
    let fixed_tex = fixed_dir.join("review.tex");
    let source_tex_body = tokio::fs::read_to_string(&source_tex).await.ok();
    let (compile_program, compile_args) = latex_compile_command();
    if let Some(report) = try_compile_existing_pr_artifact(
        &source_tex,
        &fixed_tex,
        &fixed_dir,
        compile_program,
        &compile_args,
        120,
    )
    .await?
    {
        write_loop_json(&artifact_dir.join("pr_fixes.json"), &report).await?;
        return Ok(report);
    }
    let compile_command = std::iter::once(compile_program)
        .chain(compile_args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let fix_loop = run_review_fix_code_loop(
        state,
        paper_id,
        review_id,
        ReviewFixCodeTask {
            target_id: "pr",
            language: "latex",
            filename: "review.tex",
            author_role: "pr_artifact_fixer",
            reviewer_role: "pr_artifact_reviewer",
            fixer_role: "pr_artifact_fixer",
            compile_program,
            compile_args: compile_args.clone(),
            compile_timeout_secs: 120,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        },
        serde_json::json!({
            "review_id": review_id,
            "task": "Generate corrected GrokRxiv review LaTeX for PR publication.",
            "requirements": [
                "Return the complete review.tex file.",
                "Preserve review identifiers and substantive review evidence.",
                "Fix LaTeX/build/formatting issues only.",
                format!("The artifact must compile with {compile_command}.")
            ],
            "compile_command": compile_command,
            "source_tex_path": source_tex.display().to_string(),
            "source_tex": source_tex_body,
        }),
        &fixed_dir,
        &fixed_tex,
        debug_output,
    )
    .await;
    let mut issues = Vec::new();
    if fix_loop.get("status").and_then(|value| value.as_str()) != Some("pass") {
        issues.push(review_fix_loop_summary(&fix_loop));
    }
    let pdf_path = fixed_dir.join("review.pdf");
    if !pdf_path.is_file() {
        issues.push("fixed review.pdf was not produced".to_string());
    }
    let report = serde_json::json!({
        "stage": "pr_fixer",
        "status": if issues.is_empty() { "pass" } else { "fail" },
        "artifact_worktree": fixed_dir.display().to_string(),
        "fixed_tex": "review_loop/fixed/review.tex",
        "fixed_pdf": if pdf_path.is_file() { Some("review_loop/fixed/review.pdf") } else { None::<&str> },
        "compile_review_loop": fix_loop,
        "issues": issues,
    });
    write_loop_json(&artifact_dir.join("pr_fixes.json"), &report).await?;
    Ok(report)
}

async fn try_compile_existing_pr_artifact(
    source_tex: &Path,
    fixed_tex: &Path,
    fixed_dir: &Path,
    compile_program: &str,
    compile_args: &[String],
    compile_timeout_secs: u64,
) -> anyhow::Result<Option<serde_json::Value>> {
    if !source_tex.is_file() {
        return Ok(None);
    }
    tokio::fs::create_dir_all(fixed_dir)
        .await
        .with_context(|| format!("create fixed PR artifact dir {}", fixed_dir.display()))?;
    let pdf_path = fixed_dir.join("review.pdf");
    let _ = tokio::fs::remove_file(&pdf_path).await;
    tokio::fs::copy(source_tex, fixed_tex)
        .await
        .with_context(|| {
            format!(
                "copy rendered review tex {} to {}",
                source_tex.display(),
                fixed_tex.display()
            )
        })?;

    let compile_args = compile_args.iter().map(String::as_str).collect::<Vec<_>>();
    let compile_run = run_loop_command(
        compile_program,
        &compile_args,
        fixed_dir,
        std::time::Duration::from_secs(compile_timeout_secs),
    )
    .await;
    if compile_run.status != "pass" || !pdf_path.is_file() {
        return Ok(None);
    }
    let compile_value =
        serde_json::to_value(&compile_run).unwrap_or_else(|_| serde_json::json!({}));
    let compile_loop = serde_json::json!({
        "stage": "pr_review_fix_code",
        "target": "pr",
        "language": "latex",
        "filename": "review.tex",
        "author_role": "deterministic_pr_artifact_compiler",
        "reviewer_role": "deterministic_pr_artifact_compiler",
        "fixer_role": "deterministic_pr_artifact_compiler",
        "compile_timeout_secs": compile_timeout_secs,
        "max_attempts": 0,
        "attempts": [{
            "attempt": 0,
            "status": "pass",
            "source_path": source_tex.display().to_string(),
            "final_path": fixed_tex.display().to_string(),
            "compile": compile_value,
        }],
        "agent_output_audit_summary": {
            "total": 0,
            "accepted": 0,
            "rejected": 0,
            "by_role": {}
        },
        "status": "pass",
        "final_path": fixed_tex.display().to_string(),
        "harness": {
            "path": fixed_dir.display().to_string(),
            "branch": null
        },
    });
    Ok(Some(serde_json::json!({
        "stage": "pr_fixer",
        "status": "pass",
        "artifact_worktree": fixed_dir.display().to_string(),
        "fixed_tex": "review_loop/fixed/review.tex",
        "fixed_pdf": "review_loop/fixed/review.pdf",
        "compile_review_loop": compile_loop,
        "issues": [],
    })))
}

fn latex_compile_command() -> (&'static str, Vec<String>) {
    if command_available("tectonic") {
        return (
            "tectonic",
            vec![
                "--outdir".to_string(),
                ".".to_string(),
                "review.tex".to_string(),
            ],
        );
    }
    if command_available("latexmk") {
        return (
            "latexmk",
            vec![
                "-pdf".to_string(),
                "-interaction=nonstopmode".to_string(),
                "-halt-on-error".to_string(),
                "review.tex".to_string(),
            ],
        );
    }
    if command_available("pdflatex") {
        return (
            "pdflatex",
            vec![
                "-interaction=nonstopmode".to_string(),
                "-halt-on-error".to_string(),
                "review.tex".to_string(),
            ],
        );
    }
    (
        "tectonic",
        vec![
            "--outdir".to_string(),
            ".".to_string(),
            "review.tex".to_string(),
        ],
    )
}

fn command_available(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(program);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

async fn write_loop_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, serde_json::to_vec_pretty(value)?).await?;
    Ok(())
}

fn review_loop_artifact_output_paths(manifest_output: &str) -> Option<(String, String)> {
    let bundle_rel = manifest_output
        .strip_prefix("review_loop/")
        .unwrap_or(manifest_output);
    if Path::new(bundle_rel).extension().is_none() {
        return None;
    }
    let artifact_path = if manifest_output.starts_with("review_loop/") {
        manifest_output.to_string()
    } else {
        format!("review_loop/{manifest_output}")
    };
    Some((bundle_rel.to_string(), artifact_path))
}

fn review_loop_skip_reason(
    skip_reasons: &BTreeMap<String, String>,
    manifest_output: &str,
    bundle_rel: &str,
    artifact_path: &str,
) -> Option<String> {
    skip_reasons
        .get(manifest_output)
        .or_else(|| skip_reasons.get(artifact_path))
        .or_else(|| skip_reasons.get(bundle_rel))
        .cloned()
}

async fn review_loop_bundle_completeness_report(
    stages: &[ReviewLoopStage],
    artifact_dir: &Path,
    skip_reasons: &BTreeMap<String, String>,
) -> anyhow::Result<serde_json::Value> {
    let mut artifacts = Vec::new();
    let mut present_count = 0usize;
    let mut skipped_count = 0usize;
    let mut missing_count = 0usize;
    let mut failures = Vec::new();

    for stage in stages {
        for manifest_output in &stage.outputs {
            let Some((bundle_rel, artifact_path)) =
                review_loop_artifact_output_paths(manifest_output)
            else {
                continue;
            };
            let fs_path = artifact_dir.join(&bundle_rel);
            let exists = tokio::fs::metadata(&fs_path)
                .await
                .map(|metadata| metadata.is_file())
                .unwrap_or(false);
            let skip_reason =
                review_loop_skip_reason(skip_reasons, manifest_output, &bundle_rel, &artifact_path);
            let status = if exists {
                present_count += 1;
                "present"
            } else if skip_reason.is_some() {
                skipped_count += 1;
                "skipped"
            } else {
                missing_count += 1;
                failures.push(format!(
                    "declared artifact `{artifact_path}` is missing and has no skip_reason"
                ));
                "missing"
            };
            artifacts.push(serde_json::json!({
                "stage_id": stage.id,
                "manifest_output": manifest_output,
                "artifact_path": artifact_path,
                "fs_path": fs_path.display().to_string(),
                "status": status,
                "skip_reason": skip_reason,
            }));
        }
    }

    Ok(serde_json::json!({
        "status": if missing_count == 0 { "pass" } else { "fail" },
        "declared_artifact_count": artifacts.len(),
        "present_count": present_count,
        "skipped_count": skipped_count,
        "missing_count": missing_count,
        "artifacts": artifacts,
        "failures": failures,
    }))
}

fn review_loop_bundle_skip_reasons(
    citation_report: &serde_json::Value,
    pr_fixes: &serde_json::Value,
) -> BTreeMap<String, String> {
    let mut skip_reasons = BTreeMap::new();
    skip_reasons.insert(
        "citation_validation_adjudication.json".to_string(),
        "citation-validation adjudication is not materialized separately by the current review-loop runtime; citation_validation_report.json carries the deterministic citation evidence.".to_string(),
    );
    if pr_fixes
        .get("fixed_pdf")
        .map(|value| value.is_null())
        .unwrap_or(true)
    {
        let reason = pr_fixes
            .get("issues")
            .and_then(|value| value.as_array())
            .and_then(|items| items.first())
            .and_then(|value| value.as_str())
            .unwrap_or("fixed review.pdf was not produced")
            .to_string();
        skip_reasons.insert("review_loop/fixed/review.pdf".to_string(), reason);
    }
    if citation_report
        .get("status")
        .and_then(|value| value.as_str())
        .is_none()
    {
        skip_reasons.insert(
            "citation_validation_report.json".to_string(),
            "citation validation did not return a status field".to_string(),
        );
    }
    skip_reasons
}

async fn record_review_loop_node(
    pool: &sqlx::PgPool,
    review_id: Uuid,
    stages: &[ReviewLoopStage],
    node_id: &str,
    output: serde_json::Value,
    verifier_status: grokrxiv_schemas::VerifierStatus,
    verifier_notes: serde_json::Value,
) -> anyhow::Result<()> {
    let stage = stages
        .iter()
        .find(|stage| stage.id == node_id)
        .ok_or_else(|| anyhow::anyhow!("review-loop manifest missing node `{node_id}`"))?;
    crate::db::insert_review_agent(
        pool,
        crate::db::ReviewAgentInsert {
            review_id,
            dag_type: "review-loop".to_string(),
            role: node_id.to_string(),
            node_id: Some(node_id.to_string()),
            agent_type: Some("verifier".to_string()),
            node_kind: Some(stage.kind.clone()),
            runner: crate::agents::AgentRunnerKind::Cli,
            model: "deterministic-review-loop",
            output,
            verifier_status: Some(verifier_status),
            verifier_notes: Some(verifier_notes),
            tokens_in: None,
            tokens_out: None,
            latency_ms: None,
        },
    )
    .await?;
    Ok(())
}

async fn apply_review_loop_meta_summary(
    pool: &sqlx::PgPool,
    review_id: Uuid,
    outcome: &ReviewLoopOutcome,
) -> anyhow::Result<()> {
    let mut meta: serde_json::Value =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .unwrap_or(None)
            .unwrap_or_else(|| {
                serde_json::json!({
                    "summary": "Review-loop policy evaluated persisted review evidence.",
                    "strengths": [],
                    "weaknesses": [],
                    "questions": [],
                    "recommendation": "major_revision",
                    "confidence": 1.0
                })
            });
    if !meta.is_object() {
        meta = serde_json::json!({});
    }
    let obj = meta.as_object_mut().expect("object checked");
    obj.insert(
        "review_loop".to_string(),
        serde_json::json!({
            "deterministic_status": outcome.deterministic_status,
            "publisher_ready": outcome.publisher_ready,
            "halted": outcome.halted,
            "blocking_issues": outcome.blocking_issues,
            "artifact_dir": outcome.artifact_dir,
            "report_path": outcome.report_path,
        }),
    );
    if !outcome.publisher_ready {
        obj.insert(
            "recommendation".to_string(),
            serde_json::json!("major_revision"),
        );
        let weaknesses = obj
            .entry("weaknesses".to_string())
            .or_insert_with(|| serde_json::json!([]));
        if !weaknesses.is_array() {
            *weaknesses = serde_json::json!([]);
        }
        if let Some(items) = weaknesses.as_array_mut() {
            for issue in &outcome.blocking_issues {
                let text = format!("Review-loop policy blocker: {issue}");
                if !items
                    .iter()
                    .any(|item| item.as_str() == Some(text.as_str()))
                {
                    items.push(serde_json::json!(text));
                }
            }
        }
    }
    crate::db::set_review_meta_review(pool, review_id, &meta).await?;
    Ok(())
}

async fn append_review_loop_pr_files(
    review_id: Uuid,
    repo_prefix: &str,
    files: &mut Vec<(String, Vec<u8>)>,
) {
    let dir = crate::artifacts::review_artifact_dir(review_id).join("review_loop");
    let mut rels = BTreeSet::new();
    match review_loop_stage_plan() {
        Ok(stages) => {
            for stage in stages {
                for output in stage.outputs {
                    if let Some((bundle_rel, _)) = review_loop_artifact_output_paths(&output) {
                        rels.insert(bundle_rel);
                    }
                }
            }
        }
        Err(err) => {
            tracing::warn!(%review_id, err = %err, "review-loop: failed to load manifest outputs for PR bundle");
        }
    }
    for rel in [
        "bundle_completeness.json",
        "haskell/GROKRXIV_HARNESS.md",
        "haskell/harness.json",
        "haskell/harness_task_input.json",
        "lean/GROKRXIV_HARNESS.md",
        "lean/harness.json",
        "lean/harness_task_input.json",
        "fixed/GROKRXIV_HARNESS.md",
        "fixed/harness.json",
        "fixed/harness_task_input.json",
    ] {
        rels.insert(rel.to_string());
    }
    for rel in rels {
        let path = dir.join(&rel);
        match tokio::fs::read(&path).await {
            Ok(bytes) => files.push((format!("{repo_prefix}/review_loop/{rel}"), bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    %review_id,
                    path = %path.display(),
                    err = %err,
                    "review-loop: failed to attach PR artifact"
                );
            }
        }
    }
}

async fn load_publication_gate_context(
    pool: &sqlx::PgPool,
    review_id: Uuid,
) -> anyhow::Result<(
    Option<serde_json::Value>,
    crate::review_gate::PublicationGate,
    crate::review_gate::SpecialistGate,
)> {
    let meta_review: Option<serde_json::Value> =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .unwrap_or(None);
    let recommendation = meta_review
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(|v| v.as_str());
    let specialist_gate = crate::db::load_specialist_gate_for_review(pool, review_id).await?;
    let publication_gate =
        crate::review_gate::PublicationGate::evaluate(crate::review_gate::PublicationGateInput {
            recommendation,
            specialist_gate: specialist_gate.clone(),
        });
    Ok((meta_review, publication_gate, specialist_gate))
}

#[derive(Debug, Clone, Serialize)]
struct CitationVerifierSummary {
    verifier_status: Option<String>,
    checked: u64,
    coverage_status: Option<String>,
    reason: Option<String>,
    unresolved: u64,
    retracted: u64,
    unverified: u64,
    unknown: u64,
    malformed: u64,
    unresolved_fraction: f64,
    evidence: Vec<CitationEvidenceItem>,
    artifact_hint: String,
}

fn paper_review_citation_verifier_role() -> Option<String> {
    let roles = agent_config::dag_roles_with_postprocessor(
        PAPER_REVIEW_DAG_ID,
        CITATION_VERIFIER_POSTPROCESSOR,
    )
    .ok()?;
    roles.into_iter().next()
}

fn paper_review_specialist_roles() -> anyhow::Result<Vec<String>> {
    let roles = agent_config::dag_feeds_meta_roles(PAPER_REVIEW_DAG_ID)?;
    if roles.is_empty() {
        anyhow::bail!("DAG `{PAPER_REVIEW_DAG_ID}` declares no feeds_meta specialist roles");
    }
    Ok(roles)
}

impl CitationVerifierSummary {
    fn to_markdown(&self) -> String {
        if self.checked == 0 {
            return format!(
                "**Citation verifier:** not externally checked (checked=0). {}\n\n\
                 Full evidence is in `{}`.",
                self.reason.as_deref().unwrap_or(
                    "No extracted bibliography entries were available for citation resolution."
                ),
                self.artifact_hint,
            );
        }
        let mut out = format!(
            "**Citation verifier:** checked={}, not_resolved={}, retracted={}, needs_review={}, unknown={}, malformed={}, fail_fraction={:.3}.\n\n\
             Full evidence is in `{}`.",
            self.checked,
            self.unresolved,
            self.retracted,
            self.unverified,
            self.unknown,
            self.malformed,
            self.unresolved_fraction,
            self.artifact_hint,
        );
        if !self.evidence.is_empty() {
            out.push_str("\n\nCitation checks needing review:\n");
            for item in &self.evidence {
                out.push_str("- ");
                out.push_str(&item.to_human_line());
                out.push('\n');
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
struct CitationEvidenceItem {
    key: Option<String>,
    title: Option<String>,
    author: Option<String>,
    year: Option<String>,
    doi: Option<String>,
    arxiv_id: Option<String>,
    url: Option<String>,
    status: String,
    source: Option<String>,
    reason: Option<String>,
}

impl CitationEvidenceItem {
    fn from_verifier_entry(entry: &serde_json::Value) -> Option<Self> {
        let raw = entry
            .get("raw")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let status = entry
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unverified")
            .to_string();
        Some(Self {
            key: entry
                .get("citation_key")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| citation_key_from_raw(raw)),
            title: entry
                .get("title")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty())
                .or_else(|| bib_field(raw, "title")),
            author: entry
                .get("author")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty())
                .or_else(|| bib_field(raw, "author")),
            year: entry
                .get("year")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty())
                .or_else(|| bib_field(raw, "year").or_else(|| bib_field(raw, "date"))),
            doi: entry
                .get("doi")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty())
                .or_else(|| bib_field(raw, "doi")),
            arxiv_id: entry
                .get("arxiv_id")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty()),
            url: entry
                .get("url")
                .and_then(|v| v.as_str())
                .map(clean_citation_text)
                .filter(|s| !s.is_empty())
                .or_else(|| bib_field(raw, "url")),
            status,
            source: entry
                .get("source")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reason: entry
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }

    fn to_human_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(key) = self.key.as_deref() {
            parts.push(key.to_string());
        }
        let mut title = self
            .title
            .clone()
            .unwrap_or_else(|| "Untitled citation".to_string());
        if let Some(year) = self.year.as_deref() {
            if !title.contains(year) {
                title.push_str(&format!(" ({year})"));
            }
        }
        parts.push(title);
        if let Some(author) = self.author.as_deref().and_then(short_author_label) {
            parts.push(author);
        }
        parts.push(self.status_label().to_string());
        if let Some(identifier) = self.identifier_label() {
            parts.push(identifier);
        }
        if let Some(reason) = self.reason.as_deref() {
            parts.push(human_citation_reason(reason));
        }
        truncate(&parts.join(" — "), 280)
    }

    fn status_label(&self) -> &'static str {
        match self.effective_status() {
            "unresolved" => "not resolved",
            "malformed" => "malformed identifier",
            "transient_unknown" => "temporarily unknown",
            "unverified" => "needs verification",
            "retracted" => "retracted",
            "resolved" => "verified",
            _ => "needs review",
        }
    }

    fn effective_status(&self) -> &str {
        if self.status == "unresolved" && self.is_bibliographic_coverage_gap() {
            "unverified"
        } else {
            self.status.as_str()
        }
    }

    fn is_bibliographic_coverage_gap(&self) -> bool {
        let source_is_biblio = self.source.as_deref() == Some("crossref_bibliographic");
        let reason_is_coverage_gap = self
            .reason
            .as_deref()
            .map(|reason| {
                let lower = reason.to_ascii_lowercase();
                lower.contains("no bibliographic match above score threshold")
                    || lower.contains("no match above score threshold")
            })
            .unwrap_or(false);
        source_is_biblio && reason_is_coverage_gap
    }

    fn identifier_label(&self) -> Option<String> {
        if let Some(doi) = self.doi.as_deref() {
            return Some(format!("doi:{doi}"));
        }
        if let Some(arxiv_id) = self.arxiv_id.as_deref() {
            return Some(format!("arXiv:{arxiv_id}"));
        }
        if let Some(url) = self.url.as_deref() {
            return Some(format!("url:{url}"));
        }
        None
    }
}

fn short_author_label(author: &str) -> Option<String> {
    let author = author.trim();
    if author.is_empty() {
        return None;
    }
    let first = author.split(" and ").next().unwrap_or(author).trim();
    let surname = first
        .split(',')
        .next()
        .unwrap_or(first)
        .split_whitespace()
        .last()
        .unwrap_or(first)
        .trim();
    if surname.is_empty() {
        return None;
    }
    if author.contains(" and ") {
        Some(format!("{surname} et al."))
    } else {
        Some(surname.to_string())
    }
}

fn human_citation_reason(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("no bibliographic match above score threshold")
        || lower.contains("no match above score threshold")
    {
        "Crossref bibliographic search did not find a strong match".to_string()
    } else if lower.contains("crossref status 404") && lower.contains("doi resolver status 404") {
        "not found by Crossref or DOI resolver".to_string()
    } else if lower.contains("crossref status 404") {
        "not found in Crossref".to_string()
    } else if lower.contains("doi resolver status 404") {
        "DOI resolver returned 404".to_string()
    } else if lower.contains("not present in arxiv response") {
        "not present in arXiv metadata response".to_string()
    } else if lower.contains("malformed") {
        reason.to_string()
    } else if lower.contains("status 429") || lower.contains("status 5") {
        format!("temporary lookup issue: {reason}")
    } else {
        reason.to_string()
    }
}

fn citation_key_from_raw(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('@') {
        if let Some((_, after_open)) = rest.split_once('{') {
            let key = after_open.split(',').next().unwrap_or_default().trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    if let Some((key, _)) = trimmed.split_once(':') {
        let key = key.trim();
        if !key.is_empty()
            && key.len() <= 96
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        {
            return Some(key.to_string());
        }
    }
    None
}

fn bib_field(raw: &str, field: &str) -> Option<String> {
    let idx = raw
        .find(&format!("{field} ="))
        .or_else(|| raw.find(&format!("{field}=")))?;
    let after_equals = raw[idx..].split_once('=')?.1.trim_start();
    let (value, _) = parse_bib_value(after_equals)?;
    let cleaned = clean_citation_text(&value);
    (!cleaned.is_empty()).then_some(cleaned)
}

fn parse_bib_value(input: &str) -> Option<(String, usize)> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first == '{' {
        let mut depth = 1usize;
        let start = 1usize;
        for (idx, ch) in chars {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some((input[start..idx].to_string(), idx + ch.len_utf8()));
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    if first == '"' {
        let start = 1usize;
        for (idx, ch) in chars {
            if ch == '"' {
                return Some((input[start..idx].to_string(), idx + ch.len_utf8()));
            }
        }
        return None;
    }
    let value = input
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((value, input.find(',').unwrap_or(input.len())))
}

fn clean_citation_text(value: &str) -> String {
    value
        .replace("{{", "")
        .replace("}}", "")
        .replace('{', "")
        .replace('}', "")
        .replace("\\\"", "\"")
        .replace("\\'", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

async fn citation_verifier_summary(
    pool: &sqlx::PgPool,
    review_id: Uuid,
) -> Option<CitationVerifierSummary> {
    let role = paper_review_citation_verifier_role()?;
    let row: Option<(Option<String>, Option<serde_json::Value>)> = sqlx::query_as(
        "select verifier_status, verifier_notes \
         from review_agents \
         where review_id = $1 and role = $2 \
         order by created_at desc \
         limit 1",
    )
    .bind(review_id)
    .bind(&role)
    .fetch_optional(pool)
    .await
    .ok()?;
    let (verifier_status, verifier_notes) = row?;
    let notes = verifier_notes.as_ref()?;
    let citation_notes = notes
        .get("citation")
        .and_then(|v| v.get("notes"))
        .unwrap_or(notes);
    let checked = citation_notes
        .get("checked")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let coverage_status = citation_notes
        .get("coverage_status")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reason = citation_notes
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let entry_items: Vec<CitationEvidenceItem> = citation_notes
        .get("entries")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(CitationEvidenceItem::from_verifier_entry)
        .collect();
    let (unresolved, retracted, unverified, unknown, malformed, unresolved_fraction) =
        if entry_items.is_empty() {
            let unresolved = citation_notes
                .get("unresolved")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let retracted = citation_notes
                .get("retracted")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let unverified = citation_notes
                .get("unverified")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let unknown = citation_notes
                .get("unknown")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let malformed = citation_notes
                .get("malformed")
                .and_then(|v| v.as_array())
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let unresolved_fraction = citation_notes
                .get("unresolved_fraction")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            (
                unresolved,
                retracted,
                unverified,
                unknown,
                malformed,
                unresolved_fraction,
            )
        } else {
            let unresolved = entry_items
                .iter()
                .filter(|entry| entry.effective_status() == "unresolved")
                .count() as u64;
            let retracted = entry_items
                .iter()
                .filter(|entry| entry.effective_status() == "retracted")
                .count() as u64;
            let unverified = entry_items
                .iter()
                .filter(|entry| entry.effective_status() == "unverified")
                .count() as u64;
            let unknown = entry_items
                .iter()
                .filter(|entry| entry.effective_status() == "transient_unknown")
                .count() as u64;
            let malformed = entry_items
                .iter()
                .filter(|entry| entry.effective_status() == "malformed")
                .count() as u64;
            let definitive_total = checked.saturating_sub(unknown).saturating_sub(unverified);
            let bad = unresolved + malformed + retracted;
            let unresolved_fraction = if definitive_total == 0 {
                0.0
            } else {
                bad as f64 / definitive_total as f64
            };
            (
                unresolved,
                retracted,
                unverified,
                unknown,
                malformed,
                unresolved_fraction,
            )
        };
    let evidence = entry_items
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.effective_status(),
                "unresolved" | "retracted" | "unverified" | "transient_unknown" | "malformed"
            )
        })
        .take(8)
        .collect();
    Some(CitationVerifierSummary {
        verifier_status,
        checked,
        coverage_status,
        reason,
        unresolved,
        retracted,
        unverified,
        unknown,
        malformed,
        unresolved_fraction,
        evidence,
        artifact_hint: format!(
            "{}/bundle.zip agents/{role}.json",
            crate::artifacts::review_artifact_ref(review_id)
        ),
    })
}

async fn open_review_pr_for_gate(
    state: &super::AppState,
    review_id: Uuid,
    json: bool,
    emit_output: bool,
) -> anyhow::Result<ReviewPrDispatchOutcome> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("review: DATABASE_URL not configured"))?;
    let (_, gate, _) = load_publication_gate_context(pool, review_id).await?;
    let kind = review_pr_dispatch_kind(&gate);
    let pr_url = match kind {
        ReviewPrDispatchKind::Publication => {
            open_publication_pr_impl(state, review_id, false, json, emit_output).await?
        }
        ReviewPrDispatchKind::RevisionNeeded => {
            request_revisions_impl(state, review_id, None, json, emit_output).await?
        }
    };
    crate::cli_status::emit(format!(
        "review {review_id}: PR [{}] {pr_url}",
        kind.as_str()
    ));
    Ok(ReviewPrDispatchOutcome {
        pr_url: Some(pr_url),
        gate_verdict: gate.verdict,
        recommendation: gate.recommendation,
        kind,
        external_actions_enabled: true,
    })
}

async fn open_review_pr_after_optional_loop(
    state: &super::AppState,
    review_id: Uuid,
    loop_enabled: bool,
    debug_output: bool,
    external_actions_enabled: bool,
    json: bool,
) -> anyhow::Result<(ReviewPrDispatchOutcome, Option<ReviewLoopOutcome>)> {
    let loop_outcome = if loop_enabled {
        Some(run_review_loop_for_review(state, review_id, debug_output).await?)
    } else {
        None
    };
    if !review_loop_external_actions_allowed(external_actions_enabled, loop_outcome.as_ref()) {
        if loop_outcome
            .as_ref()
            .is_some_and(review_loop_outcome_halted)
        {
            let outcome = review_pr_dispatch_skipped_for_loop_halt();
            crate::cli_status::emit(format!(
                "review {review_id}: review-loop halted; skipped PR [{}]",
                outcome.kind.as_str()
            ));
            return Ok((outcome, loop_outcome));
        }
        let pool = state
            .db
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("review: DATABASE_URL not configured"))?;
        let (_, gate, _) = load_publication_gate_context(pool, review_id).await?;
        let outcome = review_pr_dispatch_skipped_by_policy(&gate);
        crate::cli_status::emit(format!(
            "review {review_id}: external actions disabled; skipped PR [{}]",
            outcome.kind.as_str()
        ));
        return Ok((outcome, loop_outcome));
    }
    let pr = open_review_pr_for_gate(state, review_id, json, false).await?;
    Ok((pr, loop_outcome))
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
        println!(
            "review_id={review_id} artifacts={}",
            crate::artifacts::review_artifact_ref(review_id)
        );
        Ok(())
    }
    #[cfg(not(feature = "grokrxiv-render"))]
    {
        let _ = (review_id, format, out);
        anyhow::bail!("render requires --features full (grokrxiv-render)")
    }
}

#[derive(Debug, Clone, Serialize)]
struct RefreshStageResult {
    name: &'static str,
    status: String,
    duration_ms: u128,
    message: Option<String>,
    error: Option<String>,
}

fn refresh_stage_result(
    name: &'static str,
    status: impl Into<String>,
    started: std::time::Instant,
    message: Option<String>,
    error: Option<String>,
) -> RefreshStageResult {
    RefreshStageResult {
        name,
        status: status.into(),
        duration_ms: started.elapsed().as_millis(),
        message,
        error,
    }
}

fn refresh_stage_timeout() -> std::time::Duration {
    std::env::var("AGENTHERO_REFRESH_STAGE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| std::time::Duration::from_secs(15))
}

fn refresh_render_timeout() -> std::time::Duration {
    std::env::var("AGENTHERO_REFRESH_RENDER_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| {
            let html_quality_secs = std::env::var("GROKRXIV_HTML_QUALITY_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|secs| *secs > 0)
                .unwrap_or(180);
            std::time::Duration::from_secs(html_quality_secs.saturating_add(30))
        })
}

fn refresh_html_quality_timeout_secs(render_timeout: std::time::Duration) -> u32 {
    render_timeout.as_secs().clamp(1, u32::MAX as u64) as u32
}

#[derive(Debug, Clone, Copy)]
struct RefreshRenderOutcome {
    artifacts_refreshed: bool,
    html_quality_enabled: bool,
    html_quality_ran: Option<bool>,
    html_quality_timeout_secs: Option<u32>,
}

impl RefreshRenderOutcome {
    fn message(&self) -> String {
        format!(
            "artifacts_refreshed={} html_quality_enabled={} html_quality_ran={} html_quality_timeout_secs={}",
            self.artifacts_refreshed,
            self.html_quality_enabled,
            self.html_quality_ran
                .map(|ran| ran.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
            self.html_quality_timeout_secs
                .map(|secs| secs.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        )
    }
}

async fn refresh_review(review_id: Uuid, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("refresh-review: DATABASE_URL not configured"))?;
    let mut stages = Vec::new();

    crate::cli_status::emit(format!(
        "review {review_id}: repairing citation verifier metadata"
    ));
    let started = std::time::Instant::now();
    let citation_rows_repaired = repair_zero_checked_citation_agents(pool, review_id).await?;
    stages.push(refresh_stage_result(
        "citation_repair",
        "ok",
        started,
        Some(format!("rows_repaired={citation_rows_repaired}")),
        None,
    ));

    crate::cli_status::emit(format!(
        "review {review_id}: loading persisted review context"
    ));
    let row: Option<(
        Uuid,
        Option<serde_json::Value>,
        Option<String>,
        String,
        Option<String>,
        String,
        serde_json::Value,
    )> = sqlx::query_as(
        "select r.paper_id, r.meta_review, r.github_pr_url, coalesce(p.source_kind, 'arxiv'), \
                p.source_id, p.arxiv_id, p.source_metadata \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await?;
    let Some((_, Some(meta_review), github_pr_url, source_kind, source_id, arxiv_id, metadata)) =
        row
    else {
        anyhow::bail!("refresh-review: review {review_id} not found or missing meta_review");
    };

    crate::cli_status::emit(format!("review {review_id}: enriching revision targets"));
    let started = std::time::Instant::now();
    let specialist_roles = paper_review_specialist_roles()?;
    let agent_rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "select distinct on (role) role, output \
         from review_agents \
         where review_id = $1 and role = any($2) \
         order by role, created_at desc",
    )
    .bind(review_id)
    .bind(&specialist_roles)
    .fetch_all(pool)
    .await?;
    if agent_rows.is_empty() {
        anyhow::bail!("refresh-review: no specialist review_agents rows for {review_id}");
    }
    let specialists = serde_json::Value::Object(
        agent_rows
            .into_iter()
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    );
    let meta_input = serde_json::json!({ "specialists": specialists });
    let source_hint =
        refresh_revision_source_path_hint(&source_kind, source_id.as_deref(), &arxiv_id, &metadata);
    let enriched = crate::revision_targets::enrich_meta_review(
        meta_review.clone(),
        &meta_input,
        source_hint.as_deref(),
    );
    let meta_review_updated = enriched != meta_review;
    if meta_review_updated {
        crate::db::set_review_meta_review(pool, review_id, &enriched).await?;
    }
    stages.push(refresh_stage_result(
        "meta_review_enrichment",
        "ok",
        started,
        Some(format!("updated={meta_review_updated}")),
        None,
    ));

    crate::cli_status::emit(format!(
        "review {review_id}: rendering artifacts and running HTML quality if enabled"
    ));
    let started = std::time::Instant::now();
    let render_timeout = refresh_render_timeout();
    let html_quality_timeout_secs = refresh_html_quality_timeout_secs(render_timeout);
    let render_watchdog_timeout = render_timeout.saturating_add(std::time::Duration::from_secs(5));
    let render_outcome = match tokio::time::timeout(
        render_watchdog_timeout,
        refresh_rendered_artifacts(&state, review_id, Some(html_quality_timeout_secs)),
    )
    .await
    {
        Ok(Ok(outcome)) => {
            stages.push(refresh_stage_result(
                "render_artifacts",
                "ok",
                started,
                Some(outcome.message()),
                None,
            ));
            outcome
        }
        Ok(Err(e)) => {
            stages.push(refresh_stage_result(
                "render_artifacts",
                "failed",
                started,
                Some("artifacts_refreshed=false".to_string()),
                Some(e.to_string()),
            ));
            RefreshRenderOutcome {
                artifacts_refreshed: false,
                html_quality_enabled: false,
                html_quality_ran: None,
                html_quality_timeout_secs: Some(html_quality_timeout_secs),
            }
        }
        Err(_) => {
            stages.push(refresh_stage_result(
                "render_artifacts",
                "timeout",
                started,
                Some("artifacts_refreshed=false".to_string()),
                Some(format!(
                    "render/html_quality watchdog exceeded {}s",
                    render_watchdog_timeout.as_secs()
                )),
            ));
            RefreshRenderOutcome {
                artifacts_refreshed: false,
                html_quality_enabled: false,
                html_quality_ran: None,
                html_quality_timeout_secs: Some(html_quality_timeout_secs),
            }
        }
    };
    let artifacts_refreshed = render_outcome.artifacts_refreshed;

    crate::cli_status::emit(format!(
        "review {review_id}: revalidating configured web endpoint"
    ));
    let web_revalidate = refresh_web_revalidate(
        &state.http,
        state.config.web_revalidate_url.as_deref(),
        state.config.revalidate_secret.as_deref(),
        review_id,
        refresh_stage_timeout(),
    )
    .await;
    stages.push(web_revalidate.clone());

    crate::cli_status::emit(format!(
        "review {review_id}: updating GitHub gate feedback comment"
    ));
    let started = std::time::Instant::now();
    let github_feedback = match tokio::time::timeout(
        refresh_stage_timeout(),
        refresh_gate_feedback_comment(&state, pool, review_id, github_pr_url.as_deref()),
    )
    .await
    {
        Ok(Ok(status)) => {
            stages.push(refresh_stage_result(
                "github_feedback",
                status.clone(),
                started,
                None,
                None,
            ));
            status
        }
        Ok(Err(e)) => {
            stages.push(refresh_stage_result(
                "github_feedback",
                "failed",
                started,
                None,
                Some(e.to_string()),
            ));
            "failed".to_string()
        }
        Err(_) => {
            stages.push(refresh_stage_result(
                "github_feedback",
                "timeout",
                started,
                None,
                Some(format!(
                    "GitHub feedback update exceeded {}s",
                    refresh_stage_timeout().as_secs()
                )),
            ));
            "timeout".to_string()
        }
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "review_id": review_id,
                "citation_rows_repaired": citation_rows_repaired,
                "meta_review_updated": meta_review_updated,
                "artifacts_refreshed": artifacts_refreshed,
                "web_revalidate": web_revalidate.status,
                "github_feedback": github_feedback,
                "stages": stages,
            })
        );
    } else {
        println!(
            "refreshed={review_id} citation_rows_repaired={citation_rows_repaired} meta_review_updated={meta_review_updated} artifacts_refreshed={artifacts_refreshed} web_revalidate={} github_feedback={github_feedback}",
            web_revalidate.status
        );
    }
    Ok(())
}

async fn refresh_web_revalidate(
    http: &reqwest::Client,
    url: Option<&str>,
    secret: Option<&str>,
    review_id: Uuid,
    timeout_dur: std::time::Duration,
) -> RefreshStageResult {
    let started = std::time::Instant::now();
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return refresh_stage_result(
            "web_revalidate",
            "skipped_unset",
            started,
            Some("WEB_REVALIDATE_URL is unset".to_string()),
            None,
        );
    };
    let mut req = http
        .post(url)
        .json(&serde_json::json!({ "review_id": review_id }));
    if let Some(secret) = secret.filter(|secret| !secret.trim().is_empty()) {
        req = req.header("x-revalidate-secret", secret);
    }

    match tokio::time::timeout(timeout_dur, req.send()).await {
        Err(_) => refresh_stage_result(
            "web_revalidate",
            "timeout",
            started,
            None,
            Some(format!(
                "revalidate POST exceeded {}s for {url}",
                timeout_dur.as_secs_f32()
            )),
        ),
        Ok(Err(e)) if e.is_connect() => refresh_stage_result(
            "web_revalidate",
            "skipped_unreachable",
            started,
            Some(format!(
                "configured revalidate endpoint is unreachable: {url}"
            )),
            Some(e.to_string()),
        ),
        Ok(Err(e)) => refresh_stage_result(
            "web_revalidate",
            "failed",
            started,
            None,
            Some(e.to_string()),
        ),
        Ok(Ok(resp)) if resp.status().is_success() => refresh_stage_result(
            "web_revalidate",
            "updated",
            started,
            Some(format!("HTTP {}", resp.status())),
            None,
        ),
        Ok(Ok(resp)) => refresh_stage_result(
            "web_revalidate",
            "failed_http_status",
            started,
            None,
            Some(format!("HTTP {}", resp.status())),
        ),
    }
}

async fn repair_zero_checked_citation_agents(
    pool: &sqlx::PgPool,
    review_id: Uuid,
) -> anyhow::Result<u64> {
    let Some(role) = paper_review_citation_verifier_role() else {
        return Ok(0);
    };
    let rows: Vec<(Uuid, Option<String>, Option<serde_json::Value>)> = sqlx::query_as(
        "select id, verifier_status, verifier_notes from review_agents where review_id = $1 and role = $2",
    )
    .bind(review_id)
    .bind(&role)
    .fetch_all(pool)
    .await?;
    let mut repaired = 0u64;
    for (agent_id, verifier_status, notes) in rows {
        let Some(notes) = notes else {
            continue;
        };
        if citation_checked_count(&notes) != Some(0) {
            continue;
        }
        if verifier_status.as_deref() == Some("fail")
            && citation_coverage_status(&notes).as_deref() == Some("not_checked")
        {
            continue;
        }
        let notes = annotate_zero_checked_citation_notes(notes);
        sqlx::query(
            "update review_agents set verifier_status = 'fail', verifier_notes = $2 where id = $1",
        )
        .bind(agent_id)
        .bind(notes)
        .execute(pool)
        .await?;
        repaired += 1;
    }
    Ok(repaired)
}

fn citation_checked_count(notes: &serde_json::Value) -> Option<u64> {
    notes
        .get("citation")
        .and_then(|v| v.get("notes"))
        .or(Some(notes))
        .and_then(|v| v.get("checked"))
        .and_then(|v| v.as_u64())
}

fn citation_coverage_status(notes: &serde_json::Value) -> Option<String> {
    notes
        .get("citation")
        .and_then(|v| v.get("notes"))
        .or(Some(notes))
        .and_then(|v| v.get("coverage_status"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn annotate_zero_checked_citation_notes(mut notes: serde_json::Value) -> serde_json::Value {
    if let Some(citation) = notes
        .get_mut("citation")
        .and_then(serde_json::Value::as_object_mut)
    {
        citation.insert("status".to_string(), serde_json::json!("fail"));
        let notes_value = citation
            .entry("notes".to_string())
            .or_insert_with(|| serde_json::json!({}));
        annotate_citation_notes_object(notes_value);
    } else {
        annotate_citation_notes_object(&mut notes);
    }
    notes
}

fn annotate_citation_notes_object(notes: &mut serde_json::Value) {
    let Some(obj) = notes.as_object_mut() else {
        return;
    };
    obj.insert("checked".to_string(), serde_json::json!(0));
    obj.insert(
        "coverage_status".to_string(),
        serde_json::json!("not_checked"),
    );
    obj.insert(
        "reason".to_string(),
        serde_json::json!(
            "No extracted bibliography entries were available for external citation verification."
        ),
    );
    obj.entry("entries".to_string())
        .or_insert_with(|| serde_json::json!([]));
}

fn refresh_revision_source_path_hint(
    source_kind: &str,
    source_id: Option<&str>,
    arxiv_id: &str,
    metadata: &serde_json::Value,
) -> Option<String> {
    if let Some(path) = metadata
        .get("correction_source_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return Some(path.to_string());
    }
    let stable_source_id = source_id.unwrap_or(arxiv_id);
    let adapter = metadata.get("adapter").unwrap_or(&serde_json::Value::Null);
    match source_kind {
        "git_repo" => adapter
            .get("paper_path")
            .and_then(|v| v.as_str())
            .map(Path::new)
            .map(|path| correction_repo_path(stable_source_id, path)),
        "local_file" => adapter
            .get("path")
            .and_then(|v| v.as_str())
            .map(Path::new)
            .map(|path| correction_repo_path(stable_source_id, path)),
        "arxiv" => Some(correction_repo_path(
            stable_source_id,
            Path::new("paper.tex"),
        )),
        _ => None,
    }
}

async fn refresh_rendered_artifacts(
    state: &super::AppState,
    review_id: Uuid,
    html_quality_timeout_secs: Option<u32>,
) -> anyhow::Result<RefreshRenderOutcome> {
    #[cfg(feature = "grokrxiv-render")]
    {
        let report = super::supervisor::render_to_disk_with_options(
            state,
            review_id,
            super::supervisor::RenderToDiskOptions {
                html_quality_timeout_secs,
            },
        )
        .await?;
        Ok(RefreshRenderOutcome {
            artifacts_refreshed: true,
            html_quality_enabled: report.html_quality_enabled,
            html_quality_ran: report.html_quality_ran,
            html_quality_timeout_secs,
        })
    }
    #[cfg(not(feature = "grokrxiv-render"))]
    {
        let _ = (state, review_id);
        Ok(RefreshRenderOutcome {
            artifacts_refreshed: false,
            html_quality_enabled: false,
            html_quality_ran: None,
            html_quality_timeout_secs,
        })
    }
}

async fn refresh_gate_feedback_comment(
    state: &super::AppState,
    pool: &sqlx::PgPool,
    review_id: Uuid,
    github_pr_url: Option<&str>,
) -> anyhow::Result<String> {
    let Some(github_pr_url) = github_pr_url else {
        return Ok("none".to_string());
    };
    let plan = match review_pr_close_plan(Some(github_pr_url)) {
        Ok(Some(plan)) => plan,
        Ok(None) => return Ok("skipped".to_string()),
        Err(e) => {
            tracing::warn!(%review_id, err = %e, "refresh-review: invalid GitHub PR URL");
            return Ok("skipped_invalid_pr_url".to_string());
        }
    };
    let (meta_review, publication_gate, _) = load_publication_gate_context(pool, review_id).await?;
    let body = if publication_gate.verdict == crate::review_gate::GateVerdict::Pass {
        crate::github_feedback::gate_pass_comment_body(review_id, &publication_gate.recommendation)
    } else {
        let failure = crate::github_feedback::gate_failure_from_publication_gate(
            review_id,
            &publication_gate,
            meta_review.as_ref(),
        );
        crate::github_feedback::gate_failure_comment_body(
            review_id,
            &publication_gate.recommendation,
            &failure,
        )
    };

    #[cfg(feature = "grokrxiv-publisher")]
    {
        let pr_number = i64::try_from(plan.pr_number)
            .map_err(|_| anyhow::anyhow!("PR number does not fit i64: {}", plan.pr_number))?;
        match crate::github_feedback::post_or_update_gate_feedback_comment(
            state,
            &plan.owner,
            &plan.repo,
            pr_number,
            &format!("review-{review_id}"),
            &body,
        )
        .await
        {
            Ok(Some(comment)) => {
                if let Ok(comment_id) = i64::try_from(comment.comment_id) {
                    let _ = crate::db::attach_gate_feedback_comment(
                        pool,
                        review_id,
                        comment_id,
                        &comment.html_url,
                    )
                    .await;
                    let _ = crate::db::update_github_feedback_comment(
                        pool,
                        review_id,
                        comment_id,
                        &comment.html_url,
                    )
                    .await;
                }
                Ok("updated".to_string())
            }
            Ok(None) => Ok("skipped_no_token".to_string()),
            Err(e) => {
                tracing::warn!(%review_id, err = %e, "refresh-review: GitHub feedback comment failed");
                Ok("failed".to_string())
            }
        }
    }
    #[cfg(not(feature = "grokrxiv-publisher"))]
    {
        let _ = (state, pool, plan, body);
        Ok("skipped_no_publisher_feature".to_string())
    }
}

async fn approve(review_id: Uuid, force: bool, json: bool) -> anyhow::Result<()> {
    crate::cli_status::emit(format!(
        "review {review_id}: approving reviewed PR and publishing"
    ));
    publish_cmd(review_id, force, json).await
}

#[cfg(feature = "grokrxiv-publisher")]
async fn open_publication_pr_impl(
    state: &super::AppState,
    review_id: Uuid,
    force: bool,
    json: bool,
    emit_output: bool,
) -> anyhow::Result<String> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    // Read the review row + the joined paper for branch + field + arxiv_id.
    let row: (
        Uuid,
        String,
        String,
        Option<String>,
        Uuid,
        String,
        Option<String>,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "select r.id, p.arxiv_id, p.title, p.field, p.id, coalesce(r.visibility, 'public'), r.github_pr_url, \
                coalesce(p.source_kind, 'arxiv'), p.source_id \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let (_, arxiv_id, title, field, paper_id, visibility, existing_pr_url, source_kind, source_id) =
        row;
    let source_ref =
        crate::source_display::source_display_ref(&source_kind, source_id.as_deref(), &arxiv_id);
    let artifact_id = crate::source_display::source_artifact_id(source_id.as_deref(), &arxiv_id);
    if let Some(existing) = real_existing_pr_url(existing_pr_url.as_deref()) {
        if emit_output && json {
            println!(
                "{}",
                serde_json::json!({"review_id": review_id, "pr_url": existing, "status": "pr_open", "visibility": visibility, "idempotent": true})
            );
        } else if emit_output {
            println!("pr_url={existing}");
        }
        return Ok(existing.to_string());
    }

    // Phase 2: recommendation gate. Read meta_review.recommendation and bail
    // unless the operator passed --force. Missing recommendation is also a
    // bail — better to fail loudly than to publish an unverified row.
    let meta_review: Option<serde_json::Value> =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .unwrap_or(None);
    let meta_recommendation = meta_review
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(|v| v.as_str());
    let specialist_gate = crate::db::load_specialist_gate_for_review(pool, review_id).await?;
    let publication_gate =
        crate::review_gate::PublicationGate::evaluate(crate::review_gate::PublicationGateInput {
            recommendation: meta_recommendation,
            specialist_gate,
        });
    if publication_gate.verdict != crate::review_gate::GateVerdict::Pass && !force {
        let failure = crate::github_feedback::gate_failure_from_publication_gate(
            review_id,
            &publication_gate,
            meta_review.as_ref(),
        );
        let _ = crate::github_feedback::record_gate_failure(state, review_id, &failure).await;
        if let Some(pr_url) = existing_pr_url.as_deref() {
            if let Some(pr_number) = grokrxiv_publisher::parse_pr_number(pr_url) {
                let (owner, repo) = review_repo_for_visibility(&visibility);
                let body = crate::github_feedback::gate_failure_comment_body(
                    review_id,
                    &publication_gate.recommendation,
                    &failure,
                );
                match crate::github_feedback::post_or_update_gate_feedback_comment(
                    state,
                    &owner,
                    &repo,
                    pr_number as i64,
                    &format!("review-{review_id}"),
                    &body,
                )
                .await
                {
                    Ok(Some(comment)) => {
                        if let Ok(comment_id) = i64::try_from(comment.comment_id) {
                            let _ = crate::db::attach_gate_feedback_comment(
                                pool,
                                review_id,
                                comment_id,
                                &comment.html_url,
                            )
                            .await;
                            let _ = crate::db::upsert_github_review_thread(
                                pool,
                                review_id,
                                paper_id,
                                &owner,
                                &repo,
                                Some(pr_number as i64),
                                Some(pr_url),
                                None,
                                None,
                            )
                            .await;
                            let _ = crate::db::update_github_feedback_comment(
                                pool,
                                review_id,
                                comment_id,
                                &comment.html_url,
                            )
                            .await;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(%review_id, err = %e, "approve gate failure: GitHub feedback comment failed");
                    }
                }
            }
        }
        anyhow::bail!(
            "review {review_id} is not cleanly publishable: {} \
             Use `agh app run grokrxiv request-revisions {review_id}`, \
             `agh app run grokrxiv reject {review_id} --reason …`, \
             or re-run approve with `--force` to override.",
            publication_gate.reason
        );
    }
    if publication_gate.verdict != crate::review_gate::GateVerdict::Pass && force {
        tracing::warn!(
            %review_id,
            recommendation = %publication_gate.recommendation,
            reason = %publication_gate.reason,
            "approve --force: bypassing automated publication gate"
        );
    }
    match meta_recommendation {
        Some("accept" | "minor_revision" | "major_revision" | "reject") => {}
        Some(other) => {
            tracing::warn!(%review_id, recommendation = other, "approve: unknown recommendation value");
        }
        None => {}
    }

    // Attach rendered artifacts from the completed review run.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = crate::artifacts::review_artifact_dir(review_id);
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{artifact_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        artifact_id = artifact_id,
    );
    for name in ["review.html", "review.md", "review.tex", "bundle.zip"] {
        let path = dir_local.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            files.push((format!("{repo_prefix}/{name}"), bytes));
        } else {
            tracing::warn!(path = %path.display(), "approve: artifact missing, skipping");
        }
    }
    append_review_loop_pr_files(review_id, &repo_prefix, &mut files).await;
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts found under {} — \
             re-run `agh app run grokrxiv ingest <arxiv_id>` to regenerate.",
            crate::artifacts::review_artifact_ref(review_id)
        );
    }

    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; required to open publication PR"))?;

    let (owner, repo) = review_repo_for_visibility(&visibility);
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner, repo);

    let admin = AdminCaller::from_admin_endpoint();
    let raw_pr_title = format!("Review: {} ({})", title, source_ref);
    let raw_pr_body = if visibility == "private" {
        format!(
            "Opened by `agh app run grokrxiv review ...`.\n\n\
             **Automated gate:** Pass.\n\n\
             **Private review:** dashboard-only unless archived in the private reviews repo.\n\n\
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
        )
    } else {
        format!(
            "Opened by `agh app run grokrxiv review ...`.\n\n\
             **Automated gate:** Pass.\n\n\
             **Public page:** {public_url}/reviews/{review_id}\n\n\
             See linked artifacts in this PR; the rendered review.html is the human-readable preview.",
            public_url = std::env::var("GROKRXIV_PUBLIC_URL")
                .unwrap_or_else(|_| "https://grokrxiv.org".into()),
        )
    };

    // Phase I: codex (gpt-5.5) audits the PR title + body before the PR is
    // opened, scrubbing unexpanded \newcommand macros (e.g. \sysname) and
    // residual LaTeX layout commands so the PR list on grokrxiv-reviews
    // doesn't carry literal latex. Non-fatal — falls back to the raw
    // strings if codex is unavailable.
    let cleaned = if std::env::var("GROKRXIV_HTML_QUALITY_DISABLE")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        crate::html_review::CleanedPrText {
            title: raw_pr_title.clone(),
            body: raw_pr_body.clone(),
            fixes: serde_json::Value::Array(vec![]),
            summary: String::new(),
            confidence: 0.0,
        }
    } else {
        crate::html_review::clean_pr_text(state, review_id, &raw_pr_title, &raw_pr_body).await
    };

    let params = OpenReviewPr {
        arxiv_id: artifact_id.clone(),
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: cleaned.title,
        review_id,
        body_md: cleaned.body,
        correction_source_path: None,
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;

    // Persist transition.
    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = crate::db::set_review_github_pr_url(pool, review_id, &pr_url).await;

    // Keep at most one active review PR per paper.
    close_superseded_pr_if_any_cli(pool, &publisher, &admin, paper_id, &pr_url).await;

    // Phase 1: revalidate the public site so the "In Review" badge lands
    // immediately, instead of waiting on the merge webhook. Private reviews
    // never revalidate public pages.
    if visibility == "public" {
        crate::routes::webhook::spawn_revalidate(state, review_id);
    }

    if emit_output && json {
        println!(
            "{}",
            serde_json::json!({"review_id": review_id, "pr_url": pr_url, "status": "pr_open", "visibility": visibility})
        );
    } else if emit_output {
        println!("pr_url={pr_url}");
    }
    if emit_output {
        crate::cli_status::emit(format!(
            "review {review_id}: pr_open at {pr_url}; review and merge the PR manually to publish"
        ));
    }
    Ok(pr_url)
}

async fn request_revisions(review_id: Uuid, notes: Option<&str>, json: bool) -> anyhow::Result<()> {
    crate::cli_status::emit(format!(
        "review {review_id}: opening revision-needed PR for automated gate failure"
    ));
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let _ = request_revisions_impl(&state, review_id, notes, json, true).await?;
    Ok(())
}

#[cfg(feature = "grokrxiv-publisher")]
async fn request_revisions_impl(
    state: &super::AppState,
    review_id: Uuid,
    notes: Option<&str>,
    json: bool,
    emit_output: bool,
) -> anyhow::Result<String> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    let row: (
        Uuid,
        String,
        String,
        Option<String>,
        Uuid,
        String,
        Option<String>,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "select r.id, p.arxiv_id, p.title, p.field, p.id, coalesce(r.visibility, 'public'), r.github_pr_url, \
                coalesce(p.source_kind, 'arxiv'), p.source_id \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let (_, arxiv_id, title, field, paper_id, visibility, existing_pr_url, source_kind, source_id) =
        row;
    let source_ref =
        crate::source_display::source_display_ref(&source_kind, source_id.as_deref(), &arxiv_id);
    let artifact_id = crate::source_display::source_artifact_id(source_id.as_deref(), &arxiv_id);
    if let Some(existing) = real_existing_pr_url(existing_pr_url.as_deref()) {
        if emit_output && json {
            println!(
                "{}",
                serde_json::json!({"review_id": review_id, "pr_url": existing, "status": "pr_open", "visibility": visibility, "gate": "needs_revision", "idempotent": true})
            );
        } else if emit_output {
            println!("pr_url={existing}");
        }
        return Ok(existing.to_string());
    }

    let (meta_review, publication_gate, _) = load_publication_gate_context(pool, review_id).await?;
    let recommendation = publication_gate.recommendation.as_str();
    let failure = crate::github_feedback::gate_failure_from_publication_gate(
        review_id,
        &publication_gate,
        meta_review.as_ref(),
    );
    let _ = crate::github_feedback::record_gate_failure(state, review_id, &failure).await;

    let moderator = moderator_handle();
    let _ = crate::db::update_moderation_state(
        pool,
        review_id,
        "changes_requested",
        notes,
        Some(&moderator),
    )
    .await;

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let now = chrono::Utc::now();
    let dir_local = crate::artifacts::review_artifact_dir(review_id);
    let repo_prefix = format!(
        "reviews/{year}/{month:02}/{field}/{artifact_id}",
        year = now.format("%Y"),
        month = now.format("%m").to_string().parse::<u32>().unwrap_or(1),
        field = field.as_deref().unwrap_or("cs"),
        artifact_id = artifact_id,
    );
    for name in ["review.html", "review.md", "review.tex", "bundle.zip"] {
        let path = dir_local.join(name);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            files.push((format!("{repo_prefix}/{name}"), bytes));
        } else {
            tracing::warn!(path = %path.display(), "request-revisions: artifact missing, skipping");
        }
    }
    append_review_loop_pr_files(review_id, &repo_prefix, &mut files).await;
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts found under {} — \
             re-run `agh app run grokrxiv review ...` to regenerate.",
            crate::artifacts::review_artifact_ref(review_id)
        );
    }

    let correction_source =
        load_correction_source_snapshot(pool, paper_id, source_id.as_deref()).await?;
    if let Some(source) = correction_source.as_ref() {
        files.push((source.repo_path.clone(), source.bytes.clone()));
    }

    let public_url =
        std::env::var("GROKRXIV_PUBLIC_URL").unwrap_or_else(|_| "https://grokrxiv.org".into());
    let note_block = notes
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("\n\n**Moderator notes:**\n\n{s}"))
        .unwrap_or_default();
    let citation_block = citation_verifier_summary(pool, review_id)
        .await
        .map(|s| format!("\n\n{}", s.to_markdown()))
        .unwrap_or_default();
    let correction_instruction = if let Some(source) = correction_source.as_ref() {
        format!(
            "Edit the manuscript snapshot at `{}` on this PR branch, commit, and push.",
            source.repo_path
        )
    } else {
        "No editable manuscript snapshot was available in the extraction artifacts. Push a revised source/PDF change to this PR branch or rerun extraction with source artifacts enabled.".to_string()
    };
    let raw_pr_title = format!("Needs revision: {} ({})", title, source_ref);
    let raw_pr_body = format!(
        "Opened by `agh app run grokrxiv request-revisions {review_id}`.\n\n\
         **Automated gate:** Needs revision (`{recommendation}`).\n\n\
         **Public review details:** {public_url}/reviews/{review_id}\n\n\
         This review is not approved for publication yet. {correction_instruction} Each push triggers GrokRxiv automated re-review through the `pull_request.synchronize` webhook. GrokRxiv updates the stable gate comment with pass/fail status and concrete correction notes until automation accepts the fixes.{note_block}{citation_block}\n\n\
         {}",
         failure.summary,
    );

    let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
        anyhow::anyhow!("GITHUB_TOKEN not set; required to open revision-needed PR")
    })?;

    let (owner, repo) = review_repo_for_visibility(&visibility);
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, owner.clone(), repo.clone());
    let admin = AdminCaller::from_admin_endpoint();

    let cleaned = if std::env::var("GROKRXIV_HTML_QUALITY_DISABLE")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
    {
        crate::html_review::CleanedPrText {
            title: raw_pr_title.clone(),
            body: raw_pr_body.clone(),
            fixes: serde_json::Value::Array(vec![]),
            summary: String::new(),
            confidence: 0.0,
        }
    } else {
        crate::html_review::clean_pr_text(state, review_id, &raw_pr_title, &raw_pr_body).await
    };

    let params = OpenReviewPr {
        arxiv_id: artifact_id.clone(),
        field: field.unwrap_or_else(|| "cs".into()),
        date: chrono::Utc::now().date_naive(),
        files,
        title: cleaned.title,
        review_id,
        body_md: cleaned.body,
        correction_source_path: correction_source.as_ref().map(|s| s.repo_path.clone()),
    };
    let pr_url = publisher
        .open_review_pr(&admin, params)
        .await
        .map_err(|e| anyhow::anyhow!("open_review_pr: {e}"))?;
    let pr_number =
        grokrxiv_publisher::parse_pr_number(&pr_url).and_then(|n| i64::try_from(n).ok());

    let _ = crate::db::set_review_status(pool, review_id, ReviewStatus::PrOpen, None).await;
    let _ = crate::db::set_review_github_pr_url(pool, review_id, &pr_url).await;
    let _ = crate::db::upsert_github_review_thread(
        pool,
        review_id,
        paper_id,
        &owner,
        &repo,
        pr_number,
        Some(&pr_url),
        None,
        None,
    )
    .await;

    if let Some(pr_number) = pr_number {
        let body =
            crate::github_feedback::gate_failure_comment_body(review_id, recommendation, &failure);
        match crate::github_feedback::post_or_update_gate_feedback_comment(
            state,
            &owner,
            &repo,
            pr_number,
            &format!("review-{review_id}"),
            &body,
        )
        .await
        {
            Ok(Some(comment)) => {
                if let Ok(comment_id) = i64::try_from(comment.comment_id) {
                    let _ = crate::db::attach_gate_feedback_comment(
                        pool,
                        review_id,
                        comment_id,
                        &comment.html_url,
                    )
                    .await;
                    let _ = crate::db::update_github_feedback_comment(
                        pool,
                        review_id,
                        comment_id,
                        &comment.html_url,
                    )
                    .await;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(%review_id, err = %e, "request-revisions: GitHub feedback comment failed");
            }
        }
    }

    if let Some(existing) = existing_pr_url.as_deref() {
        if existing != pr_url {
            if let Some(old_pr_number) = grokrxiv_publisher::parse_pr_number(existing) {
                let comment = format!(
                    "Closed because review `{review_id}` was reopened as a revision-needed PR: {pr_url}"
                );
                if let Err(e) = publisher
                    .close_pr_with_comment(&admin, old_pr_number, &comment)
                    .await
                {
                    tracing::warn!(%review_id, %existing, err = %e, "request-revisions: failed to close superseded PR");
                }
            }
        }
    }

    if visibility == "public" {
        crate::routes::webhook::spawn_revalidate(state, review_id);
    }
    if emit_output && json {
        println!(
            "{}",
            serde_json::json!({"review_id": review_id, "pr_url": pr_url, "status": "pr_open", "visibility": visibility, "gate": "needs_revision"})
        );
    } else if emit_output {
        println!("pr_url={pr_url}");
    }
    if emit_output {
        crate::cli_status::emit(format!(
            "review {review_id}: revision-needed PR open at {pr_url}; author pushes trigger automated re-review"
        ));
    }
    Ok(pr_url)
}

#[derive(Debug, Clone)]
struct CorrectionSourceSnapshot {
    repo_path: String,
    bytes: Vec<u8>,
}

async fn load_correction_source_snapshot(
    pool: &sqlx::PgPool,
    paper_id: Uuid,
    source_id: Option<&str>,
) -> anyhow::Result<Option<CorrectionSourceSnapshot>> {
    let row: Option<(
        String,
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
    )> = sqlx::query_as(
        "select coalesce(source_kind, 'arxiv'), arxiv_id, source_uri, source_id, source_metadata \
             from papers where id = $1",
    )
    .bind(paper_id)
    .fetch_optional(pool)
    .await?;
    let Some((source_kind, arxiv_id, source_uri, row_source_id, source_metadata)) = row else {
        return Ok(None);
    };
    let stable_source_id = source_id
        .map(str::to_owned)
        .or(row_source_id)
        .unwrap_or_else(|| arxiv_id.clone());
    let adapter = source_metadata
        .get("adapter")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    match source_kind.as_str() {
        "git_repo" => {
            let repo = adapter
                .get("repo")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("git source metadata missing adapter.repo"))?;
            let paper_path = adapter
                .get("paper_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("git source metadata missing adapter.paper_path"))?;
            let paper_path = PathBuf::from(paper_path);
            ensure_relative_inside_repo(&paper_path)?;
            let rev = adapter
                .get("resolved_commit")
                .and_then(|v| v.as_str())
                .or_else(|| adapter.get("rev").and_then(|v| v.as_str()));
            let tmp = tempfile::TempDir::new().context("create temp dir for correction source")?;
            let checkout = tmp.path().join("repo");
            run_git_for_correction(["clone", "--quiet", repo, path_to_str(&checkout)?], None)
                .await
                .with_context(|| format!("clone correction source {repo}"))?;
            if let Some(rev) = rev.filter(|s| !s.trim().is_empty()) {
                run_git_for_correction(["checkout", "--quiet", rev], Some(&checkout))
                    .await
                    .with_context(|| format!("checkout correction source revision {rev}"))?;
            }
            let source_file = checkout.join(&paper_path);
            let bytes = tokio::fs::read(&source_file)
                .await
                .with_context(|| format!("read correction source {}", paper_path.display()))?;
            Ok(Some(CorrectionSourceSnapshot {
                repo_path: correction_repo_path(&stable_source_id, &paper_path),
                bytes,
            }))
        }
        "local_file" => {
            let path = adapter
                .get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .or_else(|| {
                    source_uri
                        .as_deref()
                        .and_then(|uri| uri.strip_prefix("file://"))
                        .map(PathBuf::from)
                });
            let Some(path) = path else {
                return Ok(None);
            };
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read local correction source {}", path.display()))?;
            Ok(Some(CorrectionSourceSnapshot {
                repo_path: correction_repo_path(&stable_source_id, &path),
                bytes,
            }))
        }
        "arxiv" => load_arxiv_correction_source_snapshot(&arxiv_id, &stable_source_id).await,
        _ => Ok(None),
    }
}

#[cfg(feature = "grokrxiv-ingest")]
async fn load_arxiv_correction_source_snapshot(
    arxiv_id: &str,
    stable_source_id: &str,
) -> anyhow::Result<Option<CorrectionSourceSnapshot>> {
    let staged = grokrxiv_ingest::ingest_staged(arxiv_id)
        .await
        .with_context(|| format!("fetch arXiv correction source {arxiv_id}"))?;
    if let Some(source_tarball) = staged.source_tarball.as_ref() {
        match grokrxiv_ingest::extract_main_tex_source(source_tarball) {
            Ok(main) => {
                return Ok(Some(CorrectionSourceSnapshot {
                    repo_path: correction_repo_path(stable_source_id, Path::new(&main.path)),
                    bytes: main.contents.into_bytes(),
                }));
            }
            Err(e) => {
                tracing::warn!(%arxiv_id, err = %e, "arXiv source bundle did not yield editable main TeX; falling back to PDF if available");
            }
        }
    }
    if let Some(pdf_bytes) = staged.pdf_bytes {
        return Ok(Some(CorrectionSourceSnapshot {
            repo_path: correction_repo_path(stable_source_id, Path::new("paper.pdf")),
            bytes: pdf_bytes.to_vec(),
        }));
    }
    Ok(None)
}

#[cfg(not(feature = "grokrxiv-ingest"))]
async fn load_arxiv_correction_source_snapshot(
    _arxiv_id: &str,
    _stable_source_id: &str,
) -> anyhow::Result<Option<CorrectionSourceSnapshot>> {
    Ok(None)
}

async fn run_git_for_correction<'a, I>(args: I, cwd: Option<&Path>) -> anyhow::Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd.output().await.context("spawn git")?;
    if !output.status.success() {
        anyhow::bail!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn path_to_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn ensure_relative_inside_repo(path: &Path) -> anyhow::Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("git paper_path must be a relative path inside the repository");
    }
    Ok(())
}

fn correction_repo_path(source_id: &str, source_path: &Path) -> String {
    let safe_source_id: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let file_name = source_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("paper.tex");
    format!("corrections/{safe_source_id}/{file_name}")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrView {
    body: Option<String>,
    url: String,
    head_ref_name: String,
    head_repository: GhRepository,
    head_repository_owner: GhOwner,
    comments: Vec<GhComment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRepository {
    name: Option<String>,
    name_with_owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhComment {
    body: String,
    url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SmokeEditPlan {
    edit_path: PathBuf,
    git_add_paths: Vec<PathBuf>,
    payload: String,
}

fn pr_body_needs_revision_refresh(body: &str) -> bool {
    extract_correction_source_marker(body).is_none()
}

fn smoke_edit_plan(correction_path: &str) -> anyhow::Result<SmokeEditPlan> {
    ensure_safe_relative_marker(correction_path)?;
    let correction = PathBuf::from(correction_path);
    let timestamp = chrono::Utc::now().to_rfc3339();
    let extension = correction
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension == "pdf" {
        let sidecar = correction
            .parent()
            .map(|parent| parent.join("grokrxiv-smoke-trigger.md"))
            .unwrap_or_else(|| PathBuf::from("grokrxiv-smoke-trigger.md"));
        return Ok(SmokeEditPlan {
            edit_path: sidecar.clone(),
            git_add_paths: vec![sidecar],
            payload: format!(
                "\n- {timestamp}: GrokRxiv feedback-loop smoke trigger for PDF-backed correction PR `{correction_path}`.\n"
            ),
        });
    }
    Ok(SmokeEditPlan {
        edit_path: correction.clone(),
        git_add_paths: vec![correction],
        payload: format!("\n% GrokRxiv feedback-loop smoke correction {timestamp}\n"),
    })
}

async fn feedback_loop_smoke(
    review_id: Uuid,
    max_wait_secs: u64,
    json: bool,
) -> anyhow::Result<()> {
    crate::config::load_env()?;
    if std::env::var("GROKRXIV_E2E_ALLOW_GITHUB_PUSH").as_deref() != Ok("1") {
        anyhow::bail!(
            "feedback-loop-smoke refuses to push unless GROKRXIV_E2E_ALLOW_GITHUB_PUSH=1"
        );
    }
    for key in ["GITHUB_TOKEN", "GITHUB_WEBHOOK_SECRET", "DATABASE_URL"] {
        if std::env::var(key)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_none()
        {
            anyhow::bail!("{key} must be set for feedback-loop-smoke");
        }
    }

    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    let mut thread = crate::db::fetch_feedback_loop_thread(pool, review_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("review {review_id} not found"))?;

    let mut pr_url = thread
        .github_pr_url
        .as_deref()
        .filter(|url| !url.contains("SIMULATED"))
        .map(str::to_owned);
    if pr_url.is_none() {
        pr_url = Some(request_revisions_impl(&state, review_id, None, false, false).await?);
        thread = crate::db::fetch_feedback_loop_thread(pool, review_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("review {review_id} disappeared after request-revisions")
            })?;
    }
    let pr_url = pr_url.ok_or_else(|| anyhow::anyhow!("review {review_id} has no real PR URL"))?;
    let (mut owner, mut repo, mut pr_number) = parse_github_pr_url(&pr_url)
        .or_else(|| {
            Some((
                thread.repo_owner.clone()?,
                thread.repo_name.clone()?,
                u64::try_from(thread.pr_number?).ok()?,
            ))
        })
        .ok_or_else(|| anyhow::anyhow!("github_pr_url is not a GitHub PR URL: {pr_url}"))?;
    let mut pr_info = gh_pr_view(&owner, &repo, pr_number).await?;
    if pr_info
        .body
        .as_deref()
        .map(pr_body_needs_revision_refresh)
        .unwrap_or(true)
    {
        crate::cli_status::emit(format!(
            "review {review_id}: existing PR lacks correction source marker; refreshing revision PR"
        ));
        let refreshed_pr_url =
            request_revisions_impl(&state, review_id, None, false, false).await?;
        thread = crate::db::fetch_feedback_loop_thread(pool, review_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("review {review_id} disappeared after revision PR refresh")
            })?;
        (owner, repo, pr_number) = parse_github_pr_url(&refreshed_pr_url)
            .or_else(|| {
                Some((
                    thread.repo_owner.clone()?,
                    thread.repo_name.clone()?,
                    u64::try_from(thread.pr_number?).ok()?,
                ))
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "refreshed github_pr_url is not a GitHub PR URL: {refreshed_pr_url}"
                )
            })?;
        pr_info = gh_pr_view(&owner, &repo, pr_number).await?;
    }
    let correction_path = pr_info
        .body
        .as_deref()
        .and_then(extract_correction_source_marker)
        .ok_or_else(|| {
            anyhow::anyhow!("PR body is missing grokrxiv-correction-source-path marker")
        })?;
    ensure_safe_relative_marker(correction_path)?;

    let head_repo = pr_info
        .head_repository
        .name_with_owner
        .clone()
        .or_else(|| {
            pr_info
                .head_repository
                .name
                .as_ref()
                .map(|name| format!("{}/{}", pr_info.head_repository_owner.login, name))
        })
        .ok_or_else(|| anyhow::anyhow!("PR head repository is missing from gh output"))?;
    if pr_info.head_ref_name.trim().is_empty() {
        anyhow::bail!("PR head branch is missing from gh output");
    }

    let tmp = tempfile::TempDir::new().context("create feedback-loop smoke checkout")?;
    let checkout = tmp.path().join("checkout");
    run_process(
        "gh",
        vec![
            "repo".into(),
            "clone".into(),
            head_repo.clone(),
            path_to_str(&checkout)?.into(),
        ],
        None,
    )
    .await
    .with_context(|| format!("clone PR head repository {head_repo}"))?;
    run_process(
        "git",
        vec!["checkout".into(), pr_info.head_ref_name.clone()],
        Some(&checkout),
    )
    .await
    .with_context(|| format!("checkout PR branch {}", pr_info.head_ref_name))?;

    let correction_file = checkout.join(correction_path);
    if !correction_file.starts_with(&checkout) {
        anyhow::bail!("correction source path escapes checkout");
    }
    let smoke_plan = smoke_edit_plan(correction_path)?;
    let edit_file = checkout.join(&smoke_plan.edit_path);
    if !edit_file.starts_with(&checkout) {
        anyhow::bail!("smoke edit path escapes checkout");
    }
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&edit_file)
        .await
        .with_context(|| format!("open smoke edit {}", smoke_plan.edit_path.display()))?
        .write_all(smoke_plan.payload.as_bytes())
        .await
        .with_context(|| format!("write smoke edit {}", smoke_plan.edit_path.display()))?;
    run_process(
        "git",
        vec!["config".into(), "user.name".into(), "GrokRxiv Smoke".into()],
        Some(&checkout),
    )
    .await?;
    run_process(
        "git",
        vec![
            "config".into(),
            "user.email".into(),
            "smoke@grokrxiv.local".into(),
        ],
        Some(&checkout),
    )
    .await?;
    for path in &smoke_plan.git_add_paths {
        run_process(
            "git",
            vec!["add".into(), path.to_string_lossy().to_string()],
            Some(&checkout),
        )
        .await?;
    }
    run_process(
        "git",
        vec![
            "commit".into(),
            "-m".into(),
            "test: trigger GrokRxiv feedback-loop smoke".into(),
        ],
        Some(&checkout),
    )
    .await?;
    run_process(
        "git",
        vec![
            "push".into(),
            "origin".into(),
            format!("HEAD:{}", pr_info.head_ref_name),
        ],
        Some(&checkout),
    )
    .await
    .with_context(|| format!("push smoke commit to {}", pr_info.head_ref_name))?;
    let commit_sha = run_process(
        "git",
        vec!["rev-parse".into(), "HEAD".into()],
        Some(&checkout),
    )
    .await?
    .trim()
    .to_string();

    let request = poll_rereview_request(pool, review_id, &commit_sha, max_wait_secs).await?;
    let new_review_id = request
        .new_review_id
        .ok_or_else(|| anyhow::anyhow!("re-review finished without new_review_id"))?;
    let marker = format!("<!-- grokrxiv:gate-feedback:review-{review_id} -->");
    let gate_comment = gh_find_gate_comment(&owner, &repo, pr_number, &marker).await?;
    let gate = load_publication_gate_for_review_output(pool, new_review_id).await?;

    let output = serde_json::json!({
        "prior_review_id": review_id,
        "new_review_id": new_review_id,
        "paper_id": thread.paper_id,
        "request_id": request.id,
        "pr_url": pr_info.url,
        "commit_sha": commit_sha,
        "gate_verdict": gate.verdict,
        "recommendation": gate.recommendation,
        "gate_reason": gate.reason,
        "gate_comment_url": gate_comment.url.or(thread.feedback_comment_url),
    });
    if json {
        println!("{}", output);
    } else {
        println!("prior_review_id={review_id}");
        println!("new_review_id={new_review_id}");
        println!("request_id={}", request.id);
        println!("pr_url={}", pr_info.url);
        println!("commit_sha={commit_sha}");
        println!("gate_verdict={:?}", gate.verdict);
        println!("recommendation={}", gate.recommendation);
        if let Some(url) = output.get("gate_comment_url").and_then(|v| v.as_str()) {
            println!("gate_comment_url={url}");
        }
    }
    Ok(())
}

async fn load_publication_gate_for_review_output(
    pool: &sqlx::PgPool,
    review_id: Uuid,
) -> anyhow::Result<crate::review_gate::PublicationGate> {
    let meta: Option<serde_json::Value> =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .unwrap_or(None);
    let recommendation = meta
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(serde_json::Value::as_str);
    let specialist_gate = crate::db::load_specialist_gate_for_review(pool, review_id).await?;
    Ok(crate::review_gate::PublicationGate::evaluate(
        crate::review_gate::PublicationGateInput {
            recommendation,
            specialist_gate,
        },
    ))
}

async fn poll_rereview_request(
    pool: &sqlx::PgPool,
    prior_review_id: Uuid,
    commit_sha: &str,
    max_wait_secs: u64,
) -> anyhow::Result<crate::db::RereviewRequestStatus> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(max_wait_secs);
    loop {
        if let Some(row) =
            crate::db::fetch_rereview_request_for_commit(pool, prior_review_id, commit_sha).await?
        {
            match row.state.as_str() {
                "done" => return Ok(row),
                "failed" => {
                    anyhow::bail!(
                        "feedback-loop re-review failed: {}",
                        row.error.as_deref().unwrap_or("no error recorded")
                    );
                }
                _ => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for re-review request for commit {commit_sha}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

async fn gh_pr_view(owner: &str, repo: &str, pr_number: u64) -> anyhow::Result<GhPrView> {
    let stdout = run_process(
        "gh",
        vec![
            "pr".into(),
            "view".into(),
            pr_number.to_string(),
            "--repo".into(),
            format!("{owner}/{repo}"),
            "--json".into(),
            "body,comments,headRefName,headRepository,headRepositoryOwner,url".into(),
        ],
        None,
    )
    .await?;
    serde_json::from_str(&stdout).context("parse gh pr view JSON")
}

async fn gh_find_gate_comment(
    owner: &str,
    repo: &str,
    pr_number: u64,
    marker: &str,
) -> anyhow::Result<GhComment> {
    let pr = gh_pr_view(owner, repo, pr_number).await?;
    let matches: Vec<GhComment> = pr
        .comments
        .into_iter()
        .filter(|comment| comment.body.contains(marker))
        .collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => anyhow::bail!("GitHub PR has no gate feedback comment with marker {marker}"),
        n => anyhow::bail!("GitHub PR has {n} gate feedback comments with marker {marker}"),
    }
}

async fn run_process(
    program: &str,
    args: Vec<String>,
    cwd: Option<&Path>,
) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd
        .output()
        .await
        .with_context(|| format!("spawn {program}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_github_pr_url(url: &str) -> Option<(String, String, u64)> {
    let path = url.strip_prefix("https://github.com/")?;
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pull" {
        return None;
    }
    let number = parts
        .next()?
        .split(|c| matches!(c, '?' | '#' | '/'))
        .next()?
        .parse()
        .ok()?;
    Some((owner, repo, number))
}

fn real_existing_pr_url(url: Option<&str>) -> Option<&str> {
    url.filter(|value| !value.contains("SIMULATED-") && parse_github_pr_url(value).is_some())
}

fn extract_correction_source_marker(body: &str) -> Option<&str> {
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix("grokrxiv-correction-source-path:") {
            let path = rest.trim();
            if !path.is_empty() && ensure_safe_relative_marker(path).is_ok() {
                return Some(path);
            }
        }
    }
    None
}

fn ensure_safe_relative_marker(path: &str) -> anyhow::Result<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("correction source path must be relative and stay inside the PR branch");
    }
    Ok(())
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn request_revisions_impl(
    _state: &super::AppState,
    review_id: Uuid,
    _notes: Option<&str>,
    _json: bool,
    _emit_output: bool,
) -> anyhow::Result<String> {
    anyhow::bail!(
        "request-revisions <{review_id}> requires --features full (grokrxiv-publisher) at build time."
    )
}

/// Local copy of supervisor::close_superseded_pr_if_any. Lives here so the
/// `agh app run grokrxiv approve` command (which doesn't go through the supervisor
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

fn review_repo_for_visibility(visibility: &str) -> (String, String) {
    match visibility {
        "private" => repo_from_combined_env(
            "GROKRXIV_PRIVATE_REVIEWS_REPO",
            "GrokRxiv",
            "grokrxiv-private-reviews",
        ),
        _ => {
            if let Some(repo) = repo_from_combined_env_optional("GROKRXIV_PUBLIC_REVIEWS_REPO") {
                repo
            } else {
                repo_from_legacy_public_env()
            }
        }
    }
}

fn repo_from_legacy_public_env() -> (String, String) {
    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo_raw =
        std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "grokrxiv-reviews".into());
    split_owner_repo(&repo_raw)
        .map(|(o, r)| (o, r))
        .unwrap_or((owner, repo_raw))
}

fn repo_from_combined_env(var: &str, default_owner: &str, default_repo: &str) -> (String, String) {
    repo_from_combined_env_optional(var)
        .unwrap_or_else(|| (default_owner.to_string(), default_repo.to_string()))
}

fn repo_from_combined_env_optional(var: &str) -> Option<(String, String)> {
    let raw = std::env::var(var).ok()?;
    split_owner_repo(&raw)
}

fn split_owner_repo(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let (owner, repo) = trimmed.split_once('/')?;
    let owner = owner.trim();
    let repo = repo.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn open_publication_pr_impl(
    _state: &super::AppState,
    review_id: Uuid,
    _force: bool,
    _json: bool,
    _emit_output: bool,
) -> anyhow::Result<String> {
    anyhow::bail!(
        "opening a review PR for <{review_id}> requires --features full (grokrxiv-publisher) at build time."
    )
}

#[cfg(feature = "grokrxiv-publisher")]
async fn publish_cmd(review_id: Uuid, force: bool, json: bool) -> anyhow::Result<()> {
    crate::cli_status::emit(format!(
        "review {review_id}: merging reviewed PR and publishing web output"
    ));
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    let pr_url: Option<String> =
        sqlx::query_scalar("select github_pr_url from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let pr_url = pr_url.ok_or_else(|| {
        anyhow::anyhow!(
            "review {review_id} has no github_pr_url; run `agh app run grokrxiv review ...` first"
        )
    })?;

    let meta_review: Option<serde_json::Value> =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .unwrap_or(None);
    let recommendation = meta_review
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(|v| v.as_str());
    let specialist_gate = crate::db::load_specialist_gate_for_review(pool, review_id).await?;
    let publication_gate =
        crate::review_gate::PublicationGate::evaluate(crate::review_gate::PublicationGateInput {
            recommendation,
            specialist_gate,
        });
    if publication_gate.verdict != crate::review_gate::GateVerdict::Pass && !force {
        anyhow::bail!(
            "approve refused: latest automated gate for review {review_id} is not pass: {} \
             Push fixes to the PR and wait for re-review, or run `agh app run grokrxiv approve {review_id} --force`.",
            publication_gate.reason
        );
    }
    if publication_gate.verdict != crate::review_gate::GateVerdict::Pass {
        tracing::warn!(
            %review_id,
            reason = %publication_gate.reason,
            "approve --force: bypassing latest automated gate"
        );
    }

    let (owner, repo, pr_number) = parse_github_pr_url(&pr_url).ok_or_else(|| {
        anyhow::anyhow!(
            "github_pr_url is not a real PR ({pr_url}); was this a simulated approve? \
             Re-run `agh app run grokrxiv review ...` with GITHUB_TOKEN set."
        )
    })?;

    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; required to merge"))?;
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let resp = client
        .pulls(owner, repo)
        .merge(pr_number)
        .method(octocrab::params::pulls::MergeMethod::Squash)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("merge PR #{pr_number}: {e}"))?;
    let published_finalized = if resp.merged {
        crate::routes::webhook::finalize_published_review(&state, review_id).await?
    } else {
        false
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "review_id": review_id,
                "pr_number": pr_number,
                "merged": resp.merged,
                "sha": resp.sha,
                "message": resp.message,
                "published_finalized": published_finalized,
            })
        );
    } else {
        println!(
            "pr_number={pr_number} merged={} sha={}",
            resp.merged,
            resp.sha.as_deref().unwrap_or("<none>")
        );
    }
    crate::cli_status::emit(format!(
        "review {review_id}: merged PR #{pr_number}; published_finalized={published_finalized}"
    ));
    Ok(())
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn publish_cmd(review_id: Uuid, _force: bool, _json: bool) -> anyhow::Result<()> {
    anyhow::bail!(
        "approve <{review_id}> requires --features full (grokrxiv-publisher) at build time."
    )
}

/// `agh app run grokrxiv html-review [<id>|--all]`. Re-runs the post-render html_quality
/// harness on already-rendered reviews. Used to backfill existing reviews
/// after the harness lands, or to re-run after prompt iteration.
async fn html_review_cmd(review_id: Option<Uuid>, all: bool, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("html-review: DATABASE_URL not configured"))?;

    let ids: Vec<Uuid> = if all {
        if review_id.is_some() {
            anyhow::bail!("html-review: pass either <review_id> OR --all, not both");
        }
        sqlx::query_scalar::<_, Uuid>(
            "select id from reviews where status in ('pr_open','published','corrected') order by created_at",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| anyhow::anyhow!("html-review: load review ids: {e}"))?
    } else {
        let id = review_id.ok_or_else(|| {
            anyhow::anyhow!("html-review: REVIEW_ID required unless --all is set")
        })?;
        vec![id]
    };

    let mut summaries: Vec<serde_json::Value> = Vec::new();
    for id in &ids {
        let dir = crate::artifacts::review_artifact_dir(*id);
        if !dir.exists() {
            tracing::warn!(review_id = %id, "html-review: artifact dir missing, skipping");
            summaries.push(serde_json::json!({
                "review_id": id,
                "ok": false,
                "reason": "artifacts directory missing"
            }));
            continue;
        }
        match crate::html_review::review_and_fix_html(&state, *id, &dir).await {
            Ok(ran) => {
                summaries.push(serde_json::json!({"review_id": id, "ok": true, "ran": ran}));
                if !json {
                    println!("review_id={id} ok ran={ran}");
                }
            }
            Err(e) => {
                summaries.push(serde_json::json!({
                    "review_id": id,
                    "ok": false,
                    "reason": format!("{e:#}")
                }));
                if !json {
                    eprintln!("review_id={id} ERROR: {e:#}");
                }
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string(&summaries)?);
    } else {
        println!("processed {} review(s)", ids.len());
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ReviewPrClosePlan {
    pr_url: String,
    owner: String,
    repo: String,
    pr_number: u64,
}

fn review_pr_close_plan(github_pr_url: Option<&str>) -> anyhow::Result<Option<ReviewPrClosePlan>> {
    let Some(pr_url) = github_pr_url else {
        return Ok(None);
    };
    if pr_url.contains("SIMULATED-") {
        return Ok(None);
    }
    let Some((owner, repo, pr_number)) = parse_github_pr_url(pr_url) else {
        anyhow::bail!("github_pr_url is not a GitHub PR URL: {pr_url}");
    };
    Ok(Some(ReviewPrClosePlan {
        pr_url: pr_url.to_string(),
        owner,
        repo,
        pr_number,
    }))
}

async fn close_review(
    review_id: Uuid,
    reason: &str,
    keep_github_pr: bool,
    json: bool,
) -> anyhow::Result<()> {
    use grokrxiv_schemas::ReviewStatus;
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("close: DATABASE_URL not configured"))?;

    let row: Option<(String, Option<String>)> =
        sqlx::query_as("select status, github_pr_url from reviews where id = $1")
            .bind(review_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| anyhow::anyhow!("close: review lookup failed: {e}"))?;
    let Some((previous_status, github_pr_url)) = row else {
        anyhow::bail!("close: no review row for {review_id}");
    };

    let close_plan = if keep_github_pr {
        None
    } else {
        review_pr_close_plan(github_pr_url.as_deref())?
    };
    if let Some(plan) = close_plan.as_ref() {
        close_review_github_pr(plan, review_id, reason).await?;
    }

    if previous_status != "withdrawn" {
        let moderator = moderator_handle();
        crate::db::insert_correction(pool, review_id, "withdrawal", reason, &moderator).await?;
        let n =
            crate::db::set_review_status(pool, review_id, ReviewStatus::Withdrawn, None).await?;
        if n == 0 {
            anyhow::bail!("close: failed to transition review {review_id} to status=withdrawn");
        }
    }

    revalidate_best_effort(&state, review_id).await;
    let github_pr_action = if keep_github_pr {
        "kept"
    } else if close_plan.is_some() {
        "closed"
    } else if github_pr_url.is_some() {
        "skipped"
    } else {
        "none"
    };

    if json {
        println!(
            "{}",
            serde_json::json!({
                "review_id": review_id,
                "previous_status": previous_status,
                "status": "withdrawn",
                "github_pr_url": github_pr_url,
                "github_pr_action": github_pr_action,
            })
        );
    } else {
        println!(
            "closed={review_id} previous_status={previous_status} status=withdrawn github_pr={github_pr_action}"
        );
    }
    Ok(())
}

#[cfg(feature = "grokrxiv-publisher")]
async fn close_review_github_pr(
    plan: &ReviewPrClosePlan,
    review_id: Uuid,
    reason: &str,
) -> anyhow::Result<()> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher};

    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; required to close GitHub PR"))?;
    let client = octocrab::OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .map_err(|e| anyhow::anyhow!("octocrab build: {e}"))?;
    let publisher = GithubPublisher::new(client, plan.owner.clone(), plan.repo.clone());
    let admin = AdminCaller::from_admin_endpoint();
    let comment =
        format!("Closed by `agh app run grokrxiv close {review_id}`.\n\nReason:\n\n{reason}");
    publisher
        .close_pr_with_comment(&admin, plan.pr_number, &comment)
        .await
        .map_err(|e| anyhow::anyhow!("close GitHub PR {}: {e:#}", plan.pr_url))?;
    Ok(())
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn close_review_github_pr(
    _plan: &ReviewPrClosePlan,
    review_id: Uuid,
    _reason: &str,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "close <{review_id}> requires --features full (grokrxiv-publisher) to close GitHub PR"
    )
}

/// `agh app run grokrxiv reject <REVIEW_ID> --reason TEXT`. Phase 4: rejection is a
/// public terminal state. Writes `moderation_queue` like before but ALSO:
///   - inserts a `rejections` row with the reason as `rationale_md`,
///   - flips `reviews.status` to `rejected`,
///   - revalidates the public site so the red "Rejected" badge lands.
async fn reject(review_id: Uuid, reason: &str) -> anyhow::Result<()> {
    use grokrxiv_schemas::ReviewStatus;
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

    sqlx::query("insert into rejections (review_id, rationale_md, created_by) values ($1, $2, $3)")
        .bind(review_id)
        .bind(reason)
        .bind(&moderator)
        .execute(pool)
        .await
        .map_err(|e| anyhow::anyhow!("reject: insert rejections row: {e}"))?;

    let rows_updated =
        crate::db::set_review_status(pool, review_id, ReviewStatus::Rejected, None).await?;
    if rows_updated == 0 {
        anyhow::bail!("reject: failed to transition review {review_id} to status=rejected");
    }

    crate::routes::webhook::spawn_revalidate(&state, review_id);
    println!("rejected={review_id}");
    Ok(())
}

/// Phase 5: open the GitHub human-review PR after a review reaches
/// `awaiting_moderation`. Clean gates open the publication PR; warn/fail gates
/// open the revision-needed PR with the stable automated feedback comment.
async fn auto_moderate_review(
    state: &super::AppState,
    review_id: Uuid,
    json: bool,
) -> anyhow::Result<()> {
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("auto-moderate: DATABASE_URL not configured"))?;
    let meta: Option<serde_json::Value> =
        sqlx::query_scalar("select meta_review from reviews where id = $1")
            .bind(review_id)
            .fetch_one(pool)
            .await
            .ok();
    let recommendation = meta
        .as_ref()
        .and_then(|m| m.get("recommendation"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    tracing::info!(
        target: "auto_moderate",
        %review_id,
        recommendation,
        "auto-moderate dispatch"
    );
    let _ = open_review_pr_for_gate(state, review_id, json, true).await?;
    Ok(())
}

/// `agh app run grokrxiv request-changes <REVIEW_ID> --notes TEXT`. Phase 3: record the
/// moderator's notes in `moderation_queue.notes`, then trigger a fresh
/// review of the same paper. The agents see the notes via
/// `db::fetch_latest_changes_request_notes` on the next pass.
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

    // Look up the paper this review belongs to, then trigger a fresh review.
    // The new pass calls fetch_latest_changes_request_notes(paper_id), which
    // returns the notes we just wrote, and threads them into specialist prompts.
    let paper_id: Uuid = sqlx::query_scalar("select paper_id from reviews where id = $1")
        .bind(review_id)
        .fetch_one(pool)
        .await
        .map_err(|e| anyhow::anyhow!("request-changes: paper lookup failed: {e}"))?;

    crate::cli_status::emit(format!(
        "review {review_id}: notes recorded; running fresh review pass for paper {paper_id}"
    ));
    #[cfg(feature = "grokrxiv-ingest")]
    {
        let new_review_id =
            super::supervisor::run_review_for_paper_blocking(&state, paper_id).await?;
        println!("request-changes={review_id} new_review_id={new_review_id} paper_id={paper_id}");
    }
    #[cfg(not(feature = "grokrxiv-ingest"))]
    {
        println!("request-changes={review_id} paper_id={paper_id} (re-review skipped: build without grokrxiv-ingest feature)");
    }
    Ok(())
}

/// `agh app run grokrxiv withdraw <REVIEW_ID> --reason TEXT`. Inserts a withdrawal row in
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

/// `agh app run grokrxiv correct <REVIEW_ID> --rationale-md PATH`. Reads the markdown
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
    std::env::var("AGENTHERO_MODERATOR")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_display::source_display_ref;
    use clap::{CommandFactory, Parser};
    use std::sync::{Mutex, MutexGuard};

    static CLI_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        fn clear(key: &'static str) -> Self {
            let lock = CLI_ENV_LOCK.lock().expect("cli env lock");
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn bare_agenthero_prints_help_instead_of_serving() {
        let err = Cli::try_parse_from(["agh"]).expect_err("bare CLI should print help");
        let text = err.to_string();
        assert!(text.contains("Usage: agh"));
        assert!(text.contains("Commands:"));
        assert!(text.contains("app"));
        assert!(text.contains("serve"));
    }

    #[test]
    fn default_help_shows_app_runtime_surface_only() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();

        for visible in ["app", "serve", "doctor", "config", "dag", "agent", "jobs"] {
            assert!(
                help.contains(visible),
                "expected `{visible}` in help:\n{help}"
            );
        }

        for hidden in [
            "review",
            "extract",
            "review-extracted",
            "approve",
            "request-revisions",
            "request-changes",
            "reject",
            "publish",
            "merge",
            "batch",
            "list",
            "show",
            "close",
            "open",
            "tail-jobs",
            "html-review",
            "feedback-loop-smoke",
            "migrate",
            "ingest-range",
            "ingest-daily",
            "grokrxiv",
            "c2rust",
        ] {
            assert!(
                !help.contains(&format!("\n  {hidden}")),
                "did not expect `{hidden}` in default help:\n{help}"
            );
        }
    }

    #[test]
    fn data_repo_root_requires_explicit_env() {
        let _env = EnvVarGuard::clear("GROKRXIV_DATA_REPO_PATH");
        let err = data_repo_root().expect_err("missing data repo path should fail");
        assert!(err
            .to_string()
            .contains("GROKRXIV_DATA_REPO_PATH is required"));

        std::env::set_var("GROKRXIV_DATA_REPO_PATH", "/tmp/grokrxiv-data");
        assert_eq!(
            data_repo_root().expect("configured data repo path"),
            PathBuf::from("/tmp/grokrxiv-data")
        );
    }

    #[test]
    fn legacy_unscoped_lifecycle_commands_are_not_root_commands() {
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();

        for args in [
            vec!["agh", "extract", "2605.00561"],
            vec!["agh", "ingest", "2605.00561"],
            vec!["agh", "ingest-daily"],
            vec![
                "agh",
                "ingest-range",
                "--from",
                "2026-05-01",
                "--to",
                "2026-05-02",
            ],
            vec!["agh", "review", "2605.00561"],
            vec!["agh", "review-extracted", "2605.00561"],
            vec!["agh", "approve", &review_id.to_string()],
            vec!["agh", "request-revisions", &review_id.to_string()],
            vec![
                "agh",
                "request-changes",
                &review_id.to_string(),
                "--notes",
                "fix citation evidence",
            ],
            vec![
                "agh",
                "reject",
                &review_id.to_string(),
                "--reason",
                "duplicate",
            ],
            vec!["agh", "publish", &review_id.to_string()],
            vec!["agh", "merge", &review_id.to_string()],
            vec!["agh", "show", &review_id.to_string()],
            vec!["agh", "open", &review_id.to_string()],
            vec!["agh", "list", "reviews"],
            vec!["agh", "batch", "list"],
            vec!["agh", "tail-jobs"],
            vec![
                "agh",
                "close",
                &review_id.to_string(),
                "--reason",
                "superseded",
            ],
        ] {
            let parsed = Cli::try_parse_from(args.clone());
            assert!(parsed.is_err(), "legacy root command parsed: {args:?}");
        }
    }

    #[test]
    fn source_display_ref_only_prefixes_true_arxiv_sources() {
        assert_eq!(
            source_display_ref("arxiv", Some("2605.16051"), "2605.16051"),
            "arXiv:2605.16051"
        );
        assert_eq!(
            source_display_ref(
                "local_file",
                Some("local-pdf-d96363843fd8"),
                "local-pdf-d96363843fd8"
            ),
            "local-pdf-d96363843fd8"
        );
        assert_eq!(
            source_display_ref(
                "git_repo",
                Some("git-tex-3a2e680b410f"),
                "git-tex-3a2e680b410f"
            ),
            "git-tex-3a2e680b410f"
        );
    }

    #[tokio::test]
    async fn refresh_revalidate_reports_stopped_local_web_as_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let result = refresh_web_revalidate(
            &client,
            Some(&format!("http://127.0.0.1:{port}/api/revalidate")),
            Some("secret"),
            Uuid::nil(),
            std::time::Duration::from_millis(300),
        )
        .await;

        assert_eq!(result.status, "skipped_unreachable");
        assert!(result.duration_ms < 1_000, "{result:?}");
    }

    #[tokio::test]
    async fn refresh_revalidate_times_out_slow_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/revalidate"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok": true}))
                    .set_delay(std::time::Duration::from_secs(2)),
            )
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let result = refresh_web_revalidate(
            &client,
            Some(&format!("{}/api/revalidate", server.uri())),
            None,
            Uuid::nil(),
            std::time::Duration::from_millis(50),
        )
        .await;

        assert_eq!(result.status, "timeout");
        assert!(result.duration_ms < 1_000, "{result:?}");
    }

    #[test]
    fn real_existing_pr_url_accepts_only_real_github_pull_urls() {
        let real = "https://github.com/GrokRxiv/grokrxiv-reviews/pull/123";
        assert_eq!(real_existing_pr_url(Some(real)), Some(real));
        assert_eq!(
            real_existing_pr_url(Some(
                "https://github.com/GrokRxiv/grokrxiv-reviews/pull/SIMULATED-123"
            )),
            None
        );
        assert_eq!(
            real_existing_pr_url(Some(
                "https://github.com/GrokRxiv/grokrxiv-reviews/issues/123"
            )),
            None
        );
        assert_eq!(
            real_existing_pr_url(Some(
                "https://example.com/GrokRxiv/grokrxiv-reviews/pull/123"
            )),
            None
        );
    }

    #[tokio::test]
    async fn app_approve_dry_run_returns_before_db_or_github() {
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();
        let cli = Cli::try_parse_from([
            "agh",
            "--dry-run",
            "app",
            "run",
            "grokrxiv",
            "--",
            "approve",
            &review_id.to_string(),
        ])
        .expect("agh app run grokrxiv approve --dry-run should parse");

        run(cli)
            .await
            .expect("agh app run grokrxiv approve --dry-run should not require DB or GitHub");
    }

    #[test]
    fn cli_parses_status_flags() {
        let status = Cli::try_parse_from(["agh", "--status", "doctor"]).unwrap();
        assert!(status.status);
        assert!(!status.no_status);
        assert!(!status.debug_logs);

        let no_status = Cli::try_parse_from(["agh", "--no-status", "doctor"]).unwrap();
        assert!(!no_status.status);
        assert!(no_status.no_status);

        let debug_logs = Cli::try_parse_from(["agh", "--debug-logs", "doctor"]).unwrap();
        assert!(debug_logs.debug_logs);

        let both = Cli::try_parse_from(["agh", "--status", "--no-status", "doctor"]);
        assert!(
            both.is_err(),
            "--status and --no-status must be mutually exclusive"
        );
    }

    #[test]
    fn json_foreground_runs_still_show_clean_status() {
        let cli = Cli::try_parse_from(["agh", "--json", "doctor"]).unwrap();
        assert!(status_enabled_for_stderr(&cli, true));
        assert!(!status_enabled_for_stderr(&cli, false));
    }

    #[test]
    fn no_status_suppresses_status_even_for_foreground_runs() {
        let cli = Cli::try_parse_from(["agh", "--no-status", "doctor"]).unwrap();
        assert!(!status_enabled_for_stderr(&cli, true));
    }

    #[test]
    fn explicit_status_forces_status_for_redirected_stderr() {
        let cli = Cli::try_parse_from(["agh", "--status", "--json", "doctor"]).unwrap();
        assert!(status_enabled_for_stderr(&cli, false));
    }

    #[test]
    fn cli_parses_agh_grokrxiv_extract_command() {
        let parsed = Cli::try_parse_from([
            "agh",
            "--extractor",
            "cli",
            "--status",
            "app",
            "run",
            "grokrxiv",
            "--",
            "extract",
            "2605.00561",
        ])
        .unwrap();

        assert_eq!(parsed.extractor, Some(ExtractorKind::Cli));
        assert!(parsed.status);
        match parsed.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["extract", "2605.00561"]);
            }
            other => panic!("expected agh app run grokrxiv extract, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_agh_c2rust_migrate_action() {
        let parsed = Cli::try_parse_from([
            "agh",
            "--json",
            "app",
            "run",
            "c2rust",
            "--",
            "migrate",
            "--input",
            "src/main.c",
        ])
        .unwrap();

        assert!(parsed.json);
        match parsed.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "c2rust");
                assert_eq!(args, vec!["migrate", "--input", "src/main.c"]);
            }
            other => panic!("expected agh app run c2rust migrate, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_dag_validate_command() {
        let parsed = Cli::try_parse_from([
            "grokrxiv",
            "--json",
            "dag",
            "validate",
            "--dag-type",
            "paper-review",
        ])
        .unwrap();

        match parsed.command {
            Command::Dag {
                command: DagCommand::Validate { dag_type },
            } => assert_eq!(dag_type.as_deref(), Some("paper-review")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_dag_run_command() {
        let parsed =
            Cli::try_parse_from(["grokrxiv", "--json", "dag", "run", "--dag-type", "c2rust"])
                .unwrap();

        match parsed.command {
            Command::Dag {
                command: DagCommand::Run { dag_type },
            } => assert_eq!(dag_type, "c2rust"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_agent_place_command() {
        let parsed = Cli::try_parse_from(["agh", "agent", "place", "agent.yaml"]).unwrap();

        match parsed.command {
            Command::Agent {
                command: AgentCommand::Place { path },
            } => assert_eq!(path, std::path::PathBuf::from("agent.yaml")),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_dag_add_and_remove_agent_commands() {
        let add = Cli::try_parse_from([
            "grokrxiv",
            "dag",
            "add-agent",
            "--dag-type",
            "paper-review",
            "--role-id",
            "type_theory_validator",
            "--kind",
            "type_theory_validator",
            "--after",
            "specialist_quorum",
            "--before",
            "meta_reviewer",
            "--write",
        ])
        .unwrap();

        match add.command {
            Command::Dag {
                command:
                    DagCommand::AddAgent {
                        dag_type,
                        role_id,
                        kind,
                        after,
                        before,
                        write,
                        ..
                    },
            } => {
                assert_eq!(dag_type, "paper-review");
                assert_eq!(role_id, "type_theory_validator");
                assert_eq!(kind, "type_theory_validator");
                assert_eq!(after, vec!["specialist_quorum"]);
                assert_eq!(before, vec!["meta_reviewer"]);
                assert!(write);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let remove = Cli::try_parse_from([
            "grokrxiv",
            "dag",
            "remove-agent",
            "--dag-type",
            "paper-review",
            "--role-id",
            "type_theory_validator",
        ])
        .unwrap();

        match remove.command {
            Command::Dag {
                command:
                    DagCommand::RemoveAgent {
                        dag_type,
                        role_id,
                        write,
                    },
            } => {
                assert_eq!(dag_type, "paper-review");
                assert_eq!(role_id, "type_theory_validator");
                assert!(!write);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_dag_add_remove_and_scaffold_tool_commands() {
        let add = Cli::try_parse_from([
            "grokrxiv",
            "dag",
            "add-tool",
            "--dag-type",
            "citation-validation",
            "--tool-id",
            "doi_resolver",
            "--executor",
            "rust",
            "--handler",
            "citation_validation::doi_resolver",
            "--after",
            "bibtex_reference_parser",
            "--before",
            "semantic_similarity_check",
            "--input",
            "references.json",
            "--output",
            "citation_validation_report.json",
            "--write",
        ])
        .unwrap();

        match add.command {
            Command::Dag {
                command:
                    DagCommand::AddTool {
                        dag_type,
                        tool_id,
                        executor,
                        handler,
                        after,
                        before,
                        inputs,
                        outputs,
                        write,
                        ..
                    },
            } => {
                assert_eq!(dag_type, "citation-validation");
                assert_eq!(tool_id, "doi_resolver");
                assert_eq!(executor, "rust");
                assert_eq!(
                    handler.as_deref(),
                    Some("citation_validation::doi_resolver")
                );
                assert_eq!(after, vec!["bibtex_reference_parser"]);
                assert_eq!(before, vec!["semantic_similarity_check"]);
                assert_eq!(inputs, vec!["references.json"]);
                assert_eq!(outputs, vec!["citation_validation_report.json"]);
                assert!(write);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let remove = Cli::try_parse_from([
            "grokrxiv",
            "dag",
            "remove-tool",
            "--dag-type",
            "citation-validation",
            "--tool-id",
            "doi_resolver",
        ])
        .unwrap();

        match remove.command {
            Command::Dag {
                command:
                    DagCommand::RemoveTool {
                        dag_type,
                        tool_id,
                        write,
                    },
            } => {
                assert_eq!(dag_type, "citation-validation");
                assert_eq!(tool_id, "doi_resolver");
                assert!(!write);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let scaffold = Cli::try_parse_from([
            "grokrxiv",
            "dag",
            "scaffold-tool",
            "--dag-type",
            "citation-validation",
            "--tool-id",
            "metadata_consistency_validator",
            "--handler",
            "citation_validation::metadata_consistency_validator",
        ])
        .unwrap();

        match scaffold.command {
            Command::Dag {
                command:
                    DagCommand::ScaffoldTool {
                        dag_type,
                        tool_id,
                        handler,
                        ..
                    },
            } => {
                assert_eq!(dag_type, "citation-validation");
                assert_eq!(tool_id, "metadata_consistency_validator");
                assert_eq!(
                    handler.as_deref(),
                    Some("citation_validation::metadata_consistency_validator")
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_batch_and_jobs_commands() {
        let batch = Cli::try_parse_from([
            "agh",
            "--json",
            "app",
            "run",
            "grokrxiv",
            "--",
            "batch-create",
            "--category",
            "math",
            "--month",
            "2026-05",
            "--daily-limit",
            "30",
            "--max-items",
            "15",
            "--auto-pr",
        ])
        .expect("batch create should parse");
        assert!(batch.json);
        match batch.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "batch-create",
                        "--category",
                        "math",
                        "--month",
                        "2026-05",
                        "--daily-limit",
                        "30",
                        "--max-items",
                        "15",
                        "--auto-pr",
                    ]
                );
            }
            other => panic!("expected batch create, got {other:?}"),
        }

        let batch_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();
        let run = Cli::try_parse_from([
            "agh",
            "app",
            "run",
            "grokrxiv",
            "--",
            "batch-run",
            &batch_id.to_string(),
            "--limit",
            "5",
        ])
        .expect("batch run should parse");
        match run.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "batch-run".to_string(),
                        batch_id.to_string(),
                        "--limit".into(),
                        "5".into()
                    ]
                );
            }
            other => panic!("expected batch run, got {other:?}"),
        }

        let jobs = Cli::try_parse_from([
            "agh", "--json", "jobs", "list", "--kind", "review", "--state", "running",
        ])
        .expect("jobs list should parse");
        assert!(jobs.json);
        match jobs.command {
            Command::Jobs {
                command: JobsCommand::List { kind, state, .. },
            } => {
                assert_eq!(kind.as_deref(), Some("review"));
                assert_eq!(state.as_deref(), Some("running"));
            }
            other => panic!("expected jobs list, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_agh_grokrxiv_review_extracted_command() {
        let parsed = Cli::try_parse_from([
            "agh",
            "--runner",
            "cli",
            "--status",
            "app",
            "run",
            "grokrxiv",
            "--",
            "review-extracted",
            "2605.00561",
        ])
        .unwrap();

        assert_eq!(parsed.runner, Some(AgentRunnerKind::Cli));
        assert!(parsed.status);
        match parsed.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["review-extracted", "2605.00561"]);
            }
            other => panic!("expected review-extracted command, got {other:?}"),
        }

        let forced = Cli::try_parse_from([
            "agh",
            "app",
            "run",
            "grokrxiv",
            "--",
            "review-extracted",
            "--force",
            "2605.00561",
        ])
        .unwrap();
        match forced.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["review-extracted", "--force", "2605.00561"])
            }
            other => panic!("expected forced review-extracted command, got {other:?}"),
        }
    }

    #[test]
    fn parse_grokrxiv_review_args_accepts_loop_flag() {
        let parsed = parse_grokrxiv_review_args(vec![
            "https://arxiv.org/abs/2606.00799".to_string(),
            "--loop".to_string(),
            "--debug".to_string(),
            "--no-external-actions".to_string(),
            "--type".to_string(),
            "arxiv".to_string(),
        ])
        .expect("review --loop args parse");

        assert_eq!(parsed.source, "https://arxiv.org/abs/2606.00799");
        assert_eq!(parsed.source_type, Some(SourceType::Arxiv));
        assert!(parsed.loop_enabled);
        assert!(parsed.debug_output);
        assert!(parsed.no_external_actions);
    }

    #[test]
    fn review_loop_stage_plan_is_loaded_from_manifest() {
        let stages = review_loop_stage_plan().expect("review-loop stage plan");

        assert_eq!(
            stages.first().map(|stage| stage.id.as_str()),
            Some("paper_review")
        );
        let paper_math_source_collector = stages
            .iter()
            .find(|stage| stage.id == "paper_math_source_collector")
            .expect("paper math source collector stage");
        assert!(paper_math_source_collector
            .outputs
            .iter()
            .any(|output| output == "review_loop/paper_math_sources.json"));
        let semantic_mapper = stages
            .iter()
            .find(|stage| stage.id == "semantic_category_mapper")
            .expect("semantic mapper stage");
        assert!(semantic_mapper
            .inputs
            .iter()
            .any(|input| input == "review_loop/paper_math_sources.json"));
        assert!(stages.iter().any(|stage| {
            stage.id == "citation_validation"
                && stage.kind == "dag_call"
                && stage.dag_type.as_deref() == Some("citation-validation")
        }));
        assert_eq!(
            stages.last().map(|stage| stage.id.as_str()),
            Some("publish_decision")
        );
    }

    #[tokio::test]
    async fn review_loop_bundle_completeness_flags_missing_declared_outputs() {
        let stages = vec![
            ReviewLoopStage {
                id: "citation_validation".to_string(),
                kind: "dag_call".to_string(),
                dag_type: Some("citation-validation".to_string()),
                inputs: vec!["references.json".to_string()],
                outputs: vec![
                    "citation_validation_report.json".to_string(),
                    "citation_validation_adjudication.json".to_string(),
                ],
                required: true,
            },
            ReviewLoopStage {
                id: "pr_fixer".to_string(),
                kind: "tool".to_string(),
                dag_type: None,
                inputs: vec![],
                outputs: vec![
                    "review_loop/pr_fixes.json".to_string(),
                    "review_loop/fixed/review.pdf".to_string(),
                ],
                required: true,
            },
        ];
        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-review-loop-bundle-")
            .tempdir()
            .expect("tempdir");
        tokio::fs::write(
            tempdir.path().join("citation_validation_report.json"),
            br#"{"status":"fail"}"#,
        )
        .await
        .expect("write citation report");
        tokio::fs::create_dir_all(tempdir.path().join("fixed"))
            .await
            .expect("fixed dir");
        tokio::fs::write(
            tempdir.path().join("pr_fixes.json"),
            br#"{"status":"fail","fixed_pdf":null}"#,
        )
        .await
        .expect("write pr fixes");

        let report =
            review_loop_bundle_completeness_report(&stages, tempdir.path(), &Default::default())
                .await
                .expect("bundle report");

        assert_eq!(report["status"].as_str(), Some("fail"));
        assert_eq!(report["missing_count"].as_u64(), Some(2));
        let missing_outputs = report["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .filter(|artifact| artifact["status"] == "missing")
            .filter_map(|artifact| artifact["manifest_output"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            missing_outputs,
            vec![
                "citation_validation_adjudication.json",
                "review_loop/fixed/review.pdf"
            ]
        );
    }

    #[tokio::test]
    async fn review_loop_bundle_completeness_accepts_explicit_skip_reasons() {
        let stages = vec![ReviewLoopStage {
            id: "pr_fixer".to_string(),
            kind: "tool".to_string(),
            dag_type: None,
            inputs: vec![],
            outputs: vec!["review_loop/fixed/review.pdf".to_string()],
            required: true,
        }];
        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-review-loop-bundle-skip-")
            .tempdir()
            .expect("tempdir");
        let mut skip_reasons = std::collections::BTreeMap::new();
        skip_reasons.insert(
            "review_loop/fixed/review.pdf".to_string(),
            "fixed review.pdf was not produced because LaTeX compilation failed".to_string(),
        );

        let report = review_loop_bundle_completeness_report(&stages, tempdir.path(), &skip_reasons)
            .await
            .expect("bundle report");

        assert_eq!(report["status"].as_str(), Some("pass"));
        assert_eq!(report["skipped_count"].as_u64(), Some(1));
        assert_eq!(
            report["artifacts"][0]["skip_reason"].as_str(),
            Some("fixed review.pdf was not produced because LaTeX compilation failed")
        );
    }

    #[test]
    fn review_loop_bundle_skip_reasons_include_current_honest_skips() {
        let citation_report = serde_json::json!({
            "stage": "citation_validation",
            "status": "fail",
            "source": "paper-review citation verifier evidence plus declared review-loop DAG call"
        });
        let pr_fixes = serde_json::json!({
            "stage": "pr_fixer",
            "status": "fail",
            "fixed_pdf": null,
            "issues": ["fixed review.pdf was not produced"]
        });

        let skip_reasons = review_loop_bundle_skip_reasons(&citation_report, &pr_fixes);

        assert!(skip_reasons
            .get("citation_validation_adjudication.json")
            .expect("citation adjudication skip")
            .contains("citation_validation_report.json"));
        assert_eq!(
            skip_reasons
                .get("review_loop/fixed/review.pdf")
                .map(String::as_str),
            Some("fixed review.pdf was not produced")
        );
    }

    #[tokio::test]
    async fn pr_fixer_accepts_compilable_rendered_tex_without_agent() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-pr-fast-path-")
            .tempdir()
            .expect("tempdir");
        let source_tex = tempdir.path().join("source-review.tex");
        let fixed_dir = tempdir.path().join("fixed");
        let fixed_tex = fixed_dir.join("review.tex");
        let fake_latex = tempdir.path().join("fake-latex.sh");

        tokio::fs::write(
            &source_tex,
            "\\documentclass{article}\\begin{document}ok\\end{document}\n",
        )
        .await
        .expect("source tex");
        tokio::fs::write(
            &fake_latex,
            "#!/bin/sh\nset -eu\ncp review.tex review.pdf\n",
        )
        .await
        .expect("fake latex");
        let mut perms = std::fs::metadata(&fake_latex)
            .expect("fake latex metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_latex, perms).expect("chmod fake latex");

        let report = try_compile_existing_pr_artifact(
            &source_tex,
            &fixed_tex,
            &fixed_dir,
            fake_latex.to_str().expect("script path utf8"),
            &[],
            5,
        )
        .await
        .expect("compile fast path")
        .expect("compilable source should produce a PR fixer report");

        assert_eq!(report["status"].as_str(), Some("pass"));
        assert_eq!(
            report["compile_review_loop"]["agent_output_audit_summary"]["total"].as_u64(),
            Some(0)
        );
        assert_eq!(
            report["compile_review_loop"]["attempts"][0]["compile"]["status"].as_str(),
            Some("pass")
        );
        assert!(fixed_tex.is_file());
        assert!(fixed_dir.join("review.pdf").is_file());
    }

    #[test]
    fn review_loop_n5_halts_tier_c_when_lean_reports_proved() {
        let corpus_context = ReviewLoopCorpusContext {
            id: "blum-pvnp".to_string(),
            tier: "C".to_string(),
            source: "arxiv:1708.03486".to_string(),
            expected_recommendation: Some("reject_or_major_revision".to_string()),
        };
        let theorem_map = serde_json::json!({
            "status": "PROVED",
            "entries": [
                {
                    "obligation_id": "theorem_1",
                    "kind": "theorem_formalization",
                    "status": "PROVED",
                    "statement": "P = NP"
                }
            ]
        });

        let dossier = review_loop_n5_false_proof_halt(&corpus_context, &theorem_map)
            .expect("Tier C PROVED must trigger N5");

        assert_eq!(dossier["never_event"], "N5_fake_proof");
        assert_eq!(dossier["action"], "halt_and_escalate");
        assert_eq!(dossier["corpus"]["id"], "blum-pvnp");
        assert_eq!(dossier["corpus"]["tier"], "C");
        assert_eq!(dossier["lean_verdict"], "PROVED");
        assert!(dossier["reason"]
            .as_str()
            .unwrap()
            .contains("Lean reported PROVED for a Tier C/G flawed or false claim"));
    }

    #[test]
    fn review_loop_corpus_context_matches_arxiv_source_with_version() {
        let contexts =
            review_loop_corpus_contexts_from_yaml(include_str!("../../../evals/corpus.yaml"))
                .expect("corpus contexts parse");
        let mut candidates = BTreeSet::new();
        add_review_loop_source_candidate(
            &mut candidates,
            Some("https://arxiv.org/abs/1708.03486v2"),
        );

        let context = review_loop_corpus_context_for_candidates(&contexts, &candidates)
            .expect("Blum P vs NP corpus context");

        assert_eq!(context.id, "blum-pvnp");
        assert_eq!(context.tier, "C");
        assert_eq!(
            context.expected_recommendation.as_deref(),
            Some("reject_or_major_revision")
        );
    }

    #[test]
    fn arxiv_review_source_parsing_preserves_version_suffix() {
        assert_eq!(
            parse_arxiv_source("2407.07620v4").as_deref(),
            Some("2407.07620v4")
        );
        assert_eq!(
            parse_arxiv_source("https://arxiv.org/abs/2407.07620v4").as_deref(),
            Some("2407.07620v4")
        );
        assert_eq!(
            parse_arxiv_source("https://arxiv.org/pdf/2407.07620v4.pdf").as_deref(),
            Some("2407.07620v4")
        );
    }

    #[test]
    fn review_loop_corpus_context_carries_tier_r_honest_recommendation() {
        let contexts =
            review_loop_corpus_contexts_from_yaml(include_str!("../../../evals/corpus.yaml"))
                .expect("corpus contexts parse");
        let mut candidates = BTreeSet::new();
        add_review_loop_source_candidate(&mut candidates, Some("arxiv:2606.00799v1"));

        let context = review_loop_corpus_context_for_candidates(&contexts, &candidates)
            .expect("PR-54 Weyl regression corpus context");

        assert_eq!(context.id, "regression-pr54-weyl");
        assert_eq!(context.tier, "R");
        assert_eq!(context.expected_recommendation.as_deref(), Some("honest"));
    }

    #[test]
    fn review_loop_corpus_context_matches_synthetic_false_theorem_path() {
        let contexts =
            review_loop_corpus_contexts_from_yaml(include_str!("../../../evals/corpus.yaml"))
                .expect("corpus contexts parse");
        let false_theorem_path = crate::dag_apps::app_root("grokrxiv")
            .join("evals")
            .join("synthetic")
            .join("false-theorem")
            .join("paper.tex");
        let mut candidates = BTreeSet::new();
        add_review_loop_source_candidate(
            &mut candidates,
            Some(&format!("file://{}", false_theorem_path.display())),
        );

        let context = review_loop_corpus_context_for_candidates(&contexts, &candidates)
            .expect("synthetic false theorem corpus context");

        assert_eq!(context.id, "synthetic-false-theorem");
        assert_eq!(context.tier, "G");
    }

    #[tokio::test]
    async fn corpus_synthetic_entries_are_live_app_relative_manuscripts() {
        let corpus: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../evals/corpus.yaml"))
                .expect("corpus.yaml parses");
        let entries = corpus
            .get("entries")
            .and_then(|value| value.as_sequence())
            .expect("corpus entries");
        for id in [
            "synthetic-bad-citations",
            "synthetic-injection",
            "synthetic-false-theorem",
        ] {
            let entry = entries
                .iter()
                .find(|entry| entry.get("id").and_then(|value| value.as_str()) == Some(id))
                .unwrap_or_else(|| panic!("missing corpus entry {id}"));
            assert_ne!(
                entry.get("status").and_then(|value| value.as_str()),
                Some("to_author"),
                "{id} must be live, not a placeholder"
            );
            let source = entry
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{id} missing source"));
            assert!(
                source.ends_with("/paper.tex"),
                "{id} source must point at the reviewable synthetic TeX manuscript"
            );

            let expected_path = crate::dag_apps::app_root("grokrxiv").join(source);
            assert!(
                expected_path.is_file(),
                "{id} source does not exist at {}",
                expected_path.display()
            );
            let expected_canonical = expected_path
                .canonicalize()
                .expect("synthetic source canonical path");
            let resolved = resolve_source(source, None)
                .await
                .unwrap_or_else(|err| panic!("{id} source must resolve through review CLI: {err}"));
            assert_eq!(resolved.len(), 1, "{id} should resolve to one manuscript");
            match &resolved[0] {
                ResolvedSource::LocalFile(path, SourceType::Tex, false) => {
                    assert_eq!(
                        path.canonicalize().expect("resolved path canonical"),
                        expected_canonical
                    );
                }
                other => panic!("{id} resolved to unexpected source: {other:?}"),
            }

            let prepared = grokrxiv_ingest::prepare_review_source(
                grokrxiv_ingest::ReviewSourceSpec::LocalFile {
                    path: expected_path,
                    format: Some(grokrxiv_ingest::LocalSourceFormat::Tex),
                    title: None,
                    authors: Vec::new(),
                    field: None,
                },
            )
            .await
            .unwrap_or_else(|err| panic!("{id} source must parse for review: {err}"));
            let body_chars = prepared
                .extract
                .sections
                .iter()
                .map(|section| {
                    section.heading.chars().count() + section.body_markdown.chars().count()
                })
                .sum::<usize>();
            assert!(
                body_chars >= 1_000,
                "{id} parsed body must pass extraction completeness gate, got {body_chars} chars"
            );
        }
    }

    #[test]
    fn corpus_arxiv_versions_and_toolchains_are_pinned() {
        let corpus: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../evals/corpus.yaml"))
                .expect("corpus.yaml parses");
        let entries = corpus
            .get("entries")
            .and_then(|value| value.as_sequence())
            .expect("corpus entries");
        let mut unpinned = Vec::new();
        for entry in entries {
            let source = entry.get("source").and_then(|value| value.as_str());
            if !source.is_some_and(|value| value.starts_with("arxiv:")) {
                continue;
            }
            let id = entry
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing-id>");
            let version = entry.get("version").and_then(|value| value.as_str());
            match version {
                Some(value) if value.starts_with('v') && value[1..].parse::<u64>().is_ok() => {}
                Some(value) => unpinned.push(format!("{id}={value}")),
                None => unpinned.push(format!("{id}=<missing>")),
            }
        }
        assert!(
            unpinned.is_empty(),
            "arXiv corpus entries must pin concrete versions: {}",
            unpinned.join(", ")
        );

        let lock_path = crate::dag_apps::app_root("grokrxiv").join("evals/toolchain.lock.yaml");
        assert!(
            lock_path.is_file(),
            "missing eval toolchain lock at {}",
            lock_path.display()
        );
        let lock: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&lock_path).expect("read eval toolchain lock"),
        )
        .expect("toolchain.lock.yaml parses");

        assert_eq!(lock["version"].as_i64(), Some(1));
        assert_eq!(
            lock["runner_environment"]["command"].as_str(),
            Some("agenthero/apps/grokrxiv/evals/bin/grokrxiv-corpus-env")
        );
        assert_eq!(
            lock["toolchains"]["ghc"]["version"].as_str(),
            Some("9.14.1")
        );
        assert_eq!(
            lock["toolchains"]["lean"]["version"].as_str(),
            Some("4.30.0")
        );
        assert_eq!(
            lock["toolchains"]["lean"]["commit"].as_str(),
            Some("d024af099ca4bf2c86f649261ebf59565dc8c622")
        );
        assert_eq!(
            lock["toolchains"]["lake"]["version"].as_str(),
            Some("5.0.0-src+d024af0")
        );
        assert_eq!(
            lock["toolchains"]["lake"]["lean_version"].as_str(),
            Some("4.30.0")
        );
        assert_eq!(
            lock["toolchains"]["mathlib"]["source"].as_str(),
            Some("https://github.com/leanprover-community/mathlib4.git")
        );
        assert_eq!(
            lock["toolchains"]["mathlib"]["rev"].as_str(),
            Some("c5ea00351c28e24afc9f0f84379aa41082b1188f")
        );
        assert_eq!(
            lock["toolchains"]["mathlib"]["tag"].as_str(),
            Some("v4.30.0")
        );

        let app_root = crate::dag_apps::app_root("grokrxiv");
        let lean_toolchain_path = app_root.join(
            lock["toolchains"]["lean"]["toolchain_file"]
                .as_str()
                .expect("lean toolchain_file"),
        );
        let lakefile_path = app_root.join(
            lock["toolchains"]["lake"]["lakefile"]
                .as_str()
                .expect("lakefile path"),
        );
        let lake_manifest_path = app_root.join(
            lock["toolchains"]["mathlib"]["manifest"]
                .as_str()
                .expect("mathlib manifest path"),
        );
        let lean_toolchain =
            std::fs::read_to_string(&lean_toolchain_path).expect("read eval lean-toolchain");
        let lakefile = std::fs::read_to_string(&lakefile_path).expect("read eval lakefile");
        let lake_manifest =
            std::fs::read_to_string(&lake_manifest_path).expect("read eval lake manifest");
        assert_eq!(lean_toolchain.trim(), "leanprover/lean4:v4.30.0");
        assert!(
            lakefile.contains("c5ea00351c28e24afc9f0f84379aa41082b1188f"),
            "eval lakefile must pin the locked mathlib revision"
        );
        assert!(
            lake_manifest.contains("\"rev\": \"c5ea00351c28e24afc9f0f84379aa41082b1188f\""),
            "eval lake manifest must resolve the locked mathlib revision"
        );
    }

    #[cfg(unix)]
    #[test]
    fn corpus_toolchain_env_selects_pinned_ghc_over_stale_path() {
        use std::os::unix::fs::PermissionsExt;

        let app_root = crate::dag_apps::app_root("grokrxiv");
        let env_script = app_root.join("evals/bin/grokrxiv-corpus-env");
        assert!(
            env_script.is_file(),
            "missing corpus toolchain runner at {}",
            env_script.display()
        );

        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-ghc-path-fixture")
            .tempdir()
            .expect("temp dir");
        let stale_dir = tempdir.path().join("stale-bin");
        let pinned_dir = tempdir.path().join("pinned-bin");
        std::fs::create_dir_all(&stale_dir).expect("stale bin dir");
        std::fs::create_dir_all(&pinned_dir).expect("pinned bin dir");
        let stale_ghc = stale_dir.join("ghc");
        let pinned_ghc = pinned_dir.join("ghc");
        std::fs::write(
            &stale_ghc,
            "#!/bin/sh\nif [ \"$1\" = \"--numeric-version\" ]; then echo 8.4.2; else echo stale-ghc; fi\n",
        )
        .expect("write stale ghc");
        std::fs::write(
            &pinned_ghc,
            "#!/bin/sh\nif [ \"$1\" = \"--numeric-version\" ]; then echo 9.14.1; else echo pinned-ghc; fi\n",
        )
        .expect("write pinned ghc");
        for path in [&stale_ghc, &pinned_ghc] {
            let mut perms = std::fs::metadata(path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod ghc fixture");
        }

        let output = std::process::Command::new(&env_script)
            .arg("ghc")
            .arg("--numeric-version")
            .env("GROKRXIV_GHC_BIN", &pinned_ghc)
            .env("PATH", &stale_dir)
            .output()
            .expect("run corpus toolchain env");

        assert!(
            output.status.success(),
            "toolchain env failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "9.14.1",
            "corpus toolchain env must prefer the pinned GHC over stale PATH"
        );
    }

    #[test]
    fn review_loop_n5_does_not_halt_tier_a_proved() {
        let corpus_context = ReviewLoopCorpusContext {
            id: "bertrand-elementary".to_string(),
            tier: "A".to_string(),
            source: "arxiv:2407.07620".to_string(),
            expected_recommendation: Some("accept".to_string()),
        };
        let theorem_map = serde_json::json!({
            "status": "PROVED",
            "entries": [
                {
                    "kind": "theorem_formalization",
                    "status": "PROVED"
                }
            ]
        });

        assert!(review_loop_n5_false_proof_halt(&corpus_context, &theorem_map).is_none());
    }

    #[test]
    fn tier_r_honest_recommendation_is_integrity_ready_without_publisher_ready() {
        let corpus_context = ReviewLoopCorpusContext {
            id: "regression-pr54-weyl".to_string(),
            tier: "R".to_string(),
            source: "arxiv:2606.00799".to_string(),
            expected_recommendation: Some("honest".to_string()),
        };
        let publication_gate = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Fail,
            reason: "Meta-review recommendation is `major_revision`, not `accept`.".to_string(),
            recommendation: "major_revision".to_string(),
        };

        let policy = review_loop_publication_gate_policy(Some(&corpus_context), &publication_gate);

        assert!(policy.integrity_ready);
        assert!(!policy.publisher_ready);
        assert_eq!(policy.blocking_issue, None);
        assert_eq!(policy.status, "honest_non_publishing_recommendation");
    }

    #[test]
    fn review_loop_halt_disables_external_actions() {
        let outcome = ReviewLoopOutcome {
            publisher_ready: false,
            deterministic_status: "fail".to_string(),
            halted: true,
            blocking_issues: vec!["N5 fake proof never-event triggered.".to_string()],
            artifact_dir: "/tmp/review_loop".to_string(),
            report_path: "/tmp/review_loop/review_loop_report.json".to_string(),
            report: serde_json::json!({}),
        };

        assert!(!review_loop_external_actions_allowed(true, Some(&outcome)));
    }

    #[tokio::test]
    async fn review_loop_code_harness_initializes_git_branch() {
        if !command_available("git") {
            return;
        }
        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-review-loop-harness-")
            .tempdir()
            .expect("tempdir");
        let review_id = Uuid::parse_str("76665eba-7670-47ef-b69d-42a0af86eba7").unwrap();
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };

        let harness = prepare_review_loop_git_harness(
            review_id,
            &task,
            serde_json::json!({"claims": []}),
            tempdir.path(),
        )
        .await
        .expect("git harness");

        assert!(tempdir.path().join(".git").is_dir());
        assert_eq!(harness.branch, "review-loop/haskell/76665eba7670");
        let branch = run_process(
            "git",
            vec!["branch".to_string(), "--show-current".to_string()],
            Some(tempdir.path()),
        )
        .await
        .expect("branch");
        assert_eq!(branch.trim(), harness.branch);
        assert!(tempdir.path().join("GROKRXIV_HARNESS.md").exists());

        tokio::fs::write(
            tempdir.path().join("SemanticModel.hs"),
            "module SemanticModel where\nclaimCount :: Int\nclaimCount = 0\n",
        )
        .await
        .expect("write generated code");
        let evidence = record_review_loop_harness_attempt(&harness, "haskell", 1).await;
        assert_eq!(
            evidence["commit"]["commit"]["status"].as_str(),
            Some("pass")
        );
        assert!(
            evidence["commit"]["head"]["stdout"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .len()
                >= 7
        );
    }

    #[tokio::test]
    async fn review_loop_recovers_code_artifact_written_before_author_timeout() {
        let tempdir = tempfile::Builder::new()
            .prefix("grokrxiv-review-loop-recover-")
            .tempdir()
            .expect("tempdir");
        let final_path = tempdir.path().join("SemanticModel.hs");
        let recovered_code = "module SemanticModel where\n\
data SourceSpan = SourceSpan deriving (Eq, Show)\n\
data MathType = NatType deriving (Eq, Show)\n\
data Term = Var String deriving (Eq, Show)\n\
data Proposition = PTrue deriving (Eq, Show)\n\
data Binder = Binder deriving (Eq, Show)\n\
data TheoremIR = TheoremIR deriving (Eq, Show)\n\
data ClaimIR = ClaimIR deriving (Eq, Show)\n\
data Definition = Definition deriving (Eq, Show)\n\
data Assumption = Assumption deriving (Eq, Show)\n\
data ProofObligation = ProofObligation deriving (Eq, Show)\n\
data LeanTarget = LeanTarget deriving (Eq, Show)\n\
categoryToObligations = []\n\
claimToObligations = []\n\
obligationToLean = LeanTarget\n";
        tokio::fs::write(&final_path, recovered_code)
            .await
            .expect("write recovered artifact");
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };

        let run = recovered_agent_run_from_code_file(
            task.author_role,
            &task,
            &final_path,
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap(),
            "CliRunner timed out after 360s for role haskell_semantic_author",
        )
        .await
        .expect("recover code artifact")
        .expect("artifact should be recoverable");

        assert_eq!(run.role, "haskell_semantic_author");
        assert_eq!(run.output["language"], "haskell");
        assert_eq!(run.output["filename"], "SemanticModel.hs");
        assert_eq!(run.output["code"], recovered_code);
        assert!(run.cache_hit);
        assert!(run.output["notes"][0]
            .as_str()
            .unwrap_or_default()
            .contains("recovered from on-disk artifact after runner failure"));
    }

    #[test]
    fn review_loop_haskell_code_payload_elides_bulk_math_context() {
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };
        let bulk_equation = "bulk supporting equation text ".repeat(500);
        let base = serde_json::json!({
            "review_id": "11111111-1111-1111-1111-111111111111",
            "task": "Generate Haskell IR",
            "requirements": ["retain formal theorem targets"],
            "claims": {
                "claims": [
                    {"id": "claim_1", "statement": "review metadata must not become Haskell"}
                ]
            },
            "knowledge_graph": {
                "nodes": [
                    {"id": "claim_1", "label": "review metadata must not become Haskell"}
                ],
                "edges": []
            },
            "paper_math_sources": {
                "equations": {
                    "equations": [
                        {"id": "eq_1", "statement": bulk_equation},
                        {"id": "eq_2", "statement": "context equation"}
                    ]
                },
                "theorem_graph": {"nodes": [{"id": "thm_1"}]},
                "artifact_sources": [{"artifact": "body.tex"}],
                "warnings": []
            },
            "semantic_ir": {
                "schema_version": "1.0.0",
                "source": "paper_math_sources",
                "review_id": "11111111-1111-1111-1111-111111111111",
                "formalization_policy": {"tier": "P0"},
                "theorem_candidates": [
                    {
                        "id": "thm_1",
                        "formalization_class": "formal_math",
                        "statement": "For all n, n + 0 = n.",
                        "formalization_target": {
                            "lean_declaration": "thm_1",
                            "expected_shape": "theorem"
                        }
                    }
                ],
                "definitions": [{"id": "def_1", "statement": "Nat"}],
                "assumptions": [],
                "supporting_equations": [
                    {"id": "eq_1", "statement": bulk_equation},
                    {"id": "eq_2", "statement": "context equation"}
                ],
                "nonformal_review_claims": [
                    {"id": "nf_1", "statement": "review metadata"}
                ],
                "paper_math_sources": {
                    "equations": {
                        "equations": [{"id": "eq_1", "statement": bulk_equation}]
                    }
                }
            },
            "semantic_model": {"theorem_candidate_count": 1}
        });

        let compact = compact_review_fix_code_base_artifact(&task, base);
        let compact_text = serde_json::to_string(&compact).expect("compact json");

        assert_eq!(
            compact["semantic_ir"]["theorem_candidates"][0]["formalization_target"]
                ["lean_declaration"],
            "thm_1"
        );
        assert_eq!(
            compact["semantic_ir"]["supporting_equations_summary"]["count"],
            2
        );
        assert_eq!(
            compact["semantic_ir"]["supporting_equations"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(
            compact["paper_math_sources"]["artifact_ref"],
            "review_loop/paper_math_sources.json"
        );
        assert_eq!(
            compact["claims"]["must_not_be_modeled_as_haskell_claims"],
            true
        );
        assert_eq!(
            compact["knowledge_graph"]["omitted_from_code_author_payload"],
            true
        );
        assert_eq!(
            compact["haskell_semantic_contract"]["canonical_formal_sources"][0],
            "semantic_ir.theorem_candidates"
        );
        assert!(!compact_text.contains("bulk supporting equation text"));
        assert!(!compact_text.contains("review metadata must not become Haskell"));
        assert!(compact_text.len() < 12_000, "len={}", compact_text.len());
    }

    #[test]
    fn review_loop_deterministic_haskell_author_preserves_lean_targets() {
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };
        let base = compact_review_fix_code_base_artifact(
            &task,
            serde_json::json!({
                "semantic_ir": {
                    "schema_version": "1.0.0",
                    "theorem_candidates": [
                        {
                            "id": "thm_alpha",
                            "formalization_class": "formal_math",
                            "statement": "For all n, n + 0 = n.",
                            "source_span": {
                                "artifact": "theorem_graph.json",
                                "claim_id": "thm_alpha",
                                "section_id": "sec-test",
                                "text_excerpt": "For all n, n + 0 = n."
                            },
                            "theorem_ir": {
                                "assumptions": [],
                                "binders": [],
                                "conclusion": {
                                    "kind": "equals",
                                    "lhs": {"kind": "var", "name": "n + 0"},
                                    "rhs": {"kind": "var", "name": "n"}
                                }
                            },
                            "formalization_target": {
                                "lean_declaration": "thm_alpha",
                                "expected_shape": "theorem"
                            }
                        },
                        {
                            "id": "thm_beta",
                            "formalization_class": "formal_math",
                            "statement": "If A, then A.",
                            "formalization_target": {
                                "lean_declaration": "thm_beta",
                                "expected_shape": "lemma"
                            }
                        }
                    ],
                    "definitions": [],
                    "assumptions": [],
                    "supporting_equations": [],
                    "paper_math_sources": {}
                }
            }),
        );

        let run = deterministic_haskell_semantic_model_agent_run(task.author_role, &task, &base)
            .expect("deterministic haskell run");
        let code = run.output["code"].as_str().expect("code output");

        assert!(code.contains("thm_alpha"));
        assert!(code.contains("thm_beta"));
        assert!(code.contains("SemanticGap"));
        assert!(code.contains("sourceSectionId"));
        assert!(code.contains("sourceTextExcerpt"));
        assert!(code.contains("Equals (Var \"n + 0\") (Var \"n\")"));
        assert!(!code.contains("PRaw"));
        assert!(grokrxiv_review_loop::validate_generated_code("haskell", code, &base).is_empty());

        if std::process::Command::new("ghc")
            .arg("--numeric-version")
            .output()
            .is_ok()
        {
            let tempdir = tempfile::Builder::new()
                .prefix("grokrxiv-deterministic-haskell-")
                .tempdir()
                .expect("tempdir");
            let source_path = tempdir.path().join("SemanticModel.hs");
            std::fs::write(&source_path, code).expect("write generated haskell");
            let compile = std::process::Command::new("ghc")
                .arg("-fno-code")
                .arg(&source_path)
                .output()
                .expect("run ghc");
            assert!(
                compile.status.success(),
                "stderr={}",
                String::from_utf8_lossy(&compile.stderr)
            );
        }
    }

    #[test]
    fn review_loop_deterministic_haskell_author_filters_review_categories_and_retains_formal_semantic_gaps(
    ) {
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };
        let base = compact_review_fix_code_base_artifact(
            &task,
            serde_json::json!({
                "semantic_ir": {
                    "schema_version": "1.0.0",
                    "theorem_candidates": [
                        {
                            "id": "thm_structured",
                            "formalization_class": "formal_math",
                            "statement": "For all n, n + 0 = n.",
                            "theorem_ir": {
                                "assumptions": [],
                                "binders": [],
                                "conclusion": {
                                    "kind": "equals",
                                    "lhs": {"kind": "var", "name": "n + 0"},
                                    "rhs": {"kind": "var", "name": "n"}
                                }
                            },
                            "kind": "equivalence",
                            "semantic_category": "equivalence",
                            "typed_transcription": {
                                "status": "transcribed"
                            },
                            "formalization_target": {
                                "lean_declaration": "thm_structured",
                                "expected_shape": "theorem"
                            }
                        },
                        {
                            "id": "thm_gap",
                            "formalization_class": "formal_math",
                            "statement": "The text is theorem-like, but Phase 0 cannot parse a statement.",
                            "theorem_ir": {
                                "assumptions": [],
                                "binders": [],
                                "conclusion": {
                                    "kind": "unknown_prop",
                                    "text": "statement not structurally typed"
                                }
                            },
                            "kind": "theorem",
                            "semantic_category": "plain_theorem",
                            "typed_transcription": {
                                "status": "partial"
                            },
                            "formalization_target": {
                                "lean_declaration": "thm_gap",
                                "expected_shape": "theorem"
                            }
                        }
                    ],
                    "definitions": [],
                    "assumptions": [],
                    "supporting_equations": [],
                    "paper_math_sources": {}
                }
            }),
        );

        let run = deterministic_haskell_semantic_model_agent_run(task.author_role, &task, &base)
            .expect("deterministic haskell run");
        let code = run.output["code"].as_str().expect("code output");

        assert!(code.contains("data SemanticCategory"));
        assert!(code.contains("data FormalizationClass"));
        assert!(code.contains("data TranscriptionStatus"));
        assert!(!code.contains("data ReviewCategory"));
        assert!(!code.contains("claimReviewCategory"));
        assert!(!code.contains("RCSummary"));
        assert!(code.contains("theoremSemanticCategory = SemCatPlainTheorem"));
        assert!(code.contains("theoremFormalizationClass = FormalMath"));
        assert!(code.contains("theoremTranscriptionStatus = StatusPartial"));
        assert!(code.contains("claimSemanticCategory :: SemanticCategory"));
        assert!(code.contains("SemCatPlainTheorem (Just"));
        assert!(code.contains("categoryToObligations :: ClaimIR -> [ProofObligation]"));
        assert!(code.contains("categoryToObligations = claimToObligations"));
        assert!(!code.contains("isNonFormalReview"));
        assert!(code.contains("SemanticGap span \"statement not structurally typed\""));
        assert!(!code.contains("UninterpretedPredicate \"unknown_prop\" []"));
        assert!(code.contains("isProofReadyConclusion :: Proposition -> Bool"));
        assert!(code.contains("isProofReadyConclusion (SemanticGap _ _) = False"));
        assert!(code.contains("theoremTranscriptionStatus theorem == StatusTranscribed"));
        assert!(code.contains("FormalMath | isProofReadyTheorem theorem ->"));
        assert!(code.contains("(theoremConclusion theorem)"));
        assert!(code.contains("allProofObligations = concatMap categoryToObligations claims"));
        assert!(grokrxiv_review_loop::validate_generated_code("haskell", code, &base).is_empty());
    }

    #[test]
    fn review_loop_deterministic_haskell_author_does_not_emit_obligations_for_partial_semantic_gaps(
    ) {
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };
        let base = compact_review_fix_code_base_artifact(
            &task,
            serde_json::json!({
                "semantic_ir": {
                    "schema_version": "1.0.0",
                    "theorem_candidates": [
                        {
                            "id": "thm_structured",
                            "formalization_class": "formal_math",
                            "statement": "For all n, n + 0 = n.",
                            "theorem_ir": {
                                "assumptions": [],
                                "binders": [],
                                "conclusion": {
                                    "kind": "equals",
                                    "lhs": {"kind": "var", "name": "n + 0"},
                                    "rhs": {"kind": "var", "name": "n"}
                                }
                            },
                            "typed_transcription": {
                                "status": "transcribed"
                            },
                            "formalization_target": {
                                "lean_declaration": "thm_structured",
                                "expected_shape": "theorem"
                            }
                        },
                        {
                            "id": "body_math_41",
                            "formalization_class": "formal_math",
                            "statement": "\\newblock LeanDojo: Theorem Proving with Retrieval-Augmented Language Models",
                            "theorem_ir": {
                                "assumptions": [],
                                "binders": [],
                                "conclusion": {
                                    "kind": "unknown_prop",
                                    "text": "\\newblock LeanDojo: Theorem Proving with Retrieval-Augmented Language Models"
                                }
                            },
                            "typed_transcription": {
                                "status": "partial"
                            },
                            "formalization_target": {
                                "lean_declaration": "body_math_41",
                                "expected_shape": "theorem"
                            }
                        }
                    ],
                    "definitions": [],
                    "assumptions": [],
                    "supporting_equations": [],
                    "paper_math_sources": {}
                }
            }),
        );

        let run = deterministic_haskell_semantic_model_agent_run(task.author_role, &task, &base)
            .expect("deterministic haskell run");
        let code = run.output["code"].as_str().expect("code output");

        assert!(code.contains("isProofReadyTheorem :: TheoremIR -> Bool"));
        assert!(code.contains("theoremTranscriptionStatus theorem == StatusTranscribed"));
        assert!(code.contains("isProofReadyConclusion (SemanticGap _ _) = False"));
        assert!(code.contains("FormalMath | isProofReadyTheorem theorem ->"));
        assert!(
            !code.contains("FormalMath ->\n          [ ProofObligation"),
            "formal math must not bypass transcription/conclusion readiness checks"
        );
        assert!(grokrxiv_review_loop::validate_generated_code("haskell", code, &base).is_empty());
    }

    #[test]
    fn review_loop_deterministic_haskell_author_keeps_empty_targets_when_ir_has_only_limitations() {
        let task = ReviewFixCodeTask {
            target_id: "haskell",
            language: "haskell",
            filename: "SemanticModel.hs",
            author_role: "haskell_semantic_author",
            reviewer_role: "haskell_code_reviewer",
            fixer_role: "haskell_code_fixer",
            compile_program: "ghc",
            compile_args: vec!["-fno-code".to_string(), "SemanticModel.hs".to_string()],
            compile_timeout_secs: 900,
            forbidden_terms: Vec::new(),
            max_attempts: 2,
        };
        let base = compact_review_fix_code_base_artifact(
            &task,
            serde_json::json!({
                "claims": {
                    "claims": [
                        {"id": "claim_1", "statement": "First Lean 4 proof of zeta(3) irrationality."}
                    ]
                },
                "knowledge_graph": {
                    "nodes": [
                        {"id": "claim_1", "label": "First Lean 4 proof of zeta(3) irrationality."}
                    ],
                    "edges": []
                },
                "semantic_ir": {
                    "schema_version": "1.0.0",
                    "theorem_candidates": [],
                    "definitions": [],
                    "assumptions": [],
                    "supporting_equations": [],
                    "nonformal_review_claims": [
                        {"id": "claim_1", "statement": "First Lean 4 proof of zeta(3) irrationality."}
                    ],
                    "limitations": [
                        {
                            "id": "no_paper_math_transcribed",
                            "kind": "semantic_gap",
                            "statement": "No paper-derived theorem sources were transcribed into typed IR.",
                            "source_span": {
                                "artifact": "paper_math_sources",
                                "claim_id": "paper_math_sources"
                            }
                        }
                    ]
                }
            }),
        );

        let run = deterministic_haskell_semantic_model_agent_run(task.author_role, &task, &base)
            .expect("deterministic haskell run");
        let code = run.output["code"].as_str().expect("code output");

        assert!(code.contains("data Limitation"));
        assert!(code.contains("limitations :: [Limitation]"));
        assert!(code.contains("no_paper_math_transcribed"));
        assert!(code.contains("theoremTargets =\n  []"));
        assert!(code.contains("claims =\n  []"));
        assert!(!code.contains("First Lean 4 proof of zeta(3) irrationality"));
        assert!(!code.contains("ReviewCategory"));
        assert!(grokrxiv_review_loop::validate_generated_code("haskell", code, &base).is_empty());
    }

    #[test]
    fn review_loop_contract_files_define_formalization_policy_surface() {
        let claim_graph = include_str!("../../../schemas/claim_graph.schema.json");
        let semantic_ir = include_str!("../../../schemas/semantic_ir.schema.json");
        let lean_verification = include_str!("../../../schemas/lean_verification.schema.json");
        let research_bundle = include_str!("../../../schemas/research_bundle.schema.json");
        let review_score = include_str!("../../../schemas/review_score.schema.json");
        let release_tiers = include_str!("../../../policies/release_tiers.yaml");
        let repair_policy = include_str!("../../../policies/repair_policy.yaml");

        assert!(claim_graph.contains("claim_kind"));
        assert!(claim_graph.contains("supports"));
        assert!(semantic_ir.contains("theorem_candidates"));
        assert!(semantic_ir.contains("supporting_equations"));
        assert!(semantic_ir.contains("formalization_target"));
        assert!(lean_verification.contains("USES_UNAPPROVED_AXIOM"));
        assert!(research_bundle.contains("code_artifacts"));
        assert!(review_score.contains("semantic_adequacy"));
        assert!(release_tiers.contains("formally_verified"));
        assert!(repair_policy.contains("requires_escalation"));
    }

    #[test]
    fn review_loop_code_task_schema_rejects_extra_fields() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/review_loop_code_task.schema.json"
        ))
        .expect("schema json");
        let validator = jsonschema::validator_for(&schema).expect("compiled schema");

        let valid_generation = serde_json::json!({
            "phase": "generate",
            "target": "haskell",
            "language": "haskell",
            "filename": "SemanticModel.hs",
            "attempt": 1,
            "max_attempts": 2,
            "base": {},
            "previous_code": null,
            "previous_compile": null,
            "previous_codex_review": null,
            "harness": {"path": "/tmp/harness", "branch": "review-loop/haskell/abc123"}
        });
        validator
            .validate(&valid_generation)
            .expect("valid generation task");

        let valid_review = serde_json::json!({
            "phase": "review",
            "target": "lean",
            "language": "lean",
            "filename": "GrokRxiv/Proofs.lean",
            "attempt": 1,
            "max_attempts": 2,
            "code": "theorem t : True := by trivial",
            "compile": {"status": "pass"},
            "semantic_validation": {"status": "pass", "issues": []},
            "forbidden_terms": [],
            "base": {},
            "harness": {"path": "/tmp/harness", "branch": "review-loop/lean/abc123"}
        });
        validator
            .validate(&valid_review)
            .expect("valid review task");

        let mut invalid = valid_generation;
        invalid["undeclared"] = serde_json::json!("must fail");
        assert!(
            validator.validate(&invalid).is_err(),
            "review_loop_code_task schema must be closed"
        );
    }

    #[test]
    fn review_fix_loop_agent_output_audit_summary_counts_rejected_outputs() {
        let results = serde_json::json!({
            "attempts": [
                {
                    "attempt": 1,
                    "agent_output_audits": [
                        {"role": "haskell_semantic_author", "decision": {"status": "accepted"}},
                        {"role": "haskell_code_reviewer", "decision": {"status": "rejected"}}
                    ]
                },
                {
                    "attempt": 2,
                    "agent_output_audits": [
                        {"role": "haskell_code_fixer", "decision": {"status": "accepted"}}
                    ]
                }
            ]
        });

        let summary = review_fix_loop_agent_output_audit_summary(&results);

        assert_eq!(summary["total"], 3);
        assert_eq!(summary["accepted"], 2);
        assert_eq!(summary["rejected"], 1);
        assert_eq!(summary["by_role"]["haskell_code_reviewer"]["rejected"], 1);
    }

    #[test]
    fn skipped_lean_review_fix_code_reports_not_proved_semantic_gap() {
        let task = ReviewFixCodeTask {
            target_id: "lean",
            language: "lean",
            filename: "GrokRxiv/Proofs.lean",
            author_role: "lean_proof_author",
            reviewer_role: "lean_code_reviewer",
            fixer_role: "lean_code_fixer",
            compile_program: "lake",
            compile_args: vec![
                "env".to_string(),
                "lean".to_string(),
                "GrokRxiv/Proofs.lean".to_string(),
            ],
            compile_timeout_secs: 1800,
            forbidden_terms: vec!["sorry", "admit", "axiom"],
            max_attempts: 2,
        };
        let proof_obligations = serde_json::json!({
            "obligations": [
                {
                    "id": "semantic_gap_haskell_model_failed",
                    "kind": "semantic_gap",
                    "statement": "Haskell mathematical IR generation did not pass; Lean verification is blocked.",
                    "lean_declaration": null,
                    "source_claim_id": null
                }
            ]
        });

        let skipped = skipped_review_fix_code_results(
            &task,
            std::path::Path::new("review_loop/lean/GrokRxiv/Proofs.lean"),
            "Haskell mathematical IR generation did not pass; Lean verification is blocked.",
        );
        let results = annotate_lean_review_fix_code_results(skipped, &proof_obligations);

        assert_eq!(results["status"], "fail");
        assert_eq!(results["verdict"], "NOT_PROVED");
        assert_eq!(results["proof_status"], "SEMANTIC_GAP");
        assert_eq!(results["entries"][0]["status"], "SEMANTIC_GAP");
        assert!(review_fix_loop_summary(&results).contains("verdict=NOT_PROVED"));
    }

    #[test]
    fn failed_lean_review_fix_code_reports_not_proved_type_error() {
        let proof_obligations = serde_json::json!({
            "obligations": [
                {
                    "id": "formalize_false_claim",
                    "kind": "theorem_formalization",
                    "statement": "A false theorem candidate.",
                    "lean_declaration": "false_claim",
                    "source_claim_id": "false_claim"
                }
            ]
        });
        let results = serde_json::json!({
            "status": "fail",
            "attempts": [
                {
                    "attempt": 2,
                    "generation": {
                        "code": "namespace GrokRxiv\n\ntheorem false_claim : True := by\n  skip\n\nend GrokRxiv\n"
                    },
                    "compile": {
                        "status": "fail",
                        "stdout": "GrokRxiv/Proofs.lean:3:32: error: unsolved goals\n⊢ True\n",
                        "stderr": ""
                    },
                    "codex_review": {
                        "status": "fail",
                        "issues": [
                            {
                                "severity": "blocking",
                                "message": "Do not replace this with sorry."
                            }
                        ]
                    }
                }
            ]
        });

        let annotated = annotate_lean_review_fix_code_results(results, &proof_obligations);

        assert_eq!(annotated["status"], "fail");
        assert_eq!(annotated["verdict"], "NOT_PROVED");
        assert_eq!(annotated["proof_status"], "TYPE_ERROR");
        assert_eq!(annotated["entries"][0]["status"], "TYPE_ERROR");
        assert!(review_fix_loop_summary(&annotated).contains("verdict=NOT_PROVED"));
    }

    #[test]
    fn review_extracted_existing_review_notice_is_operator_friendly() {
        let paper_id = Uuid::parse_str("7dad48b0-d4db-44dc-97f9-7caf25258b81").unwrap();
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();
        let pr_url = "https://github.com/GrokRxiv/grokrxiv-reviews/pull/19";

        let text = existing_review_text(paper_id, "2605.00561", review_id, "pr_open", Some(pr_url));
        assert!(text.contains("already_reviewed=true"));
        assert!(text.contains("review_status=pr_open"));
        assert!(text.contains("pr_url=https://github.com/GrokRxiv/grokrxiv-reviews/pull/19"));
        assert!(text.contains(
            "show_command=agh app run grokrxiv show 03c0843f-80f8-46b4-8d7a-ad7292c449f8"
        ));
        assert!(
            text.contains("force_command=agh app run grokrxiv review-extracted --force 2605.00561")
        );

        let json = existing_review_json(paper_id, "2605.00561", review_id, "pr_open", Some(pr_url));
        assert_eq!(json["status"], "already_reviewed");
        assert_eq!(json["review_status"], "pr_open");
        assert_eq!(json["pr_url"], pr_url);
    }

    #[test]
    fn cli_parses_agh_grokrxiv_list_args() {
        let listed = Cli::try_parse_from([
            "agh",
            "app",
            "run",
            "grokrxiv",
            "--",
            "list",
            "extracted",
            "--limit",
            "50",
        ])
        .unwrap();
        match listed.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(args, vec!["list", "extracted", "--limit", "50"])
            }
            other => panic!("expected agh app run grokrxiv list extracted, got {other:?}"),
        }

        let reviews = Cli::try_parse_from([
            "agh",
            "--json",
            "app",
            "run",
            "grokrxiv",
            "--",
            "list",
            "reviews",
            "--review-status",
            "awaiting_moderation",
        ])
        .unwrap();
        assert!(reviews.json);
        match reviews.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec!["list", "reviews", "--review-status", "awaiting_moderation"]
                );
            }
            other => panic!("expected agh app run grokrxiv list reviews, got {other:?}"),
        }
    }

    #[test]
    fn citation_evidence_entry_formats_concise_human_text() {
        let entry = serde_json::json!({
            "raw": "cohenCubicalTypeTheory2018: title = {Cubical {{Type Theory}}: {{A Constructive Interpretation}} of the {{Univalence Axiom}}}, author = {Cohen, Cyril and Coquand, Thierry and Huber, Simon and M{\\\"o}rtberg, Anders}, year = {2018}, doi = {10.4230/LIPICS.TYPES.2015.5}, abstract = {Long abstract should not appear.}",
            "status": "unverified",
            "source": "crossref_bibliographic",
            "reason": "no bibliographic match above score threshold",
            "doi": "10.4230/LIPICS.TYPES.2015.5",
            "title": "Cubical Type Theory: A Constructive Interpretation of the Univalence Axiom"
        });

        let item = CitationEvidenceItem::from_verifier_entry(&entry).expect("evidence item");
        let text = item.to_human_line();

        assert!(text.contains("cohenCubicalTypeTheory2018"));
        assert!(text.contains("Cubical Type Theory"));
        assert!(text.contains("2018"));
        assert!(text.contains("needs verification"));
        assert!(text.contains("10.4230/LIPICS.TYPES.2015.5"));
        assert!(!text.contains("Long abstract"));
        assert!(text.len() < 280, "{text}");
    }

    #[test]
    fn citation_verifier_summary_surfaces_retracted_entries() {
        let entry = CitationEvidenceItem::from_verifier_entry(&serde_json::json!({
            "raw": "majorana2018: title = {Quantized inertia}, year = {2018}, doi = {10.retracted/example}",
            "status": "retracted",
            "source": "crossref_retraction",
            "reason": "updated-by type=retraction doi=10.notice/retraction source=retraction-watch",
            "doi": "10.retracted/example",
            "title": "Quantized inertia"
        }))
        .expect("retraction evidence item");
        let summary = CitationVerifierSummary {
            verifier_status: Some("fail".to_string()),
            checked: 1,
            coverage_status: None,
            reason: None,
            unresolved: 0,
            retracted: 1,
            unverified: 0,
            unknown: 0,
            malformed: 0,
            unresolved_fraction: 1.0,
            evidence: vec![entry],
            artifact_hint: "bundle.zip agents/citation.json".to_string(),
        };

        let text = summary.to_markdown();

        assert!(text.contains("retracted=1"), "{text}");
        assert!(text.contains("retracted"), "{text}");
        assert!(text.contains("10.notice/retraction"), "{text}");
    }

    #[test]
    fn feedback_loop_pr_without_correction_marker_requires_refresh() {
        let stale_body = "\
Needs revision.

grokrxiv-review-id: 11111111-1111-1111-1111-111111111111
";
        assert!(pr_body_needs_revision_refresh(stale_body));

        let fresh_body = "\
Needs revision.

grokrxiv-correction-source-path: corrections/source/paper.tex
grokrxiv-review-id: 11111111-1111-1111-1111-111111111111
";
        assert!(!pr_body_needs_revision_refresh(fresh_body));
    }

    #[test]
    fn smoke_edit_plan_uses_sidecar_for_pdf_and_inline_comment_for_tex() {
        let tex = smoke_edit_plan("corrections/source/paper.tex").expect("tex plan");
        assert_eq!(tex.edit_path, PathBuf::from("corrections/source/paper.tex"));
        assert!(tex
            .git_add_paths
            .contains(&PathBuf::from("corrections/source/paper.tex")));
        assert!(tex
            .payload
            .contains("% GrokRxiv feedback-loop smoke correction"));

        let pdf = smoke_edit_plan("corrections/source/paper.pdf").expect("pdf plan");
        assert_eq!(
            pdf.edit_path,
            PathBuf::from("corrections/source/grokrxiv-smoke-trigger.md")
        );
        assert!(pdf.git_add_paths.contains(&PathBuf::from(
            "corrections/source/grokrxiv-smoke-trigger.md"
        )));
        assert!(!pdf
            .git_add_paths
            .contains(&PathBuf::from("corrections/source/paper.pdf")));
        assert!(pdf.payload.contains("PDF-backed correction PR"));
    }

    #[test]
    fn cli_parses_agh_grokrxiv_git_corpus_review_options() {
        let cli = Cli::try_parse_from([
            "agh",
            "--runner",
            "cli",
            "--extractor",
            "cli",
            "app",
            "run",
            "grokrxiv",
            "--",
            "review",
            "https://github.com/MagnetonIO/emergent_spacetime",
            "--type",
            "git",
            "--rev",
            "main",
            "--corpus",
            "--scan-root",
            "papers/information-theory/src",
            "--limit",
            "1",
            "--include",
            "*.tex",
            "--exclude",
            "target/**",
        ])
        .expect("git corpus review command should parse");

        match cli.command {
            Command::App {
                command: AppCommand::Run { app, args },
            } => {
                assert_eq!(app, "grokrxiv");
                assert_eq!(
                    args,
                    vec![
                        "review",
                        "https://github.com/MagnetonIO/emergent_spacetime",
                        "--type",
                        "git",
                        "--rev",
                        "main",
                        "--corpus",
                        "--scan-root",
                        "papers/information-theory/src",
                        "--limit",
                        "1",
                        "--include",
                        "*.tex",
                        "--exclude",
                        "target/**",
                    ]
                );
            }
            other => panic!("expected agh app run grokrxiv review, got {other:?}"),
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

    #[cfg(feature = "grokrxiv-storage")]
    #[test]
    fn paper_math_source_collector_uses_data_repo_cache_when_asset_pointer_not_ready() {
        let repo = tempfile::tempdir().unwrap();
        let arxiv_id = "2606.00799";
        let rel = |file: &str| format!("papers/{arxiv_id}/{file}");
        let paper_dir = repo.path().join("papers").join(arxiv_id);
        std::fs::create_dir_all(&paper_dir).unwrap();
        std::fs::write(
            paper_dir.join("review_input.json"),
            serde_json::to_vec(&grokrxiv_storage::ReviewInput {
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
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("metadata.json"),
            r#"{"title":"Weyl-type test","abstract":"A real abstract.","authors":[]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("body.md"),
            "## Main\n\n\\begin{theorem} Let x=x. \\end{theorem}\n",
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("sections.json"),
            r#"{"sections":[{"heading":"Main","body_markdown":"A theorem section."}]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("equations.json"),
            r#"{"equations":[{"id":"eq1","canonical_tex":"x=x"},{"id":"eq2","canonical_tex":"y=y"}]}"#,
        )
        .unwrap();
        std::fs::write(
            paper_dir.join("theorem_graph.json"),
            r#"{"nodes":[{"id":"thm-main","kind":"theorem","statement":"Let x=x."}]}"#,
        )
        .unwrap();
        std::fs::write(paper_dir.join("references.json"), r#"{"citations":[]}"#).unwrap();
        std::fs::write(paper_dir.join("extraction_report.json"), r#"{"stages":[]}"#).unwrap();

        let files =
            load_review_loop_paper_math_sources_from_data_repo_cache(repo.path(), "2606.00799v1")
                .unwrap()
                .expect("cache should load by base arxiv id");

        assert_eq!(
            files
                .body
                .get("sections")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            files
                .equations
                .get("equations")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            files
                .theorem_graph
                .get("nodes")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert!(files
            .artifact_sources
            .iter()
            .any(|source| source.contains("review_input:")));
    }

    #[test]
    fn extraction_report_failed_stage_is_audit_failure() {
        let report = serde_json::json!({
            "stages": [
                {"name": "source_to_body", "status": "failed"}
            ]
        });
        let mut warnings = Vec::new();
        let mut failures = Vec::new();

        audit_extraction_report_provenance(&report, &mut warnings, &mut failures);

        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(failures
            .iter()
            .any(|msg| msg == "extraction stage source_to_body failed"));
    }

    #[test]
    fn review_pr_close_plan_rejects_non_github_urls() {
        let plan =
            review_pr_close_plan(Some("https://github.com/GrokRxiv/grokrxiv-reviews/pull/42"))
                .expect("valid GitHub PR URL")
                .expect("close plan");
        assert_eq!(plan.owner, "GrokRxiv");
        assert_eq!(plan.repo, "grokrxiv-reviews");
        assert_eq!(plan.pr_number, 42);

        assert!(review_pr_close_plan(Some("SIMULATED-123"))
            .expect("simulated PR URL is skipped")
            .is_none());
        assert!(review_pr_close_plan(Some("https://example.com/not-a-pr")).is_err());
    }

    #[test]
    fn citation_summary_zero_checked_is_not_reviewed() {
        let summary = CitationVerifierSummary {
            verifier_status: Some("fail".to_string()),
            checked: 0,
            coverage_status: Some("not_checked".to_string()),
            reason: Some(
                "No extracted bibliography entries were available for external citation verification."
                    .to_string(),
            ),
            unresolved: 0,
            retracted: 0,
            unverified: 0,
            unknown: 0,
            malformed: 0,
            unresolved_fraction: 0.0,
            evidence: vec![],
            artifact_hint: ".agenthero/artifacts/grokrxiv/reviews/review-id/bundle.zip agents/citation.json".to_string(),
        };
        let markdown = summary.to_markdown();
        assert!(markdown.contains("not externally checked"));
        assert!(!markdown.contains("fail_fraction=0.000"));
    }

    #[test]
    fn review_pr_dispatch_uses_revision_pr_for_non_pass_gates() {
        let clean = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Pass,
            reason: "ok".to_string(),
            recommendation: "accept".to_string(),
        };
        assert_eq!(
            review_pr_dispatch_kind(&clean),
            ReviewPrDispatchKind::Publication
        );

        let warned = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Warn,
            reason: "blocked roles remain".to_string(),
            recommendation: "accept".to_string(),
        };
        assert_eq!(
            review_pr_dispatch_kind(&warned),
            ReviewPrDispatchKind::RevisionNeeded
        );

        let failed = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Fail,
            reason: "Meta-review recommendation is `major_revision`, not `accept`.".to_string(),
            recommendation: "major_revision".to_string(),
        };
        assert_eq!(
            review_pr_dispatch_kind(&failed),
            ReviewPrDispatchKind::RevisionNeeded
        );
    }

    #[test]
    fn review_pr_dispatch_disabled_external_actions_do_not_plan_pr_side_effect() {
        let failed = crate::review_gate::PublicationGate {
            verdict: crate::review_gate::GateVerdict::Fail,
            reason: "Meta-review recommendation is `major_revision`, not `accept`.".to_string(),
            recommendation: "major_revision".to_string(),
        };

        let outcome = review_pr_dispatch_skipped_by_policy(&failed);

        assert_eq!(outcome.kind, ReviewPrDispatchKind::RevisionNeeded);
        assert_eq!(outcome.pr_url, None);
        assert!(!outcome.external_actions_enabled);
        assert_eq!(outcome.gate_verdict, crate::review_gate::GateVerdict::Fail);
    }
}
