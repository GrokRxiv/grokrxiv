//! `grokrxiv` CLI surface.
//!
//! The binary's `main()` dispatches to one of the subcommands below. Each
//! variant delegates to a small function so the library/HTTP path and the
//! CLI path call the same plumbing — no duplication.

use clap::{Parser, Subcommand};
use uuid::Uuid;

type PaperListRow = (
    Uuid,
    String,
    String,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
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
}

/// Top-level CLI subcommand variants.
#[derive(Debug, Subcommand)]
pub enum Command {
    // ---------- service ----------
    /// Run the HTTP API + tokio supervisor + scheduler (default).
    Serve,
    /// Print which env vars / external deps / DB / LLM providers are reachable.
    Doctor,
    /// Print the resolved orchestrator config. Secrets are redacted unless --show-secrets.
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
    /// Re-run the review DAG against an already-ingested paper.
    Review {
        /// UUID of the paper to re-review.
        paper_id: Uuid,
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
    /// Open the publication PR on `GrokRxiv/reviews`.
    Approve {
        /// UUID of the review to approve and publish.
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
        /// Optional status filter (e.g. `awaiting_moderation`).
        #[arg(long)]
        status: Option<String>,
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
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => super::serve::run().await,
        Command::Doctor => doctor().await,
        Command::Config { show_secrets } => print_config(show_secrets),
        Command::Migrate => migrate().await,
        Command::Categories => print_categories(),
        Command::Ingest { arxiv_ids } => ingest_many(&arxiv_ids).await,
        Command::IngestRange {
            from,
            to,
            categories,
            no_review,
        } => ingest_range(from, to, categories, no_review).await,
        Command::IngestDaily => ingest_daily().await,
        Command::List { what } => list(what).await,
        Command::Show { review_id, json } => show(review_id, json).await,
        Command::Review { paper_id } => review_paper(paper_id).await,
        Command::Verify { review_id } => verify(review_id).await,
        Command::Render {
            review_id,
            format,
            out,
        } => render(review_id, format, out).await,
        Command::Approve { review_id } => approve(review_id).await,
        Command::Reject { review_id, reason } => reject(review_id, &reason).await,
        Command::RequestChanges { review_id, notes } => request_changes(review_id, &notes).await,
        Command::Withdraw { review_id, reason } => withdraw(review_id, &reason).await,
        Command::Correct {
            review_id,
            rationale_md,
        } => correct(review_id, &rationale_md).await,
        Command::Open { review_id } => open_review(review_id),
        Command::TailJobs { kind, state } => tail_jobs(kind, state).await,
    }
}

// ---------------------------------------------------------------------------
// Subcommand implementations. Where the supporting plumbing already exists
// (serve, ingest one paper, approve) we wire through; the rest emit a clear
// "not yet implemented in stub build" message that points at the right task.
// ---------------------------------------------------------------------------

async fn doctor() -> anyhow::Result<()> {
    let cfg = super::Config::from_env();
    let ok = |b: bool| if b { "  ok " } else { "MISS " };
    let anthropic = std::env::var("ANTHROPIC_API_KEY").is_ok();
    let openai = std::env::var("OPENAI_API_KEY").is_ok();
    let gemini = std::env::var("GOOGLE_GENERATIVE_AI_API_KEY").is_ok();
    let database = std::env::var("DATABASE_URL").is_ok();

    println!("GrokRxiv doctor:");
    println!(
        "  [{}] ANTHROPIC_API_KEY              (required for /preview)",
        ok(anthropic)
    );
    println!(
        "  [{}] OPENAI_API_KEY                 (optional)",
        ok(openai)
    );
    println!(
        "  [{}] GOOGLE_GENERATIVE_AI_API_KEY   (optional)",
        ok(gemini)
    );
    println!(
        "  [{}] DATABASE_URL                   (required for persistence)",
        ok(database)
    );
    println!("  [{}] ORCHESTRATOR_BIND = {}", ok(true), cfg.bind);

    if !anthropic || !database {
        anyhow::bail!("doctor: required env vars missing");
    }
    Ok(())
}

fn print_config(show_secrets: bool) -> anyhow::Result<()> {
    let cfg = super::Config::from_env();
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

async fn ingest_many(arxiv_ids: &[String]) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    let supervisor = super::supervisor::Supervisor::spawn(state.clone());
    for id in arxiv_ids {
        let review_id = super::supervisor::run_one_paper_blocking(&supervisor, &state, id).await?;
        println!("arxiv_id={id} review_id={review_id}");
    }
    Ok(())
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
            status,
            limit,
            json,
            ..
        } => {
            let rows = crate::db::list_reviews(pool, status.as_deref(), limit as i64).await?;
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
        ListKind::Papers { json, limit, .. } => {
            let lim = limit as i64;
            let rows: Vec<PaperListRow> = sqlx::query_as(
                "select id, arxiv_id, title, field, ingested_at \
                 from papers order by ingested_at desc limit $1",
            )
            .bind(lim)
            .fetch_all(pool)
            .await?;
            if json {
                let v: Vec<_> = rows
                    .iter()
                    .map(|(id, arxiv, title, field, ts)| {
                        serde_json::json!({
                            "id": id,
                            "arxiv_id": arxiv,
                            "title": title,
                            "field": field,
                            "ingested_at": ts,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string(&v)?);
            } else if rows.is_empty() {
                println!("(no papers)");
            } else {
                for (id, arxiv, title, field, _) in rows {
                    println!(
                        "{}  {:12}  {:8}  {}",
                        id,
                        arxiv,
                        field.as_deref().unwrap_or(""),
                        truncate(&title, 70)
                    );
                }
            }
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

async fn approve(review_id: Uuid) -> anyhow::Result<()> {
    let config = super::Config::from_env();
    let state = super::AppState::from_config(config).await?;
    approve_impl(&state, review_id).await
}

#[cfg(feature = "grokrxiv-publisher")]
async fn approve_impl(state: &super::AppState, review_id: Uuid) -> anyhow::Result<()> {
    use grokrxiv_publisher::{AdminCaller, GithubPublisher, OpenReviewPr};
    use grokrxiv_schemas::ReviewStatus;

    let pool = state
        .db
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;

    // Read the review row + the joined paper for branch + field + arxiv_id.
    let row: (Uuid, String, String, Option<String>) = sqlx::query_as(
        "select r.id, p.arxiv_id, p.title, p.field \
         from reviews r join papers p on p.id = r.paper_id \
         where r.id = $1",
    )
    .bind(review_id)
    .fetch_one(pool)
    .await
    .map_err(|e| anyhow::anyhow!("review not found: {e}"))?;
    let (_, arxiv_id, title, field) = row;

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
            "https://github.com/GrokRxiv/reviews/pull/SIMULATED-{}",
            &review_id.simple().to_string()[..8]
        );
        let _ = sqlx::query("update reviews set github_pr_url = $2 where id = $1")
            .bind(review_id)
            .bind(&simulated)
            .execute(pool)
            .await;
        println!("pr_url={simulated}");
        return Ok(());
    };

    let owner = std::env::var("GROKRXIV_REVIEWS_OWNER").unwrap_or_else(|_| "GrokRxiv".into());
    let repo = std::env::var("GROKRXIV_REVIEWS_REPO").unwrap_or_else(|_| "reviews".into());
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

    println!("pr_url={pr_url}");
    Ok(())
}

#[cfg(not(feature = "grokrxiv-publisher"))]
async fn approve_impl(_state: &super::AppState, review_id: Uuid) -> anyhow::Result<()> {
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
