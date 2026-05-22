//! `grokrxiv` CLI surface.
//!
//! The binary's `main()` dispatches to one of the subcommands below. Each
//! variant delegates to a small function so the library/HTTP path and the
//! CLI path call the same plumbing — no duplication.

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use grokrxiv_dag_runtime::{
    AgentKind, DagEdge, DagManifest, DagNode, DagNodeKind, DagRole, DagTool, OneOrMany, RoleId,
    ToolExecutorKind,
};
use serde::{Deserialize, Serialize};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;

use crate::agents::config as agent_config;
use crate::agents::{AgentMode, AgentRunnerKind, RevisionTarget, SandboxPolicy};
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

/// GrokRxiv — agentic peer-review pipeline for arXiv.
#[derive(Debug, Parser)]
#[command(
    name = "grokrxiv",
    version,
    about = "GrokRxiv — agentic peer-review pipeline for arXiv",
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
    /// Cloud agent provider (e.g. `vercel_open_agents`, `e2b`).
    #[arg(long, global = true, hide = true)]
    pub cloud_provider: Option<String>,
    /// LiteLLM gateway URL (overrides env).
    #[arg(long, global = true, hide = true)]
    pub litellm_url: Option<String>,
    /// Ollama host (overrides env).
    #[arg(long, global = true, hide = true)]
    pub ollama_host: Option<String>,
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
    /// Path to the TOML config file. Defaults to `~/.grokrxiv/config.toml`.
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

/// Hint for `grokrxiv review <source>` when the source can't be inferred.
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

    // ---------- ingestion ----------
    /// Synchronously ingest + review one or more papers.
    #[command(hide = true)]
    Ingest {
        /// arXiv IDs (e.g. `2605.12484`).
        #[arg(required = true)]
        arxiv_ids: Vec<String>,
        /// Phase 5: after the review reaches `awaiting_moderation`, open the
        /// human-review PR. Clean gates use the publication PR body; warn/fail
        /// gates use the revision-needed PR body.
        #[arg(long)]
        auto_moderate: bool,
    },
    /// Fetch + extract one or more papers, validate reviewer input, then stop before review.
    Extract {
        /// arXiv IDs (e.g. `2605.12484`).
        #[arg(required = true)]
        arxiv_ids: Vec<String>,
    },
    /// Bulk OAI-PMH backfill across an arXiv date range.
    #[command(hide = true)]
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
    #[command(hide = true)]
    IngestDaily,

    /// Create and run scheduled arXiv review batches.
    Batch {
        /// Batch operation to run.
        #[command(subcommand)]
        command: BatchCommand,
    },

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
    /// The canonical end-to-end entry point. Runs the review and opens the
    /// GitHub PR used for human review.
    ///
    /// `source` can be:
    /// - an arXiv id (e.g. `2605.12484`),
    /// - an arXiv URL (`https://arxiv.org/abs/...` / `/pdf/...`),
    /// - a local `.tex` or `.pdf` manuscript,
    /// - a git repository containing a `.tex` or `.pdf` manuscript,
    /// - `@<path>` to read a newline-delimited file of review sources.
    ///
    /// arXiv, local `.tex`/`.pdf`, and git repository sources can run through
    /// the production review path when built with ingest support.
    Review {
        /// Source: arXiv id | URL | path | `-` | `@file`.
        source: String,
        /// Force the source kind when it can't be inferred (e.g. stdin).
        #[arg(long, value_enum)]
        r#type: Option<SourceType>,
        /// Git revision to review when `--type git` or a git source is inferred.
        #[arg(long)]
        rev: Option<String>,
        /// Relative PDF/TeX manuscript path inside a git repository.
        #[arg(long, value_name = "PATH")]
        paper_path: Option<PathBuf>,
        /// Optional title override for local file or git sources.
        #[arg(long)]
        title: Option<String>,
        /// Optional field/category override for local file or git sources.
        #[arg(long)]
        field: Option<String>,
        /// Scan a git repository/folder for every reviewable TeX/PDF
        /// manuscript and run one review per discovered paper.
        #[arg(long)]
        corpus: bool,
        /// Relative root inside the git repository to scan when `--corpus`.
        #[arg(long, value_name = "PATH")]
        scan_root: Option<PathBuf>,
        /// Maximum number of corpus manuscripts to review after de-duplication.
        #[arg(long)]
        limit: Option<usize>,
        /// Include glob for corpus scan. Repeatable; supports simple `*.tex`
        /// and `prefix/**` forms.
        #[arg(long)]
        include: Vec<String>,
        /// Exclude glob for corpus scan. Repeatable; supports simple `*.tex`
        /// and `prefix/**` forms.
        #[arg(long)]
        exclude: Vec<String>,
    },
    /// Re-run the review DAG against an already-ingested paper.
    #[command(hide = true)]
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
    #[command(hide = true)]
    Verify {
        /// UUID of the review to re-verify.
        review_id: Uuid,
    },
    /// Re-emit one or all artifacts for a persisted review.
    #[command(hide = true)]
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
    /// Refresh derived review metadata without rerunning agents.
    #[command(name = "refresh-review", hide = true)]
    RefreshReview {
        /// UUID of the review to refresh.
        review_id: Uuid,
    },

    // ---------- moderation (admin) ----------
    /// Merge the reviewed publication PR and publish/revalidate the web output.
    Approve {
        /// UUID of the review whose already-open PR should be merged and published.
        review_id: Uuid,
        /// Bypass the latest automated publication gate and merge anyway.
        #[arg(long)]
        force: bool,
    },
    /// Open a revision-needed PR for a failed automated review gate.
    #[command(alias = "needs-revision")]
    RequestRevisions {
        /// UUID of the review that needs author revisions.
        review_id: Uuid,
        /// Optional moderator note to include in the PR body.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Destructive live smoke for the GitHub correction feedback loop.
    #[command(hide = true)]
    FeedbackLoopSmoke {
        /// Prior review id whose revision PR should receive a smoke commit.
        review_id: Uuid,
        /// Maximum time to wait for the webhook-driven re-review.
        #[arg(long, default_value_t = 3600)]
        max_wait_secs: u64,
    },
    /// Deprecated alias for `approve`.
    #[command(hide = true, alias = "merge")]
    Publish {
        /// UUID of the review whose PR should be merged.
        review_id: Uuid,
        /// Bypass the latest automated gate and merge anyway.
        #[arg(long)]
        force: bool,
    },
    /// Re-run the post-render html_quality harness on an already-rendered
    /// review. Reads `artifacts/<review_id>/review.html`, runs codex (gpt-5.5)
    /// to fix formatting issues, writes the corrected HTML back, and persists
    /// `formatting_fixes.json` sidecar.
    #[command(hide = true)]
    HtmlReview {
        /// UUID of the review whose review.html should be audited + fixed.
        /// Required unless `--all` is set.
        review_id: Option<Uuid>,
        /// Run the harness on every review with status in {pr_open, published,
        /// corrected}. Skips reviews whose artifact directory is missing.
        #[arg(long)]
        all: bool,
    },
    /// Close a review: hide it from web and close the linked GitHub PR.
    #[command(visible_alias = "remove-review")]
    Close {
        /// UUID of the review to close.
        review_id: Uuid,
        /// Human-readable reason recorded on the withdrawal row.
        #[arg(long)]
        reason: String,
        /// Leave the linked GitHub PR untouched.
        #[arg(long)]
        keep_github_pr: bool,
    },
    /// Mark a review rejected and publish the rejection rationale.
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
    #[command(hide = true)]
    Withdraw {
        /// UUID of the review to withdraw.
        review_id: Uuid,
        /// Reason recorded on the corrections row.
        #[arg(long)]
        reason: String,
    },
    /// Append a correction; status → corrected.
    #[command(hide = true)]
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
    /// Inspect queued, running, completed, and failed jobs.
    Jobs {
        /// Jobs operation to run.
        #[command(subcommand)]
        command: JobsCommand,
    },
    /// Deprecated alias for `jobs list`.
    #[command(hide = true)]
    TailJobs {
        /// Optional `kind` filter (e.g. `ingest`, `review`).
        #[arg(long)]
        kind: Option<String>,
        /// Optional `state` filter (e.g. `running`, `failed`).
        #[arg(long)]
        state: Option<String>,
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
        /// DAG type id to run, e.g. `c-to-rust`.
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
    // Export resolved per-role runner/model choices into the env vars the
    // supervisor reads while composing the agent registry.
    if let Some(rt) = runtime_cfg.as_ref() {
        // Always export `default_runner` so the supervisor can pick up the
        // CLI's `--runner` flag (the resolved RuntimeConfig already merges
        // CLI > ENV > TOML > default).
        let kind = rt.default_runner;
        if let Ok(s) = serde_json::to_string(&kind) {
            let bare = s.trim_matches('"');
            std::env::set_var("GROKRXIV_RUNNER_OVERRIDE", bare);
        }
        for role in crate::runtime_config::configured_review_agent_roles() {
            std::env::remove_var(role_model_override_env_var(&role));
        }
        for (role, kind) in &rt.runner_for {
            let role_slug = role_env_suffix(role);
            if let Ok(s) = serde_json::to_string(kind) {
                let bare = s.trim_matches('"');
                std::env::set_var(format!("GROKRXIV_RUNNER_OVERRIDE_{role_slug}"), bare);
            }
        }
        for (role, model) in &rt.model_for {
            std::env::set_var(role_model_override_env_var(role), model);
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
        command,
        Command::Review { .. }
            | Command::Ingest { .. }
            | Command::ReReview { .. }
            | Command::ReviewExtracted { .. }
            | Command::IngestRange { .. }
            | Command::IngestDaily
            | Command::Batch {
                command: BatchCommand::Run { .. },
            }
    );

    if dry_run {
        if let Command::Approve { review_id, .. } | Command::Publish { review_id, .. } = &command {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "dry_run": true,
                        "command": "approve",
                        "review_id": review_id,
                    }))?
                );
            } else {
                println!("dry_run=true command=approve review_id={review_id}");
            }
            return Ok(());
        }
    }

    let result = match command {
        Command::Serve => super::serve::run().await,
        Command::Doctor => {
            let code = doctor_mod::doctor(&profile, json).await?;
            if code != 0 {
                anyhow::bail!("doctor: one or more critical checks failed");
            }
            Ok(())
        }
        Command::Dag { command } => dag_command(command, json).await,
        Command::Agent { command } => agent_command(command, json).await,
        Command::Config {
            show_secrets: cmd_show,
        } => print_config(show_secrets || cmd_show, runtime_cfg.as_ref(), json),
        Command::Migrate => migrate().await,
        Command::Categories => print_categories(),
        Command::Ingest {
            arxiv_ids,
            auto_moderate,
        } => ingest_many(&arxiv_ids, auto_moderate, json).await,
        Command::Extract { arxiv_ids } => extract_many(&arxiv_ids, json).await,
        Command::IngestRange {
            from,
            to,
            categories,
            no_review,
        } => ingest_range(from, to, categories, no_review).await,
        Command::IngestDaily => ingest_daily().await,
        Command::Batch { command } => batch_command(command, dry_run, json).await,
        Command::List { what } => list(what).await,
        Command::Show { review_id, json } => show(review_id, json).await,
        Command::Review {
            source,
            r#type,
            rev,
            paper_path,
            title,
            field,
            corpus,
            scan_root,
            limit,
            include,
            exclude,
        } => {
            review_source(
                &source,
                r#type,
                ReviewSourceOptions {
                    rev,
                    paper_path,
                    title,
                    field,
                    corpus,
                    scan_root,
                    limit,
                    include,
                    exclude,
                },
                json,
                dry_run,
            )
            .await
        }
        Command::ReReview { paper_id } => review_paper(paper_id).await,
        Command::ReviewExtracted { source, force } => review_extracted(&source, force, json).await,
        Command::Verify { review_id } => verify(review_id).await,
        Command::Render {
            review_id,
            format,
            out,
        } => render(review_id, format, out).await,
        Command::RefreshReview { review_id } => refresh_review(review_id, json).await,
        Command::Approve { review_id, force } => approve(review_id, force, json).await,
        Command::RequestRevisions { review_id, notes } => {
            request_revisions(review_id, notes.as_deref(), json).await
        }
        Command::FeedbackLoopSmoke {
            review_id,
            max_wait_secs,
        } => feedback_loop_smoke(review_id, max_wait_secs, json).await,
        Command::Publish { review_id, force } => approve(review_id, force, json).await,
        Command::HtmlReview { review_id, all } => html_review_cmd(review_id, all, json).await,
        Command::Close {
            review_id,
            reason,
            keep_github_pr,
        } => close_review(review_id, &reason, keep_github_pr, json).await,
        Command::Reject { review_id, reason } => reject(review_id, &reason).await,
        Command::RequestChanges { review_id, notes } => request_changes(review_id, &notes).await,
        Command::Withdraw { review_id, reason } => withdraw(review_id, &reason).await,
        Command::Correct {
            review_id,
            rationale_md,
        } => correct(review_id, &rationale_md).await,
        Command::Open { review_id } => open_review(review_id),
        Command::Jobs { command } => jobs_command(command, json).await,
        Command::TailJobs { kind, state } => {
            jobs_command(
                JobsCommand::List {
                    kind,
                    state,
                    limit: 20,
                },
                json,
            )
            .await
        }
    };

    if let Some(dir) = debug_prompt_dir.as_ref() {
        if is_review_command {
            println!("debug_prompt_dir={}", dir.display());
        }
    }

    result
}

pub(crate) fn status_enabled_for_stderr(cli: &Cli, stderr_is_terminal: bool) -> bool {
    cli.status || (!cli.no_status && stderr_is_terminal)
}

fn emit_pipeline_header(command: &str, subject: &str) {
    let runner = std::env::var("GROKRXIV_RUNNER_OVERRIDE")
        .or_else(|_| std::env::var("GROKRXIV_RUNNER"))
        .unwrap_or_else(|_| "cli".to_string());
    let extractor = std::env::var("GROKRXIV_EXTRACTOR").unwrap_or_else(|_| "cli".to_string());
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
        crate::dag_apps::run_registered_dag_app(dag_type, grokrxiv_dag_executor::DagIo::default())
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
    let dags_dir = dags_dir();
    for manifest in &manifests {
        validate_declared_agent_configs(manifest, &dags_dir)?;
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

fn validate_declared_agent_configs(manifest: &DagManifest, dags_dir: &Path) -> anyhow::Result<()> {
    let repo_root = dags_dir.parent().unwrap_or_else(|| Path::new("."));
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
    let dags_dir = dags_dir();
    let mut paths = Vec::new();
    if let Some(id) = dag_type {
        paths.push(dag_manifest_path_in(&dags_dir, id));
    } else {
        for entry in std::fs::read_dir(&dags_dir)
            .with_context(|| format!("read DAG manifest directory {}", dags_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                paths.push(path);
            }
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
    dag_manifest_path_in(&dags_dir(), dag_type)
}

fn dag_manifest_path_in(dags_dir: &Path, dag_type: &str) -> PathBuf {
    dags_dir.join(format!("{dag_type}.yaml"))
}

fn resolve_agent_config_path(repo_root: &Path, config: &str) -> PathBuf {
    let path = PathBuf::from(config);
    if path.is_absolute() {
        return path;
    }
    if let Some(agents_dir) = std::env::var_os("GROKRXIV_AGENTS_DIR").map(PathBuf::from) {
        if let Ok(stripped) = path.strip_prefix("agents") {
            return agents_dir.join(stripped);
        }
    }
    repo_root.join(path)
}

fn dags_dir() -> PathBuf {
    if let Ok(path) = std::env::var("GROKRXIV_DAGS_DIR") {
        return PathBuf::from(path);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_dags = cwd.join("dags");
    if cwd_dags.is_dir() {
        return cwd_dags;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("dags")
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
    eprintln!("`migrate` is handled by `bash infra/supabase/setup.sh` in this checkout.");
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
                pr_url = Some(pr.pr_url);
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
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "arxiv_id": arxiv_id,
                    "paper_id": paper_id,
                    "review_id": review_id,
                    "pr_url": pr.pr_url,
                    "gate_verdict": pr.gate_verdict,
                    "recommendation": pr.recommendation,
                    "pr_kind": pr.kind.as_str(),
                }))?
            );
        } else {
            println!(
                "arxiv_id={arxiv_id} paper_id={paper_id} review_id={review_id} pr_url={}",
                pr.pr_url
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
        "show_command": format!("grokrxiv show {review_id}"),
        "force_command": format!("grokrxiv review-extracted --force {arxiv_id}"),
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
    out.push_str(&format!("show_command=grokrxiv show {review_id}\n"));
    out.push_str(&format!(
        "force_command=grokrxiv review-extracted --force {arxiv_id}\n"
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
        return Ok(vec![ResolvedSource::LocalFile(path, kind)]);
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
        return Ok(vec![ResolvedSource::LocalFile(path, kind)]);
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

/// Canonical end-to-end entry point — `grokrxiv review <source>`.
async fn review_source(
    source: &str,
    type_hint: Option<SourceType>,
    options: ReviewSourceOptions,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let resolved = resolve_source(source, type_hint).await?;
    let resolved = expand_corpus_sources(resolved, &options).await?;
    if dry_run {
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
                ResolvedSource::LocalFile(p, k) => serde_json::json!({
                    "kind": "local",
                    "path": p.display().to_string(),
                    "type": format!("{k:?}"),
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
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());
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
                    "paper {id}: review_id={review_id} opening GitHub review PR"
                ));
                let pr = open_review_pr_for_gate(&state, review_id, json, false).await?;
                let paper_id = paper_id_for_review(pool, review_id).await.ok();
                let envelope = review_result_envelope_with_pr(
                    review_result_envelope(pool, review_id, "arxiv", id, paper_id).await?,
                    &pr,
                );
                if !json {
                    println!(
                        "source_kind=arxiv source_id={id} paper_id={} review_id={review_id} pr_url={}",
                        envelope
                            .get("paper_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<unknown>"),
                        pr.pr_url
                    );
                }
                results.push(envelope);
            }
            ResolvedSource::LocalFile(path, kind) => {
                let spec = grokrxiv_ingest::ReviewSourceSpec::LocalFile {
                    path: path.clone(),
                    format: local_source_format(*kind),
                    title: options.title.clone(),
                    authors: Vec::new(),
                    field: options.field.clone(),
                };
                let (paper_id, review_id, source_kind, source_id) =
                    review_prepared_source(&state, spec).await?;
                let pr = open_review_pr_for_gate(&state, review_id, json, false).await?;
                if !json {
                    println!(
                        "source_kind={source_kind} source_id={source_id} paper_id={paper_id} review_id={review_id} pr_url={}",
                        pr.pr_url
                    );
                }
                let envelope = review_result_envelope_with_pr(
                    review_result_envelope(
                        pool,
                        review_id,
                        &source_kind,
                        &source_id,
                        Some(paper_id),
                    )
                    .await?,
                    &pr,
                );
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
                let pr = open_review_pr_for_gate(&state, review_id, json, false).await?;
                if !json {
                    println!(
                        "source_kind={source_kind} source_id={source_id} paper_id={paper_id} review_id={review_id} pr_url={}",
                        pr.pr_url
                    );
                }
                let envelope = review_result_envelope_with_pr(
                    review_result_envelope(
                        pool,
                        review_id,
                        &source_kind,
                        &source_id,
                        Some(paper_id),
                    )
                    .await?,
                    &pr,
                );
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

fn resolved_source_label(source: &ResolvedSource) -> String {
    match source {
        ResolvedSource::Arxiv(id) => id.clone(),
        ResolvedSource::LocalFile(path, _) => path.display().to_string(),
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
    pr_url: String,
    gate_verdict: crate::review_gate::GateVerdict,
    recommendation: String,
    kind: ReviewPrDispatchKind,
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
    }
    envelope
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
            "**Citation verifier:** checked={}, not_resolved={}, needs_review={}, unknown={}, malformed={}, fail_fraction={:.3}.\n\n\
             Full evidence is in `{}`.",
            self.checked,
            self.unresolved,
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
    let (unresolved, unverified, unknown, malformed, unresolved_fraction) =
        if entry_items.is_empty() {
            let unresolved = citation_notes
                .get("unresolved")
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
            let bad = unresolved + malformed;
            let unresolved_fraction = if definitive_total == 0 {
                0.0
            } else {
                bad as f64 / definitive_total as f64
            };
            (
                unresolved,
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
                "unresolved" | "unverified" | "transient_unknown" | "malformed"
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
        unverified,
        unknown,
        malformed,
        unresolved_fraction,
        evidence,
        artifact_hint: format!("artifacts/{review_id}/bundle.zip agents/{role}.json"),
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
        pr_url,
        gate_verdict: gate.verdict,
        recommendation: gate.recommendation,
        kind,
    })
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

async fn refresh_review(review_id: Uuid, json: bool) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("refresh-review: DATABASE_URL not configured"))?;

    let citation_rows_repaired = repair_zero_checked_citation_agents(pool, review_id).await?;
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

    let artifacts_refreshed = refresh_rendered_artifacts(&state, review_id).await?;
    revalidate_best_effort(&state, review_id).await;
    let github_feedback =
        refresh_gate_feedback_comment(&state, pool, review_id, github_pr_url.as_deref()).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "review_id": review_id,
                "citation_rows_repaired": citation_rows_repaired,
                "meta_review_updated": meta_review_updated,
                "artifacts_refreshed": artifacts_refreshed,
                "github_feedback": github_feedback,
            })
        );
    } else {
        println!(
            "refreshed={review_id} citation_rows_repaired={citation_rows_repaired} meta_review_updated={meta_review_updated} artifacts_refreshed={artifacts_refreshed} github_feedback={github_feedback}"
        );
    }
    Ok(())
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
) -> anyhow::Result<bool> {
    #[cfg(feature = "grokrxiv-render")]
    {
        super::supervisor::render_to_disk(state, review_id).await?;
        Ok(true)
    }
    #[cfg(not(feature = "grokrxiv-render"))]
    {
        let _ = (state, review_id);
        Ok(false)
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
             Use `grokrxiv request-revisions {review_id}`, `grokrxiv reject {review_id} --reason …`, \
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
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
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
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts found under artifacts/{review_id} — \
             re-run `grokrxiv ingest <arxiv_id>` to regenerate."
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
            "Opened by `grokrxiv review ...`.\n\n\
             **Automated gate:** Pass.\n\n\
             **Private review:** dashboard-only unless archived in the private reviews repo.\n\n\
             See linked artifacts in this PR; the rendered review.html is the human-readable preview."
        )
    } else {
        format!(
            "Opened by `grokrxiv review ...`.\n\n\
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
    let dir_local = std::path::PathBuf::from(format!("artifacts/{review_id}"));
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
    if files.is_empty() {
        anyhow::bail!(
            "no rendered artifacts found under artifacts/{review_id} — \
             re-run `grokrxiv review ...` to regenerate."
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
        "Opened by `grokrxiv request-revisions {review_id}`.\n\n\
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
        anyhow::anyhow!("review {review_id} has no github_pr_url; run `grokrxiv review ...` first")
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
             Push fixes to the PR and wait for re-review, or run `grokrxiv approve {review_id} --force`.",
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
             Re-run `grokrxiv review ...` with GITHUB_TOKEN set."
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

/// `grokrxiv html-review [<id>|--all]`. Re-runs the post-render html_quality
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
        let dir = std::path::PathBuf::from(format!("artifacts/{id}"));
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
    let comment = format!("Closed by `grokrxiv close {review_id}`.\n\nReason:\n\n{reason}");
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

/// `grokrxiv reject <REVIEW_ID> --reason TEXT`. Phase 4: rejection is a
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

/// `grokrxiv request-changes <REVIEW_ID> --notes TEXT`. Phase 3: record the
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_display::source_display_ref;
    use clap::{CommandFactory, Parser};

    #[test]
    fn bare_grokrxiv_prints_help_instead_of_serving() {
        let err = Cli::try_parse_from(["grokrxiv"]).expect_err("bare CLI should print help");
        let text = err.to_string();
        assert!(text.contains("Usage: grokrxiv"));
        assert!(text.contains("Commands:"));
        assert!(text.contains("review"));
        assert!(text.contains("serve"));
    }

    #[test]
    fn default_help_shows_operator_surface_only() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();

        for visible in [
            "serve",
            "doctor",
            "config",
            "review",
            "extract",
            "review-extracted",
            "batch",
            "list",
            "show",
            "approve",
            "request-revisions",
            "close",
            "reject",
            "request-changes",
            "open",
            "jobs",
        ] {
            assert!(
                help.contains(visible),
                "expected `{visible}` in help:\n{help}"
            );
        }

        for hidden in [
            "publish",
            "merge",
            "tail-jobs",
            "html-review",
            "feedback-loop-smoke",
            "migrate",
            "ingest-range",
            "ingest-daily",
        ] {
            assert!(
                !help.contains(&format!("\n  {hidden}")),
                "did not expect `{hidden}` in default help:\n{help}"
            );
        }
    }

    #[test]
    fn cli_parses_publish_and_hidden_merge_alias() {
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();

        let publish = Cli::try_parse_from(["grokrxiv", "publish", &review_id.to_string()])
            .expect("publish should parse");
        match publish.command {
            Command::Publish {
                review_id: parsed,
                force,
            } => {
                assert_eq!(parsed, review_id);
                assert!(!force);
            }
            other => panic!("expected publish command, got {other:?}"),
        }

        let merge = Cli::try_parse_from(["grokrxiv", "merge", &review_id.to_string(), "--force"])
            .expect("hidden merge alias should parse");
        match merge.command {
            Command::Publish {
                review_id: parsed,
                force,
            } => {
                assert_eq!(parsed, review_id);
                assert!(force);
            }
            other => panic!("expected publish command from hidden alias, got {other:?}"),
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
    async fn publish_dry_run_returns_before_db_or_github() {
        let review_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();
        let cli = Cli::try_parse_from(["grokrxiv", "--dry-run", "publish", &review_id.to_string()])
            .expect("publish --dry-run should parse");

        run(cli)
            .await
            .expect("publish --dry-run should not require DB or GitHub");
    }

    #[test]
    fn cli_parses_status_flags() {
        let status = Cli::try_parse_from(["grokrxiv", "--status", "doctor"]).unwrap();
        assert!(status.status);
        assert!(!status.no_status);
        assert!(!status.debug_logs);

        let no_status = Cli::try_parse_from(["grokrxiv", "--no-status", "doctor"]).unwrap();
        assert!(!no_status.status);
        assert!(no_status.no_status);

        let debug_logs = Cli::try_parse_from(["grokrxiv", "--debug-logs", "doctor"]).unwrap();
        assert!(debug_logs.debug_logs);

        let both = Cli::try_parse_from(["grokrxiv", "--status", "--no-status", "doctor"]);
        assert!(
            both.is_err(),
            "--status and --no-status must be mutually exclusive"
        );
    }

    #[test]
    fn json_foreground_runs_still_show_clean_status() {
        let cli = Cli::try_parse_from(["grokrxiv", "--json", "doctor"]).unwrap();
        assert!(status_enabled_for_stderr(&cli, true));
        assert!(!status_enabled_for_stderr(&cli, false));
    }

    #[test]
    fn no_status_suppresses_status_even_for_foreground_runs() {
        let cli = Cli::try_parse_from(["grokrxiv", "--no-status", "doctor"]).unwrap();
        assert!(!status_enabled_for_stderr(&cli, true));
    }

    #[test]
    fn explicit_status_forces_status_for_redirected_stderr() {
        let cli = Cli::try_parse_from(["grokrxiv", "--status", "--json", "doctor"]).unwrap();
        assert!(status_enabled_for_stderr(&cli, false));
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
            Command::Extract { arxiv_ids } => assert_eq!(arxiv_ids, vec!["2605.00561"]),
            other => panic!("expected extract command, got {other:?}"),
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
        let parsed = Cli::try_parse_from([
            "grokrxiv",
            "--json",
            "dag",
            "run",
            "--dag-type",
            "c-to-rust",
        ])
        .unwrap();

        match parsed.command {
            Command::Dag {
                command: DagCommand::Run { dag_type },
            } => assert_eq!(dag_type, "c-to-rust"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn cli_parses_agent_place_command() {
        let parsed = Cli::try_parse_from(["grokrxiv", "agent", "place", "agent.yaml"]).unwrap();

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
            "grokrxiv",
            "--json",
            "batch",
            "create",
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
            Command::Batch {
                command:
                    BatchCommand::Create {
                        category,
                        month,
                        daily_limit,
                        max_items,
                        auto_pr,
                        ..
                    },
            } => {
                assert_eq!(category, "math");
                assert_eq!(month, "2026-05");
                assert_eq!(daily_limit, 30);
                assert_eq!(max_items, Some(15));
                assert!(auto_pr);
            }
            other => panic!("expected batch create, got {other:?}"),
        }

        let batch_id = Uuid::parse_str("03c0843f-80f8-46b4-8d7a-ad7292c449f8").unwrap();
        let run = Cli::try_parse_from([
            "grokrxiv",
            "batch",
            "run",
            &batch_id.to_string(),
            "--limit",
            "5",
        ])
        .expect("batch run should parse");
        match run.command {
            Command::Batch {
                command:
                    BatchCommand::Run {
                        batch_id: parsed,
                        limit,
                        ..
                    },
            } => {
                assert_eq!(parsed, batch_id);
                assert_eq!(limit, Some(5));
            }
            other => panic!("expected batch run, got {other:?}"),
        }

        let jobs = Cli::try_parse_from([
            "grokrxiv", "--json", "jobs", "list", "--kind", "review", "--state", "running",
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
            Command::ReviewExtracted { source, force } => {
                assert_eq!(source, "2605.00561");
                assert!(!force);
            }
            other => panic!("expected review-extracted command, got {other:?}"),
        }

        let forced =
            Cli::try_parse_from(["grokrxiv", "review-extracted", "--force", "2605.00561"]).unwrap();
        match forced.command {
            Command::ReviewExtracted { source, force } => {
                assert_eq!(source, "2605.00561");
                assert!(force);
            }
            other => panic!("expected forced review-extracted command, got {other:?}"),
        }
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
        assert!(text.contains("show_command=grokrxiv show 03c0843f-80f8-46b4-8d7a-ad7292c449f8"));
        assert!(text.contains("force_command=grokrxiv review-extracted --force 2605.00561"));

        let json = existing_review_json(paper_id, "2605.00561", review_id, "pr_open", Some(pr_url));
        assert_eq!(json["status"], "already_reviewed");
        assert_eq!(json["review_status"], "pr_open");
        assert_eq!(json["pr_url"], pr_url);
    }

    #[test]
    fn cli_parses_list_extracted_and_review_status_filter() {
        let listed =
            Cli::try_parse_from(["grokrxiv", "list", "extracted", "--limit", "50"]).unwrap();
        match listed.command {
            Command::List {
                what: ListKind::Extracted { limit, .. },
            } => assert_eq!(limit, 50),
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
            Command::List {
                what:
                    ListKind::Reviews {
                        review_status,
                        json,
                        ..
                    },
            } => {
                assert_eq!(review_status.as_deref(), Some("awaiting_moderation"));
                assert!(json);
            }
            other => panic!("expected list reviews command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_hidden_feedback_loop_smoke() {
        let review_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let cli = Cli::try_parse_from([
            "grokrxiv",
            "feedback-loop-smoke",
            &review_id.to_string(),
            "--max-wait-secs",
            "3600",
        ])
        .expect("hidden smoke command parses");
        match cli.command {
            Command::FeedbackLoopSmoke {
                review_id: parsed,
                max_wait_secs,
            } => {
                assert_eq!(parsed, review_id);
                assert_eq!(max_wait_secs, 3600);
            }
            other => panic!("expected FeedbackLoopSmoke, got {other:?}"),
        }

        let mut help = Vec::new();
        Cli::command().write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();
        assert!(!help.contains("feedback-loop-smoke"));
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
    fn cli_parses_git_corpus_review_options() {
        let cli = Cli::try_parse_from([
            "grokrxiv",
            "--runner",
            "cli",
            "--extractor",
            "cli",
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
            Command::Review {
                r#type,
                corpus,
                scan_root,
                limit,
                include,
                exclude,
                ..
            } => {
                assert_eq!(r#type, Some(SourceType::Git));
                assert!(corpus);
                assert_eq!(
                    scan_root,
                    Some(PathBuf::from("papers/information-theory/src"))
                );
                assert_eq!(limit, Some(1));
                assert_eq!(include, vec!["*.tex"]);
                assert_eq!(exclude, vec!["target/**"]);
            }
            other => panic!("expected Review command, got {other:?}"),
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
    fn approve_help_is_merge_and_publish() {
        let mut cmd = Cli::command();
        let approve = cmd
            .find_subcommand_mut("approve")
            .expect("approve subcommand exists");
        let mut help = Vec::new();
        approve.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("Merge the reviewed publication PR"));
        assert!(help.contains("publish"));
        assert!(!help.contains("does not merge or publish"));
    }

    #[test]
    fn close_help_describes_web_and_github_effects() {
        let mut cmd = Cli::command();
        let close = cmd
            .find_subcommand_mut("close")
            .expect("close subcommand exists");
        let mut help = Vec::new();
        close.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("hide it from web"));
        assert!(help.contains("close the linked GitHub PR"));
        assert!(help.contains("--keep-github-pr"));
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
            unverified: 0,
            unknown: 0,
            malformed: 0,
            unresolved_fraction: 0.0,
            evidence: vec![],
            artifact_hint: "artifacts/review-id/bundle.zip agents/citation.json".to_string(),
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
}
