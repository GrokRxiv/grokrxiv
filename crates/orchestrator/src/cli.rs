//! `agh` CLI surface.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::{IsTerminal, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::AppAdapterResponse;
use agenthero_dag_runtime::{
    AgentKind, DagEdge, DagManifest, DagNode, DagNodeKind, DagRole, DagTool, OneOrMany, RoleId,
    ToolExecutorKind,
};
use anyhow::Context as _;
use clap::{Parser, Subcommand};
use serde_json::json;
use sqlx::Row as _;
use uuid::Uuid;

/// AgentHero DAGOps runtime for agentic applications as DAGs.
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
    /// Emit JSON instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Emit short foreground progress lines to stderr.
    #[arg(long, global = true, conflicts_with = "no_status")]
    pub status: bool,
    /// Suppress foreground progress lines for background runs.
    #[arg(long, global = true)]
    pub no_status: bool,
    /// Emit structured tracing diagnostics to stderr.
    #[arg(long, global = true)]
    pub debug_logs: bool,
    /// Write structured AgentHero tracing diagnostics to this JSONL file.
    #[arg(long, global = true, value_name = "PATH")]
    pub log_file: Option<PathBuf>,
    /// Plan-only: pass dry-run intent to app adapters that support it.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Print provider secrets in cleartext for `config`.
    #[arg(long, global = true, hide = true)]
    pub show_secrets: bool,
}

/// Top-level CLI subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List, inspect, or run installed DAGOps apps.
    App {
        /// App registry operation.
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Run the HTTP API + DB-backed app-run scheduler workers.
    Serve,
    /// Print env vars, app roots, DB, and provider reachability.
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
    /// Print resolved platform config.
    Config {
        /// Print secrets in cleartext instead of `***`.
        #[arg(long)]
        show_secrets: bool,
    },
    /// Inspect queued, running, completed, and failed jobs.
    Jobs {
        /// Jobs operation to run.
        #[command(subcommand)]
        command: JobsCommand,
    },
}

/// Product app execution operations.
#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// List installed DAGOps apps.
    List,
    /// Show one app's available actions.
    Show {
        /// Installed app id.
        app: String,
    },
    /// List or select app-owned eval suites.
    Eval {
        /// Installed app id.
        app: String,
        /// Optional eval suite id.
        eval_id: Option<String>,
    },
    /// Run one installed app action. With no action, prints that app's action catalog.
    Run {
        /// Installed app id.
        app: String,
        /// App command path and action-specific arguments.
        #[arg(num_args = 0.., allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Queue one installed app action in app_runs without executing it immediately.
    Enqueue {
        /// Installed app id.
        app: String,
        /// App command path and action-specific arguments.
        #[arg(num_args = 1.., allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// List app run records from the runtime database.
    Runs {
        /// Optional app id filter.
        #[arg(long)]
        app: Option<String>,
        /// Optional action id filter.
        #[arg(long)]
        action: Option<String>,
        /// Optional state filter.
        #[arg(long)]
        state: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show one app run record.
    Status {
        /// App run UUID.
        run_id: Uuid,
    },
    /// Compare two app runs for deterministic replay/audit drift.
    Compare {
        /// Left app run UUID.
        left_run_id: Uuid,
        /// Right app run UUID.
        right_run_id: Uuid,
    },
    /// Queue a checkpoint replay from one persisted app run.
    Replay {
        /// Source app run UUID to replay.
        source_run_id: Uuid,
    },
    /// Show the durable stderr log for one app run.
    Logs {
        /// App run UUID.
        run_id: Uuid,
        /// Number of trailing lines to print before exiting or following.
        #[arg(long, default_value_t = 120)]
        tail: usize,
        /// Continue streaming until the app run reaches a terminal state.
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Show the durable structured event stream for one app run.
    Events {
        /// App run UUID.
        run_id: Uuid,
        /// Number of trailing events to print before exiting or following.
        #[arg(long, default_value_t = 120)]
        tail: usize,
        /// Continue streaming until the app run reaches a terminal state.
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Approve a paused app run and requeue it for workers.
    ApproveRun {
        /// App run UUID.
        run_id: Uuid,
        /// Approval input key to set, for example `approval/human_release`.
        #[arg(long = "key")]
        key: String,
    },
    /// Claim and execute one queued app run, then exit.
    Work {
        /// Optional app run UUID to claim. Without this, claims the oldest queued run.
        #[arg(long = "run-id")]
        run_id: Option<Uuid>,
        /// Optional worker name persisted in worker_nodes.
        #[arg(long = "worker-name")]
        worker_name: Option<String>,
    },
    /// Cancel one queued or running app run.
    Cancel {
        /// App run UUID.
        run_id: Uuid,
        /// Operator-visible cancellation reason.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Cancel queued app runs matching a safe filter.
    CancelQueued {
        /// App id to cancel queued runs for.
        #[arg(long)]
        app: String,
        /// Optional action id filter.
        #[arg(long)]
        action: Option<String>,
        /// Keep these run UUIDs queued.
        #[arg(long = "except")]
        except: Vec<Uuid>,
        /// Only cancel runs older than this many minutes.
        #[arg(long = "older-than-mins")]
        older_than_mins: Option<i64>,
        /// Print matching runs without changing state.
        #[arg(long)]
        dry_run: bool,
        /// Operator-visible cancellation reason.
        #[arg(long)]
        reason: Option<String>,
    },
}

/// DAG manifest operations.
#[derive(Debug, Subcommand)]
pub enum DagCommand {
    /// Validate all DAG manifests, or one selected DAG type.
    Validate {
        /// DAG type id to validate.
        #[arg(long = "dag-type")]
        dag_type: Option<String>,
    },
    /// Run one registered DAG app through its declared adapter.
    Run {
        /// DAG type id to run.
        #[arg(long = "dag-type")]
        dag_type: String,
    },
    /// Add an agent role/node to one DAG manifest.
    AddAgent {
        /// DAG type id to edit.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped role id.
        #[arg(long = "role-id")]
        role_id: String,
        /// Agent capability kind.
        #[arg(long)]
        kind: String,
        /// Agent config path.
        #[arg(id = "agent_config", long = "agent-config")]
        config: Option<String>,
        /// Add an edge from this existing node to the new node.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Write the manifest. Without this, print the updated YAML.
        #[arg(long)]
        write: bool,
    },
    /// Remove an agent role/node from one DAG manifest.
    RemoveAgent {
        /// DAG type id to edit.
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
        /// DAG type id to edit.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped tool id.
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
        /// Add an edge from this existing node to the new node.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Artifact or node input name.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Artifact output name.
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
        /// DAG type id to edit.
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
        /// DAG type id to edit.
        #[arg(long = "dag-type")]
        dag_type: String,
        /// DAG-scoped tool id.
        #[arg(long = "tool-id")]
        tool_id: String,
        /// Rust handler name. Defaults to the tool id.
        #[arg(long)]
        handler: Option<String>,
        /// Add an edge from this existing node to the new node.
        #[arg(long = "after")]
        after: Vec<String>,
        /// Add an edge from the new node to this existing node.
        #[arg(long = "before")]
        before: Vec<String>,
        /// Artifact or node input name.
        #[arg(long = "input")]
        inputs: Vec<String>,
        /// Artifact output name.
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

/// Agent config placement commands.
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Print DAG types compatible with an agent YAML's `kind`.
    Place {
        /// Path to an agent YAML file containing a `kind` field.
        path: PathBuf,
    },
}

/// Generic platform job commands.
#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// List generic jobs with optional kind/state filters.
    List {
        /// Optional `kind` filter.
        #[arg(long)]
        kind: Option<String>,
        /// Optional `state` filter.
        #[arg(long)]
        state: Option<String>,
        /// Maximum rows to return.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

/// Run the parsed CLI.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let _status_enabled = cli.status || (!cli.no_status && std::io::stderr().is_terminal());
    let stream_app_stderr = stream_app_stderr_for_cli(&cli);
    let json = cli.json;
    let dry_run = cli.dry_run;
    let debug_logs = cli.debug_logs;
    match cli.command {
        Command::App { command } => {
            app_command(command, json, dry_run, stream_app_stderr, debug_logs).await
        }
        Command::Serve => crate::serve::run().await,
        Command::Doctor => crate::doctor::doctor(json).await,
        Command::Dag { command } => dag_command(command, json).await,
        Command::Validate { dag_type } => {
            dag_command(DagCommand::Validate { dag_type }, json).await
        }
        Command::Agent { command } => agent_command(command, json).await,
        Command::Config { show_secrets } => print_config(cli.show_secrets || show_secrets, json),
        Command::Jobs { command } => jobs_command(command, json).await,
    }
}

/// Whether `agh app run` should tee adapter stderr to the operator while the app runs.
pub fn stream_app_stderr_for_cli(cli: &Cli) -> bool {
    !cli.no_status
}

async fn app_command(
    command: AppCommand,
    json: bool,
    dry_run: bool,
    stream_app_stderr: bool,
    debug_logs: bool,
) -> anyhow::Result<()> {
    match command {
        AppCommand::List => app_list(json),
        AppCommand::Show { app } => app_show(&app, json),
        AppCommand::Eval { app, eval_id } => app_eval(&app, eval_id.as_deref(), json),
        AppCommand::Run { app, args } => {
            let manifest = crate::dag_apps::load_app_manifest_by_slug(&app)?;
            if args.is_empty() {
                return app_show_manifest(&manifest, json);
            }
            let resolved = crate::dag_apps::resolve_app_action_args_in_manifest(&manifest, &args)?;
            app_run_command(
                &manifest,
                &resolved.id,
                resolved.args,
                json,
                dry_run,
                stream_app_stderr,
                debug_logs,
            )
            .await
        }
        AppCommand::Enqueue { app, args } => {
            app_enqueue(&app, args, json, dry_run, debug_logs).await
        }
        AppCommand::Runs {
            app,
            action,
            state,
            limit,
        } => {
            app_runs(
                app.as_deref(),
                action.as_deref(),
                state.as_deref(),
                limit,
                json,
            )
            .await
        }
        AppCommand::Status { run_id } => app_status(run_id, json).await,
        AppCommand::Compare {
            left_run_id,
            right_run_id,
        } => app_compare(left_run_id, right_run_id, json).await,
        AppCommand::Replay { source_run_id } => app_replay(source_run_id, json).await,
        AppCommand::Logs {
            run_id,
            tail,
            follow,
        } => app_logs(run_id, tail, follow, json).await,
        AppCommand::Events {
            run_id,
            tail,
            follow,
        } => app_events(run_id, tail, follow, json).await,
        AppCommand::ApproveRun { run_id, key } => app_approve_run(run_id, &key, json).await,
        AppCommand::Work {
            run_id,
            worker_name,
        } => app_work(run_id, worker_name, json, stream_app_stderr, debug_logs).await,
        AppCommand::Cancel { run_id, reason } => app_cancel(run_id, reason.as_deref(), json).await,
        AppCommand::CancelQueued {
            app,
            action,
            except,
            older_than_mins,
            dry_run,
            reason,
        } => {
            app_cancel_queued(
                &app,
                action.as_deref(),
                &except,
                older_than_mins,
                dry_run,
                reason.as_deref(),
                json,
            )
            .await
        }
    }
}

/// Render app-owned help before Clap handles `--help` generically.
pub fn try_print_app_run_help_from_args(args: Vec<String>) -> anyhow::Result<bool> {
    let Some(app_run_index) = args
        .windows(2)
        .position(|window| window[0] == "app" && window[1] == "run")
    else {
        return Ok(false);
    };
    let Some(app) = args.get(app_run_index + 2) else {
        return Ok(false);
    };
    let trailing = &args[(app_run_index + 3)..];
    let Some(help_index) = trailing.iter().position(|arg| is_help_token(arg)) else {
        return Ok(false);
    };

    let json = args.iter().any(|arg| arg == "--json");
    let manifest = crate::dag_apps::load_app_manifest_by_slug(app)?;
    let action_args = app_action_args_without_agenthero_control_flags(&trailing[..help_index]);
    if action_args.is_empty() {
        return app_show_manifest(&manifest, json).map(|_| true);
    }

    let resolved = crate::dag_apps::resolve_app_action_args_in_manifest(&manifest, &action_args)?;
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == resolved.id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{} {}`", manifest.slug, resolved.id))?;
    print_app_action_help(&manifest, action, json)?;
    Ok(true)
}

fn is_help_token(arg: &str) -> bool {
    matches!(arg, "--help" | "-h" | "help")
}

fn is_agenthero_control_flag(arg: &str) -> bool {
    matches!(
        arg,
        "--json"
            | "--status"
            | "--no-status"
            | "--debug-logs"
            | "--log-file"
            | "--dry-run"
            | "--show-secrets"
    ) || arg.starts_with("--log-file=")
}

fn app_action_args_without_agenthero_control_flags(args: &[String]) -> Vec<String> {
    let mut filtered = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--log-file" {
            skip_next = true;
            continue;
        }
        if is_agenthero_control_flag(arg) {
            continue;
        }
        filtered.push(arg.clone());
    }
    filtered
}

/// Help styling that mirrors clap's defaults; `anstream` strips the ANSI codes
/// when stdout is not a terminal or `NO_COLOR` is set.
const HELP_HEADER: anstyle::Style = anstyle::Style::new().bold().underline();
const HELP_LITERAL: anstyle::Style = anstyle::Style::new().bold();
const HELP_PLACEHOLDER: anstyle::Style =
    anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Cyan)));
const HELP_MUTED: anstyle::Style = anstyle::Style::new().dimmed();

/// Column threshold above which a row's description wraps onto its own line.
const HELP_COLUMN_MAX: usize = 32;

/// One help row: styled left column, its unstyled display width, and the
/// description column (which may itself carry styled suffixes).
type HelpRow = (String, usize, String);

fn print_help_rows(rows: &[HelpRow]) {
    let column = rows
        .iter()
        .map(|row| row.1)
        .filter(|width| *width <= HELP_COLUMN_MAX)
        .max()
        .unwrap_or(0);
    for (styled, width, description) in rows {
        if description.is_empty() {
            anstream::println!("  {styled}");
        } else if *width <= column {
            anstream::println!("  {styled}{}  {description}", " ".repeat(column - width));
        } else {
            // Row wider than the column: drop the description to its own line,
            // aligned with the shared description column.
            anstream::println!("  {styled}");
            anstream::println!("  {}  {description}", " ".repeat(column));
        }
    }
}

fn positional_placeholder(option: &crate::dag_apps::AppActionOption) -> String {
    let value = option
        .value_name
        .clone()
        .unwrap_or_else(|| option.name.to_ascii_uppercase().replace('-', "_"));
    let token = if option.required {
        format!("<{value}>")
    } else {
        format!("[<{value}>]")
    };
    if option.multiple {
        format!("{token}...")
    } else {
        token
    }
}

/// Styled `command <ARGS>...` summary used by app overviews and listings.
fn app_action_summary(action: &crate::dag_apps::AppManifestAction) -> (String, usize) {
    let command = action.command.join(" ");
    let mut styled = format!("{HELP_LITERAL}{command}{HELP_LITERAL:#}");
    let mut width = command.len();
    for option in action
        .options
        .iter()
        .filter(|option| option.kind == "positional")
    {
        let token = positional_placeholder(option);
        styled.push_str(&format!(" {HELP_PLACEHOLDER}{token}{HELP_PLACEHOLDER:#}"));
        width += token.len() + 1;
    }
    (styled, width)
}

fn option_help_row(option: &crate::dag_apps::AppActionOption) -> HelpRow {
    let (styled, width) = if option.kind == "positional" {
        let token = positional_placeholder(option);
        let width = token.len();
        (
            format!("{HELP_PLACEHOLDER}{token}{HELP_PLACEHOLDER:#}"),
            width,
        )
    } else {
        let mut styled = format!("{HELP_LITERAL}{}{HELP_LITERAL:#}", option.name);
        let mut width = option.name.len();
        if let Some(value) = option.value_name.as_deref() {
            styled.push_str(&format!(" {HELP_PLACEHOLDER}<{value}>{HELP_PLACEHOLDER:#}"));
            width += value.len() + 3;
        }
        (styled, width)
    };

    let mut description = option.description.clone();
    if option.kind != "positional" {
        if option.required {
            description.push_str(&format!(" {HELP_MUTED}[required]{HELP_MUTED:#}"));
        }
        if option.multiple {
            description.push_str(&format!(" {HELP_MUTED}[repeatable]{HELP_MUTED:#}"));
        }
        if !option.conflicts_with.is_empty() {
            description.push_str(&format!(
                " {HELP_MUTED}[conflicts: {}]{HELP_MUTED:#}",
                option.conflicts_with.join(",")
            ));
        }
    }
    (styled, width, description.trim_start().to_string())
}

fn app_list(json: bool) -> anyhow::Result<()> {
    let apps = crate::dag_apps::load_app_manifests()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "apps": apps.iter().map(app_catalog_json).collect::<Vec<_>>()
            }))?
        );
    } else {
        for (index, app) in apps.iter().enumerate() {
            if index > 0 {
                println!();
            }
            anstream::println!(
                "{HELP_LITERAL}{}{HELP_LITERAL:#} {HELP_MUTED}- {}{HELP_MUTED:#}",
                app.slug,
                app.label
            );
            let rows = app
                .actions
                .iter()
                .map(|action| {
                    let (styled, width) = app_action_summary(action);
                    (styled, width, action.description.clone())
                })
                .collect::<Vec<_>>();
            print_help_rows(&rows);
        }
    }
    Ok(())
}

fn app_show(app_id: &str, json: bool) -> anyhow::Result<()> {
    let app = crate::dag_apps::load_app_manifest_by_slug(app_id)?;
    app_show_manifest(&app, json)
}

fn app_show_manifest(app: &crate::dag_apps::AppManifest, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&app_catalog_json(app))?);
    } else {
        anstream::println!(
            "{HELP_LITERAL}{}{HELP_LITERAL:#} {HELP_MUTED}({}){HELP_MUTED:#}",
            app.label,
            app.slug
        );
        if !app.description.is_empty() {
            println!("{}", app.description);
        }
        println!();
        anstream::println!(
            "{HELP_HEADER}Usage:{HELP_HEADER:#} {HELP_LITERAL}agh app run {}{HELP_LITERAL:#} {HELP_PLACEHOLDER}<COMMAND>{HELP_PLACEHOLDER:#} [ARGS] [OPTIONS]",
            app.slug
        );
        if !app.actions.is_empty() {
            println!();
            anstream::println!("{HELP_HEADER}Commands:{HELP_HEADER:#}");
            let rows = app
                .actions
                .iter()
                .map(|action| {
                    let (styled, width) = app_action_summary(action);
                    (styled, width, action.description.clone())
                })
                .collect::<Vec<_>>();
            print_help_rows(&rows);
        }
        if !app.deployments.is_empty() {
            println!();
            anstream::println!("{HELP_HEADER}Deployments:{HELP_HEADER:#}");
            for deployment in &app.deployments {
                match deployment {
                    crate::dag_apps::AppDeployment::Vercel {
                        id,
                        project,
                        root,
                        framework,
                        build_command,
                        output_directory,
                        env,
                    } => {
                        let mut fields = vec![format!("project={project}"), format!("root={root}")];
                        if let Some(framework) = framework {
                            fields.push(format!("framework={framework}"));
                        }
                        if let Some(build_command) = build_command {
                            fields.push(format!("build_command={build_command}"));
                        }
                        if let Some(output_directory) = output_directory {
                            fields.push(format!("output_directory={output_directory}"));
                        }
                        if !env.is_empty() {
                            fields.push(format!("env={}", env.join(",")));
                        }
                        anstream::println!(
                            "  {HELP_LITERAL}{id}{HELP_LITERAL:#} {HELP_PLACEHOLDER}vercel{HELP_PLACEHOLDER:#} {HELP_MUTED}{}{HELP_MUTED:#}",
                            fields.join(" ")
                        );
                    }
                }
            }
        }
        println!();
        anstream::println!("{HELP_HEADER}Observability:{HELP_HEADER:#}");
        anstream::println!(
            "  events={} logs={} status={} event_stream={} lifecycle_events={} trace_fields={}",
            app.observability.events,
            app.observability.logs,
            app.observability.status,
            app.observability.event_stream,
            app.observability.lifecycle_events.join(","),
            app.observability.trace_fields.len(),
        );
        let contracts = crate::dag_apps::app_contracts(&app.slug).unwrap_or_default();
        if !contracts.state_schemas.is_empty()
            || contracts.tools.is_some()
            || !contracts.policies.is_empty()
            || !contracts.evals.is_empty()
        {
            println!();
            anstream::println!("{HELP_HEADER}Contracts:{HELP_HEADER:#}");
            for schema in contracts.state_schemas {
                anstream::println!("  {HELP_LITERAL}state{HELP_LITERAL:#} {schema}");
            }
            if let Some(tools) = contracts.tools {
                anstream::println!("  {HELP_LITERAL}tools{HELP_LITERAL:#} {tools}");
            }
            for policy in contracts.policies {
                anstream::println!("  {HELP_LITERAL}policy{HELP_LITERAL:#} {policy}");
            }
            for eval in contracts.evals {
                anstream::println!("  {HELP_LITERAL}eval{HELP_LITERAL:#} {eval}");
            }
        }
        println!();
        anstream::println!(
            "{HELP_MUTED}See `agh app run {} <command> --help` for command arguments and options.{HELP_MUTED:#}",
            app.slug
        );
    }
    Ok(())
}

fn app_catalog_json(app: &crate::dag_apps::AppManifest) -> serde_json::Value {
    json!({
        "id": app.slug,
        "label": app.label,
        "description": app.description,
        "deployments": app.deployments,
        "observability": app.observability,
        "contracts": crate::dag_apps::app_contracts(&app.slug).unwrap_or_default(),
        "actions": app.actions.iter().map(|action| json!({
            "id": action.id,
            "command": action.command,
            "dag_type": action.dag_type,
            "description": action.description,
            "options": action.options,
        })).collect::<Vec<_>>(),
    })
}

fn app_eval(app_id: &str, eval_id: Option<&str>, json: bool) -> anyhow::Result<()> {
    let _manifest = crate::dag_apps::load_app_manifest_by_slug(app_id)?;
    let contracts = crate::dag_apps::app_contracts(app_id)?;
    let mut evals = Vec::new();
    for rel in contracts.evals {
        let path = crate::dag_apps::app_root(app_id).join(&rel);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read eval contract {}", path.display()))?;
        let parsed: serde_yaml::Value = serde_yaml::from_str(&text)
            .with_context(|| format!("parse eval contract {}", path.display()))?;
        let id = parsed
            .get("id")
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("eval")
                    .to_string()
            });
        if eval_id.is_some_and(|wanted| wanted != id) {
            continue;
        }
        let description = parsed
            .get("description")
            .and_then(serde_yaml::Value::as_str)
            .unwrap_or("")
            .to_string();
        evals.push(json!({
            "id": id,
            "path": rel,
            "description": description,
            "status": "defined",
        }));
    }
    if let Some(eval_id) = eval_id {
        if evals.is_empty() {
            anyhow::bail!("app `{app_id}` has no eval `{eval_id}`");
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "app": app_id,
                "evals": evals,
            }))?
        );
    } else if evals.is_empty() {
        println!("No evals declared for app `{app_id}`.");
    } else {
        anstream::println!("{HELP_LITERAL}{app_id}{HELP_LITERAL:#} evals");
        for eval in evals {
            let id = eval
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let path = eval
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let description = eval
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if description.is_empty() {
                anstream::println!(
                    "  {HELP_LITERAL}{id}{HELP_LITERAL:#} {HELP_MUTED}{path}{HELP_MUTED:#}"
                );
            } else {
                anstream::println!("  {HELP_LITERAL}{id}{HELP_LITERAL:#} {HELP_MUTED}{path}{HELP_MUTED:#} {description}");
            }
        }
    }
    Ok(())
}

fn print_app_action_help(
    app: &crate::dag_apps::AppManifest,
    action: &crate::dag_apps::AppManifestAction,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "app": app.slug,
                "label": app.label,
                "action": {
                    "id": action.id,
                    "command": action.command,
                    "dag_type": action.dag_type,
                    "description": action.description,
                    "options": action.options,
                }
            }))?
        );
        return Ok(());
    }

    let command = action.command.join(" ");
    anstream::println!("{HELP_LITERAL}{} {}{HELP_LITERAL:#}", app.label, command);
    if !action.description.is_empty() {
        println!("{}", action.description);
    }
    anstream::println!("{HELP_MUTED}dag: {}{HELP_MUTED:#}", action.dag_type);
    println!();
    anstream::println!(
        "{HELP_HEADER}Usage:{HELP_HEADER:#} {}",
        app_action_usage(app, action)
    );

    let positionals = action
        .options
        .iter()
        .filter(|option| option.kind == "positional")
        .map(option_help_row)
        .collect::<Vec<_>>();
    if !positionals.is_empty() {
        println!();
        anstream::println!("{HELP_HEADER}Arguments:{HELP_HEADER:#}");
        print_help_rows(&positionals);
    }

    let flags = action
        .options
        .iter()
        .filter(|option| option.kind != "positional")
        .map(option_help_row)
        .collect::<Vec<_>>();
    if !flags.is_empty() {
        println!();
        anstream::println!("{HELP_HEADER}Options:{HELP_HEADER:#}");
        print_help_rows(&flags);
    }
    Ok(())
}

/// Styled one-line usage string for an app action.
fn app_action_usage(
    app: &crate::dag_apps::AppManifest,
    action: &crate::dag_apps::AppManifestAction,
) -> String {
    let mut usage = format!(
        "{HELP_LITERAL}agh app run {} {}{HELP_LITERAL:#}",
        app.slug,
        action.command.join(" ")
    );
    for option in action
        .options
        .iter()
        .filter(|option| option.kind == "positional")
    {
        let token = positional_placeholder(option);
        usage.push_str(&format!(" {HELP_PLACEHOLDER}{token}{HELP_PLACEHOLDER:#}"));
    }
    if action
        .options
        .iter()
        .any(|option| option.kind != "positional")
    {
        usage.push_str(" [OPTIONS]");
    }
    usage
}

async fn app_run_command(
    app: &crate::dag_apps::AppManifest,
    action: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
    stream_app_stderr: bool,
    debug_logs: bool,
) -> anyhow::Result<()> {
    let binding = app
        .actions
        .iter()
        .find(|candidate| candidate.id == action)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{} {action}`", app.slug))?;
    crate::dag_apps::validate_app_action_args(binding, &args)?;
    let input = app_run_adapter_input(
        &app.slug,
        action,
        &binding.dag_type,
        stream_app_stderr,
        debug_logs,
    );

    let pool = connect_db().await?;
    let run_id = crate::app_runs::insert_queued(
        &pool,
        &app.slug,
        action,
        crate::app_runs::AppRunRequest {
            args,
            input,
            dry_run,
            json,
        },
    )
    .await?;

    if stream_app_stderr {
        eprintln!("AgentHero app run {run_id}");
        eprintln!(
            "log {}",
            crate::dag_apps::app_run_log_path(run_id).display()
        );
    }

    let claimed = crate::scheduler::work_once(
        pool.clone(),
        Some(run_id),
        Some(format!("foreground-{}", std::process::id())),
        stream_app_stderr,
        debug_logs,
    )
    .await?;
    if claimed.is_none() {
        anyhow::bail!("foreground app run {run_id} was queued but no worker claimed it");
    }

    let record = crate::app_runs::get_run(&pool, run_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!("foreground app run disappeared after execution: {run_id}")
        })?;
    let response = successful_app_run_response(&record)?;
    print_app_adapter_response(app, action, &response, json)?;
    Ok(())
}

fn successful_app_run_response(
    record: &crate::app_runs::AppRunRecord,
) -> anyhow::Result<AppAdapterResponse> {
    match record.state.as_str() {
        "done" | "partial" | "awaiting_approval" => {
            let response: AppAdapterResponse = serde_json::from_value(record.output.clone())
                .with_context(|| {
                    format!("parse stored adapter response for app run {}", record.id)
                })?;
            if response.ok {
                Ok(response)
            } else {
                anyhow::bail!(
                    "app run {} completed with ok=false: {}",
                    record.id,
                    response
                        .error
                        .unwrap_or_else(|| "adapter returned no error message".to_string())
                );
            }
        }
        state => {
            let message = record
                .error_message
                .as_deref()
                .unwrap_or("app run failed without a stored error message");
            anyhow::bail!("app run {} ended in state {state}: {message}", record.id);
        }
    }
}

fn print_app_adapter_response(
    app: &crate::dag_apps::AppManifest,
    action: &str,
    response: &AppAdapterResponse,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(report) = response.report.as_ref() {
        println!(
            "app={} action={} dag_type={} status={:?} nodes={}",
            app.slug,
            action,
            report.dag_type,
            report.status,
            report.nodes.len()
        );
    } else if let Some(output) = response.output.as_ref() {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("app={} action={} ok", app.slug, action);
    }
    Ok(())
}

async fn app_enqueue(
    app: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
    debug_logs: bool,
) -> anyhow::Result<()> {
    let manifest = crate::dag_apps::load_app_manifest_by_slug(app)?;
    let resolved = crate::dag_apps::resolve_app_action_args_in_manifest(&manifest, &args)?;
    let action = manifest
        .actions
        .iter()
        .find(|candidate| candidate.id == resolved.id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{} {}`", manifest.slug, resolved.id))?;
    crate::dag_apps::validate_app_action_args(action, &resolved.args)?;
    let pool = connect_db().await?;
    let input = app_run_adapter_input(
        &manifest.slug,
        &resolved.id,
        &resolved.dag_type,
        true,
        debug_logs,
    );
    let run_id = crate::app_runs::insert_queued(
        &pool,
        &manifest.slug,
        &resolved.id,
        crate::app_runs::AppRunRequest {
            args: resolved.args,
            input,
            dry_run,
            json,
        },
    )
    .await?;
    let work_command = format!("agh app work --run-id {run_id}");
    let logs_command = format!("agh app logs {run_id} --follow");
    let log_path = crate::dag_apps::app_run_log_path(run_id);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "id": run_id,
                "app_id": manifest.slug,
                "action_id": resolved.id,
                "dag_type": resolved.dag_type,
                "state": "queued",
                "work_command": work_command,
                "logs_command": logs_command,
                "log_path": log_path.to_string_lossy(),
            }))?
        );
    } else {
        println!("queued app run {run_id}");
        println!("app_action   {}:{}", manifest.slug, resolved.id);
        println!("dag_type     {}", resolved.dag_type);
        println!("run          {work_command}");
        println!("logs         {logs_command}");
        println!("log_path     {}", log_path.display());
    }
    Ok(())
}

async fn app_replay(source_run_id: Uuid, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let replay = crate::app_runs::insert_replay_queued(&pool, source_run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("source app run not found: {source_run_id}"))?;
    let work_command = format!("agh app work --run-id {}", replay.id);
    let logs_command = format!("agh app logs {} --follow", replay.id);
    let compare_command = format!("agh app compare {} {}", replay.source_run_id, replay.id);
    let log_path = crate::dag_apps::app_run_log_path(replay.id);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "id": replay.id,
                "source_run_id": replay.source_run_id,
                "app_id": replay.app_id,
                "action_id": replay.action_id,
                "dag_type": replay.dag_type,
                "manifest_hash": replay.manifest_hash,
                "state": "queued",
                "replay": true,
                "work_command": work_command,
                "logs_command": logs_command,
                "compare_command": compare_command,
                "log_path": log_path.to_string_lossy(),
            }))?
        );
    } else {
        println!("queued replay app run {}", replay.id);
        println!("source       {}", replay.source_run_id);
        println!("app_action   {}:{}", replay.app_id, replay.action_id);
        println!("dag_type     {}", replay.dag_type);
        println!("manifest     {}", replay.manifest_hash);
        println!("run          {work_command}");
        println!("logs         {logs_command}");
        println!("compare      {compare_command}");
        println!("log_path     {}", log_path.display());
    }
    Ok(())
}

fn app_run_adapter_input(
    app: &str,
    action: &str,
    dag_type: &str,
    stream_app_stderr: bool,
    debug_logs: bool,
) -> agenthero_dag_executor::DagIo {
    let mut input = agenthero_dag_executor::DagIo::default();
    input.values.insert("app".into(), json!(app));
    input.values.insert("action".into(), json!(action));
    input.values.insert("dag_type".into(), json!(dag_type));
    input
        .values
        .insert("stream_stderr".into(), json!(stream_app_stderr));
    input.values.insert("debug_logs".into(), json!(debug_logs));
    input
}

#[derive(Debug, Clone)]
struct AppRunListRow {
    id: String,
    app_id: String,
    action_id: String,
    state: String,
    review_id: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
    observability: AppRunListObservability,
}

#[derive(Debug, Clone, Default)]
struct AppRunListObservability {
    event_count: usize,
    log_exists: bool,
    log_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
struct AppRunLeaseSummary {
    state: String,
    leased_until: chrono::DateTime<chrono::Utc>,
    worker_name: Option<String>,
    worker_state: Option<String>,
    worker_last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
struct AppRunEventSummary {
    id: i64,
    level: String,
    event_type: String,
    message: Option<String>,
    payload: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
struct AppRunQueueSummary {
    older_queued_count: i64,
    running_same_action: Vec<AppRunListRow>,
}

async fn app_runs(
    app: Option<&str>,
    action: Option<&str>,
    state: Option<&str>,
    limit: u32,
    json: bool,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let limit = limit.clamp(1, 500) as i64;
    let rows = sqlx::query(
        "select ar.id::text, ar.app_id, ar.action_id, ar.state, ar.input, \
                ar.created_at, ar.started_at, ar.finished_at, \
                (select count(*) from dag_events de where de.app_run_id = ar.id) as event_count \
         from app_runs ar \
         where ($1::text is null or ar.app_id = $1) \
           and ($2::text is null or ar.action_id = $2) \
           and ($3::text is null or ar.state = $3) \
         order by ar.created_at desc \
         limit $4",
    )
    .bind(app)
    .bind(action)
    .bind(state)
    .bind(limit)
    .fetch_all(&pool)
    .await
    .context("list app runs")?;

    let values = rows
        .iter()
        .map(|row| {
            let run_id = row.get::<String, _>(0);
            let input = row.get::<serde_json::Value, _>(4);
            let event_count = row.get::<i64, _>(8);
            json!({
                "id": run_id,
                "app_id": row.get::<String, _>(1),
                "action_id": row.get::<String, _>(2),
                "state": row.get::<String, _>(3),
                "review_id": app_run_review_id(&input),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>(5),
                "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(6),
                "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(7),
                "observability": app_run_list_observability_json(&app_run_list_observability(&run_id, event_count)),
            })
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "runs": values }))?
        );
    } else if values.is_empty() {
        println!("No app runs found.");
    } else {
        let rows = values
            .iter()
            .filter_map(app_run_list_row_from_value)
            .collect::<Vec<_>>();
        print!("{}", format_app_run_table(&rows, chrono::Utc::now()));
    }
    Ok(())
}

async fn app_status(run_id: Uuid, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let row = sqlx::query(
        "select id::text, app_id, action_id, state, input, output, error_code, error_message, \
                created_at, started_at, finished_at, attempt \
         from app_runs where id = $1",
    )
    .bind(run_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("app run not found: {run_id}"))?;
    let lease = load_latest_app_run_lease(&pool, run_id).await?;
    let events = load_recent_app_run_events(&pool, run_id, 50).await?;
    let total_event_count = count_app_run_events(&pool, run_id).await?;
    let observability = crate::app_runs::load_observability(&pool, run_id).await?;
    let input = row.get::<serde_json::Value, _>(4);
    let output = row.get::<serde_json::Value, _>(5);
    let state = row.get::<String, _>(3);
    let app_id = row.get::<String, _>(1);
    let action_id = row.get::<String, _>(2);
    let created_at = row.get::<chrono::DateTime<chrono::Utc>, _>(8);
    let started_at = row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(9);
    let finished_at = row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(10);
    let log_path = crate::dag_apps::app_run_log_path(run_id);
    let log_exists = log_path.is_file();
    let log_bytes = std::fs::metadata(&log_path)
        .ok()
        .map(|metadata| metadata.len());
    let now = chrono::Utc::now();
    let queue_summary = if state == "queued" {
        Some(load_app_run_queue_summary(&pool, run_id, &app_id, &action_id, created_at).await?)
    } else {
        None
    };
    let determinism = crate::app_runs::app_run_determinism_summary(
        &output,
        observability.latest_dag_run.as_ref(),
        &observability.nodes,
        &observability.artifacts,
    );
    let policies = crate::app_runs::app_run_policy_summary(&observability.nodes);
    let diagnosis = diagnose_app_run_status(
        &state,
        lease.as_ref().map(|lease| lease.state.as_str()),
        lease.as_ref().map(|lease| lease.leased_until),
        lease
            .as_ref()
            .and_then(|lease| lease.worker_name.as_deref()),
        now,
    );
    let payload = json!({
        "id": row.get::<String, _>(0),
        "app_id": app_id,
        "action_id": action_id,
        "state": state,
        "diagnosis": diagnosis,
        "review_id": app_run_review_id(&input),
        "age": format_app_run_age(created_at, now),
        "attempt": row.get::<i32, _>(11),
        "input": input,
        "output": output,
        "error_code": row.get::<Option<String>, _>(6),
        "error_message": row.get::<Option<String>, _>(7),
        "created_at": created_at,
        "started_at": started_at,
        "finished_at": finished_at,
        "log_path": log_path.to_string_lossy(),
        "log_exists": log_exists,
        "observability": app_run_observability_json(
            run_id,
            &log_path,
            Some(&app_id),
            Some(&action_id),
            observability
                .latest_dag_run
                .as_ref()
                .map(|dag_run| dag_run.dag_type.as_str()),
            AppStatusObservabilitySummary {
                total_event_count,
                recent_event_count: events.len(),
                log_exists,
                log_bytes,
            },
        ),
        "latest_lease": lease.as_ref().map(app_run_lease_json),
        "queue": queue_summary.as_ref().map(app_run_queue_summary_json),
        "determinism": determinism,
        "policies": policies,
        "latest_dag_run": observability.latest_dag_run,
        "live_nodes": observability.live_nodes,
        "nodes": observability.nodes,
        "artifacts": observability.artifacts,
        "recent_events": events.iter().map(app_run_event_json).collect::<Vec<_>>(),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "run_id       {}",
            payload["id"].as_str().unwrap_or_default()
        );
        println!(
            "app_action   {}:{}",
            payload["app_id"].as_str().unwrap_or_default(),
            payload["action_id"].as_str().unwrap_or_default()
        );
        println!(
            "state        {}",
            payload["state"].as_str().unwrap_or_default()
        );
        println!(
            "diagnosis    {}",
            payload["diagnosis"].as_str().unwrap_or_default()
        );
        println!(
            "review_id    {}",
            payload["review_id"].as_str().unwrap_or("-")
        );
        println!("attempt      {}", payload["attempt"]);
        println!(
            "age          {}",
            payload["age"].as_str().unwrap_or_default()
        );
        println!("queued_at    {}", format_app_run_ts(Some(created_at)));
        println!("started_at   {}", format_app_run_ts(started_at));
        println!("finished_at  {}", format_app_run_ts(finished_at));
        println!(
            "log          {}{}",
            payload["log_path"].as_str().unwrap_or_default(),
            if payload["log_exists"].as_bool() == Some(true) {
                ""
            } else {
                " (missing)"
            }
        );
        if let Some(summary) = format_observability_summary_line(&payload["observability"]) {
            println!("observability {summary}");
        }
        println!(
            "logs         {}",
            payload["observability"]["logs_command"]
                .as_str()
                .unwrap_or_default()
        );
        println!(
            "events       {}",
            payload["observability"]["events_command"]
                .as_str()
                .unwrap_or_default()
        );
        println!(
            "event_stream {}",
            payload["observability"]["event_stream_path"]
                .as_str()
                .unwrap_or_default()
        );
        println!(
            "metrics      {}",
            payload["observability"]["metrics_path"]
                .as_str()
                .unwrap_or_default()
        );
        if let Some(scope) = format_metrics_label_scope(&payload["observability"]) {
            println!("metrics_scope {scope}");
        }
        if let Some(policies) = format_policy_summary_line(&payload["policies"]) {
            println!("policies     {policies}");
        }
        if let Some(lease) = lease {
            println!(
                "lease        {} until {} worker={} heartbeat={}",
                lease.state,
                format_app_run_ts(Some(lease.leased_until)),
                lease.worker_name.as_deref().unwrap_or("-"),
                format_app_run_ts(lease.worker_last_heartbeat_at)
            );
        } else {
            println!("lease        -");
        }
        if let Some(summary) = &queue_summary {
            println!(
                "queue        position={} older_queued={} running_same_action={}",
                summary.older_queued_count.saturating_add(1),
                summary.older_queued_count,
                summary.running_same_action.len()
            );
            if !summary.running_same_action.is_empty() {
                println!("running_same_action");
                print!(
                    "{}",
                    format_app_run_table(&summary.running_same_action, chrono::Utc::now())
                );
            }
        }
        if let Some(error) = payload["error_message"].as_str() {
            println!("error        {error}");
        }
        if let Some(dag) = payload["latest_dag_run"].as_object() {
            println!(
                "dag          {} state={} manifest={} hash={}",
                dag.get("dag_type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
                dag.get("state")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-"),
                dag.get("manifest_version")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "-".to_string()),
                dag.get("manifest_hash")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-")
            );
        }
        if let Some(live_nodes) = payload["live_nodes"].as_array() {
            if !live_nodes.is_empty() {
                println!("live_nodes");
                for node in live_nodes.iter().take(20) {
                    println!("{}", format_live_node_status_line(node));
                }
                if live_nodes.len() > 20 {
                    println!("  ... {} more live nodes", live_nodes.len() - 20);
                }
            }
        }
        if let Some(nodes) = payload["nodes"].as_array() {
            if !nodes.is_empty() {
                println!("nodes");
                for node in nodes.iter().take(20) {
                    println!(
                        "  {:<32} {:<18} attempt={} runner={} model={} prompt_hash={} exit={}",
                        node.get("node_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        node.get("state")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        node.get("attempt")
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "-".to_string()),
                        node.get("runner")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        node.get("model")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        node.get("prompt_hash")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        node.get("exit_status")
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "-".to_string())
                    );
                }
                if nodes.len() > 20 {
                    println!("  ... {} more nodes", nodes.len() - 20);
                }
            }
        }
        if let Some(artifacts) = payload["artifacts"].as_array() {
            if !artifacts.is_empty() {
                println!("artifacts    {}", artifacts.len());
                for artifact in artifacts.iter().take(10) {
                    println!(
                        "  {:<32} {}",
                        artifact
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-"),
                        artifact
                            .get("uri")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("-")
                    );
                }
                if artifacts.len() > 10 {
                    println!("  ... {} more artifacts", artifacts.len() - 10);
                }
            }
        }
        if !events.is_empty() {
            println!("events");
            for event in events {
                println!(
                    "  {} {:<5} {:<28} {}",
                    format_app_run_ts(Some(event.created_at)),
                    event.level,
                    event.event_type,
                    event.message.as_deref().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

async fn app_compare(left_run_id: Uuid, right_run_id: Uuid, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let left = crate::app_runs::get_run_detail(&pool, left_run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("left app run not found: {left_run_id}"))?;
    let right = crate::app_runs::get_run_detail(&pool, right_run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("right app run not found: {right_run_id}"))?;
    let comparison = crate::app_runs::compare_app_run_details(&left, &right);
    if json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    } else {
        print!("{}", format_app_run_comparison(&comparison));
    }
    Ok(())
}

fn format_app_run_comparison(comparison: &crate::app_runs::AppRunComparison) -> String {
    let mut output = String::new();
    writeln!(&mut output, "left_run      {}", comparison.left.run_id).ok();
    writeln!(&mut output, "right_run     {}", comparison.right.run_id).ok();
    writeln!(
        &mut output,
        "app_action    {}:{}",
        comparison.left.app_id, comparison.left.action_id
    )
    .ok();
    writeln!(
        &mut output,
        "right_action  {}:{}",
        comparison.right.app_id, comparison.right.action_id
    )
    .ok();
    writeln!(&mut output, "compare_ready {}", comparison.compare_ready).ok();
    writeln!(&mut output, "matches       {}", comparison.matches).ok();
    writeln!(
        &mut output,
        "work_product {}",
        comparison.work_product_matches
    )
    .ok();
    writeln!(
        &mut output,
        "checks        app={} action={} dag_type={} manifest={} input={} output={} node_outputs={} artifacts={}",
        comparison.checks.same_app,
        comparison.checks.same_action,
        comparison.checks.same_dag_type,
        comparison.checks.same_manifest_hash,
        comparison.checks.same_frozen_input_hash,
        comparison.checks.same_dag_output_hash,
        comparison.checks.same_node_outputs,
        comparison.checks.same_artifacts,
    )
    .ok();
    writeln!(
        &mut output,
        "normalized    input={} output={} node_outputs={}",
        comparison.checks.same_normalized_frozen_input_hash,
        comparison.checks.same_normalized_dag_output_hash,
        comparison.checks.same_normalized_node_outputs,
    )
    .ok();
    push_comparison_difference_section(&mut output, "differences", &comparison.differences);
    push_comparison_difference_section(
        &mut output,
        "work_product_differences",
        &comparison.work_product_differences,
    );
    output
}

fn push_comparison_difference_section(
    output: &mut String,
    label: &str,
    differences: &[crate::app_runs::AppRunComparisonDifference],
) {
    if differences.is_empty() {
        writeln!(output, "{label:<13} -").ok();
        return;
    }
    writeln!(output, "{label}").ok();
    for difference in differences.iter().take(50) {
        writeln!(
            output,
            "  {} left={} right={}",
            difference.field,
            format_comparison_value(&difference.left),
            format_comparison_value(&difference.right)
        )
        .ok();
    }
    if differences.len() > 50 {
        writeln!(output, "  ... {} more differences", differences.len() - 50).ok();
    }
}

fn format_comparison_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

fn app_run_observability_json(
    run_id: Uuid,
    log_path: &Path,
    app_id: Option<&str>,
    action_id: Option<&str>,
    dag_type: Option<&str>,
    summary: AppStatusObservabilitySummary,
) -> serde_json::Value {
    json!({
        "status_command": format!("agh app status {run_id}"),
        "logs_command": format!("agh app logs {run_id} --follow"),
        "logs_path": format!("/app-runs/{run_id}/logs"),
        "events_command": format!("agh app events {run_id} --follow"),
        "events_path": format!("/app-runs/{run_id}/events"),
        "event_stream_path": format!("/app-runs/{run_id}/events/stream"),
        "metrics_path": "/metrics",
        "metrics_labels": {
            "app": app_id,
            "action": action_id,
            "dag_type": dag_type,
        },
        "log_path": log_path.to_string_lossy(),
        "trace_fields": agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS,
        "summary": {
            "total_event_count": summary.total_event_count,
            "recent_event_count": summary.recent_event_count,
            "log_exists": summary.log_exists,
            "log_bytes": summary.log_bytes,
        },
        "event_contract": app_event_contract_json(),
        "log_contract": agenthero_agent_runtime::agenthero_log_contract(),
        "stream_contract": app_event_stream_contract_json(),
    })
}

#[derive(Clone, Copy)]
struct AppStatusObservabilitySummary {
    total_event_count: usize,
    recent_event_count: usize,
    log_exists: bool,
    log_bytes: Option<u64>,
}

fn format_metrics_label_scope(observability: &serde_json::Value) -> Option<String> {
    let labels = observability.get("metrics_labels")?;
    let mut parts = Vec::new();
    for key in ["app", "action", "dag_type"] {
        if let Some(value) = labels.get(key).and_then(|value| value.as_str()) {
            if !value.is_empty() {
                parts.push(format!("{key}={value}"));
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn format_observability_summary_line(observability: &serde_json::Value) -> Option<String> {
    let summary = observability.get("summary")?;
    if !summary.is_object() {
        return None;
    }
    let total_event_count = json_u64_field(summary, "total_event_count").unwrap_or(0);
    let recent_event_count = json_u64_field(summary, "recent_event_count").unwrap_or(0);
    let log_exists = summary
        .get("log_exists")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let log_bytes = json_u64_field(summary, "log_bytes")
        .map(|bytes| bytes.to_string())
        .unwrap_or_else(|| "-".to_string());

    Some(format!(
        "events={total_event_count} recent={recent_event_count} log_exists={log_exists} log_bytes={log_bytes}"
    ))
}

fn format_policy_summary_line(policies: &serde_json::Value) -> Option<String> {
    if !policies.is_object() {
        return None;
    }
    let parts = [
        ("nodes", "node_attempts"),
        ("timeout", "timeout_limited_nodes"),
        ("budget", "budget_limited_nodes"),
        ("units", "budget_units_requested"),
        ("approval_gates", "approval_gates"),
        ("approval_required", "approval_required_tools"),
        ("network_denied", "network_denied_nodes"),
        ("filesystem_restricted", "filesystem_restricted_nodes"),
        ("isolation_required", "isolation_required_nodes"),
        ("retry", "retry_policies"),
        ("policy_denied", "policy_denied_nodes"),
    ]
    .into_iter()
    .map(|(label, key)| format!("{label}={}", json_u64_field(policies, key).unwrap_or(0)))
    .collect::<Vec<_>>();
    Some(parts.join(" "))
}

fn json_u64_field(value: &serde_json::Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(|value| value.as_u64().or_else(|| value.as_i64()?.try_into().ok()))
}

async fn app_logs(run_id: Uuid, tail: usize, follow: bool, json: bool) -> anyhow::Result<()> {
    if json && follow {
        anyhow::bail!("agh app logs --json cannot be combined with --follow");
    }
    let pool = connect_db().await?;
    let event_limit = if tail == 0 { 500 } else { tail.clamp(1, 500) };
    let events = load_recent_app_run_events(&pool, run_id, event_limit).await?;
    let log_path = crate::dag_apps::app_run_log_path(run_id);
    let exists = log_path.is_file();
    let tail_text = if exists {
        Some(
            read_log_tail(&log_path, tail)
                .with_context(|| format!("read app run log tail from {}", log_path.display()))?,
        )
    } else {
        None
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&app_log_snapshot_json(
                run_id,
                &log_path,
                exists,
                tail_text.as_deref(),
                &events
            ))?
        );
        return Ok(());
    }

    let snapshot =
        format_app_log_snapshot(run_id, &log_path, tail_text.as_deref(), &events, follow);
    if !snapshot.is_empty() {
        print!("{snapshot}");
    }
    if tail_text.is_none() && follow {
        eprintln!("Waiting for app run log {}", log_path.display());
    }

    if follow {
        let offset = std::fs::metadata(&log_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let last_event_id = events.iter().map(|event| event.id).max().unwrap_or(0);
        follow_app_run_log(run_id, &log_path, offset, last_event_id).await?;
    }
    Ok(())
}

async fn app_events(run_id: Uuid, tail: usize, follow: bool, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let event_limit = if tail == 0 { 500 } else { tail.clamp(1, 500) };
    let events = load_recent_app_run_events(&pool, run_id, event_limit).await?;

    if json {
        if follow {
            print_app_run_event_json_lines(&events)?;
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&app_events_snapshot_json(run_id, &events))?
            );
        }
    } else {
        let snapshot = format_app_events_snapshot(run_id, &events, follow);
        if !snapshot.is_empty() {
            print!("{snapshot}");
        }
    }

    if follow {
        let last_event_id = events.iter().map(|event| event.id).max().unwrap_or(0);
        follow_app_run_events(run_id, last_event_id, json).await?;
    }
    Ok(())
}

fn app_events_snapshot_json(run_id: Uuid, events: &[AppRunEventSummary]) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "event_contract": app_event_contract_json(),
        "events": events.iter().map(app_run_event_json).collect::<Vec<_>>(),
    })
}

fn app_log_snapshot_json(
    run_id: Uuid,
    log_path: &Path,
    exists: bool,
    tail_text: Option<&str>,
    events: &[AppRunEventSummary],
) -> serde_json::Value {
    json!({
        "run_id": run_id,
        "log_path": log_path.to_string_lossy(),
        "exists": exists,
        "tail": tail_text.unwrap_or_default(),
        "log_contract": agenthero_agent_runtime::agenthero_log_contract(),
        "recent_events": events.iter().map(app_run_event_json).collect::<Vec<_>>(),
    })
}

fn app_event_contract_json() -> serde_json::Value {
    json!({
        "trace_fields": agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS,
    })
}

fn app_event_stream_contract_json() -> serde_json::Value {
    json!({
        "format": "server_sent_events",
        "cursor_parameter": "after_id",
        "limit_parameter": "limit",
        "event_id_field": "id",
        "data": "AppRunEvent JSON",
    })
}

fn format_app_events_snapshot(run_id: Uuid, events: &[AppRunEventSummary], follow: bool) -> String {
    let mut out = String::new();
    if events.is_empty() {
        if follow {
            let _ = writeln!(out, "Waiting for durable events for app run {run_id}.");
        } else {
            let _ = writeln!(out, "No durable events for app run {run_id}.");
        }
        return out;
    }
    out.push_str("durable_events\n");
    for event in events {
        let _ = writeln!(out, "{}", format_app_run_event_line(event));
    }
    out
}

fn print_app_run_event_json_lines(events: &[AppRunEventSummary]) -> anyhow::Result<()> {
    for event in events {
        println!("{}", serde_json::to_string(&app_run_event_json(event))?);
    }
    Ok(())
}

fn format_app_log_snapshot(
    run_id: Uuid,
    log_path: &Path,
    tail_text: Option<&str>,
    events: &[AppRunEventSummary],
    follow: bool,
) -> String {
    let mut out = String::new();
    if let Some(text) = tail_text {
        out.push_str(text);
    } else if !follow {
        let _ = writeln!(out, "No log file for app run {run_id}.");
        let _ = writeln!(out, "expected_log {}", log_path.display());
    }

    if !events.is_empty() {
        out.push_str("durable_events\n");
        for event in events {
            let _ = writeln!(out, "{}", format_app_run_event_line(event));
        }
    }
    out
}

fn read_log_tail(path: &Path, tail: usize) -> anyhow::Result<String> {
    let text = String::from_utf8_lossy(&std::fs::read(path)?).to_string();
    if tail == 0 {
        return Ok(text);
    }
    let lines = text.lines().collect::<Vec<_>>();
    let start = lines.len().saturating_sub(tail);
    let mut out = lines[start..].join("\n");
    if text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

async fn follow_app_run_log(
    run_id: Uuid,
    path: &Path,
    mut offset: u64,
    mut last_event_id: i64,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        offset = print_log_since(path, offset)?;
        last_event_id = print_app_run_events_since(&pool, run_id, last_event_id, false).await?;
        if let Some(state) = load_app_run_state(&pool, run_id).await? {
            if app_run_state_is_terminal(&state) {
                let _ = print_log_since(path, offset)?;
                let _ = print_app_run_events_since(&pool, run_id, last_event_id, false).await?;
                break;
            }
        }
    }
    Ok(())
}

async fn follow_app_run_events(
    run_id: Uuid,
    mut last_event_id: i64,
    json: bool,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        last_event_id = print_app_run_events_since(&pool, run_id, last_event_id, json).await?;
        if let Some(state) = load_app_run_state(&pool, run_id).await? {
            if app_run_state_is_terminal(&state) {
                let _ = print_app_run_events_since(&pool, run_id, last_event_id, json).await?;
                break;
            }
        }
    }
    Ok(())
}

async fn print_app_run_events_since(
    pool: &sqlx::PgPool,
    run_id: Uuid,
    last_event_id: i64,
    json: bool,
) -> anyhow::Result<i64> {
    let events = load_app_run_events_after(pool, run_id, last_event_id, 100).await?;
    if events.is_empty() {
        return Ok(last_event_id);
    }
    if json {
        print_app_run_event_json_lines(&events)?;
    } else {
        println!("durable_events");
        for event in &events {
            println!("{}", format_app_run_event_line(event));
        }
    }
    Ok(events
        .iter()
        .map(|event| event.id)
        .max()
        .unwrap_or(last_event_id))
}

fn print_log_since(path: &Path, offset: u64) -> anyhow::Result<u64> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(offset);
    };
    let mut offset = if meta.len() < offset { 0 } else { offset };
    if meta.len() == offset {
        return Ok(offset);
    }
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut chunk = String::new();
    file.read_to_string(&mut chunk)?;
    offset = meta.len();
    print!("{chunk}");
    Ok(offset)
}

async fn load_app_run_state(pool: &sqlx::PgPool, run_id: Uuid) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("select state from app_runs where id = $1")
            .bind(run_id)
            .fetch_optional(pool)
            .await?,
    )
}

fn app_run_state_is_terminal(state: &str) -> bool {
    matches!(
        state,
        "done" | "partial" | "awaiting_approval" | "failed" | "system_failed" | "cancelled"
    )
}

async fn app_work(
    run_id: Option<Uuid>,
    worker_name: Option<String>,
    json: bool,
    stream_app_stderr: bool,
    debug_logs: bool,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let claimed = crate::scheduler::work_once(
        pool.clone(),
        run_id,
        worker_name,
        stream_app_stderr,
        debug_logs,
    )
    .await?;
    let no_claim = if claimed.is_none() {
        match run_id {
            Some(run_id) => load_app_work_no_claim_summary(&pool, run_id).await?,
            None => None,
        }
    } else {
        None
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "claimed": claimed.is_some(),
                "run_id": claimed,
                "existing_run": no_claim.as_ref().map(app_work_no_claim_summary_json),
            }))?
        );
    } else if let Some(run_id) = claimed {
        println!("app run {run_id} completed");
    } else if let Some(run_id) = run_id {
        if let Some(summary) = no_claim {
            println!(
                "No queued app run claimed for id {run_id} (state={}, diagnosis={}).",
                summary.state, summary.diagnosis
            );
            println!(
                "log          {}",
                crate::dag_apps::app_run_log_path(run_id).display()
            );
        } else {
            println!("No queued app run claimed for id {run_id}.");
        }
    } else {
        println!("No queued app run claimed.");
    }
    Ok(())
}

struct AppWorkNoClaimSummary {
    state: String,
    diagnosis: &'static str,
}

fn app_work_no_claim_summary_json(summary: &AppWorkNoClaimSummary) -> serde_json::Value {
    json!({
        "state": summary.state,
        "diagnosis": summary.diagnosis,
    })
}

async fn load_app_work_no_claim_summary(
    pool: &sqlx::PgPool,
    run_id: Uuid,
) -> anyhow::Result<Option<AppWorkNoClaimSummary>> {
    let row = sqlx::query("select state from app_runs where id = $1")
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let state = row.get::<String, _>(0);
    let lease = load_latest_app_run_lease(pool, run_id).await?;
    let diagnosis = diagnose_app_run_status(
        &state,
        lease.as_ref().map(|lease| lease.state.as_str()),
        lease.as_ref().map(|lease| lease.leased_until),
        lease
            .as_ref()
            .and_then(|lease| lease.worker_name.as_deref()),
        chrono::Utc::now(),
    );
    Ok(Some(AppWorkNoClaimSummary { state, diagnosis }))
}

async fn app_approve_run(run_id: Uuid, key: &str, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let approved = crate::app_runs::approve_awaiting_run(&pool, run_id, key).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "run_id": run_id,
                "approved": approved,
                "key": key,
                "state": approved.then_some("queued"),
            }))?
        );
    } else if approved {
        println!("Approved {key} and requeued app run {run_id}.");
    } else {
        println!("No awaiting-approval app run found for id {run_id}.");
    }
    Ok(())
}

async fn load_latest_app_run_lease(
    pool: &sqlx::PgPool,
    run_id: Uuid,
) -> anyhow::Result<Option<AppRunLeaseSummary>> {
    let row = sqlx::query(
        "select wl.state, wl.leased_until, wn.name, wn.state, wn.last_heartbeat_at \
         from worker_leases wl \
         left join worker_nodes wn on wn.id = wl.worker_id \
         where wl.app_run_id = $1 \
         order by wl.created_at desc \
         limit 1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| AppRunLeaseSummary {
        state: row.get(0),
        leased_until: row.get(1),
        worker_name: row.get(2),
        worker_state: row.get(3),
        worker_last_heartbeat_at: row.get(4),
    }))
}

async fn load_recent_app_run_events(
    pool: &sqlx::PgPool,
    run_id: Uuid,
    limit: usize,
) -> anyhow::Result<Vec<AppRunEventSummary>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from dag_events \
         where app_run_id = $1 \
         order by created_at desc, id desc \
         limit $2",
    )
    .bind(run_id)
    .bind(i64::try_from(limit).unwrap_or(500).clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(chronological_event_tail(
        rows.into_iter()
            .map(|row| AppRunEventSummary {
                id: row.get(0),
                level: row.get(1),
                event_type: row.get(2),
                message: row.get(3),
                payload: row.get(4),
                created_at: row.get(5),
            })
            .collect(),
    ))
}

async fn count_app_run_events(pool: &sqlx::PgPool, run_id: Uuid) -> anyhow::Result<usize> {
    let count: i64 = sqlx::query_scalar("select count(*) from dag_events where app_run_id = $1")
        .bind(run_id)
        .fetch_one(pool)
        .await?;
    usize::try_from(count).map_err(|err| anyhow::anyhow!("event count overflow: {err}"))
}

fn chronological_event_tail(mut events: Vec<AppRunEventSummary>) -> Vec<AppRunEventSummary> {
    events.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    events
}

async fn load_app_run_events_after(
    pool: &sqlx::PgPool,
    run_id: Uuid,
    after_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<AppRunEventSummary>> {
    let rows = sqlx::query(
        "select id, level, event_type, message, payload, created_at \
         from dag_events \
         where app_run_id = $1 and id > $2 \
         order by id asc \
         limit $3",
    )
    .bind(run_id)
    .bind(after_id)
    .bind(i64::try_from(limit).unwrap_or(100).clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunEventSummary {
            id: row.get(0),
            level: row.get(1),
            event_type: row.get(2),
            message: row.get(3),
            payload: row.get(4),
            created_at: row.get(5),
        })
        .collect())
}

async fn load_app_run_queue_summary(
    pool: &sqlx::PgPool,
    run_id: Uuid,
    app_id: &str,
    action_id: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<AppRunQueueSummary> {
    let older_queued_count = sqlx::query_scalar::<_, i64>(
        "select count(*) from app_runs \
         where app_id = $1 \
           and action_id = $2 \
           and state = 'queued' \
           and created_at < $3 \
           and id <> $4",
    )
    .bind(app_id)
    .bind(action_id)
    .bind(created_at)
    .bind(run_id)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query(
        "select ar.id::text, ar.app_id, ar.action_id, ar.state, ar.input, \
                ar.created_at, ar.started_at, ar.finished_at, \
                (select count(*) from dag_events de where de.app_run_id = ar.id) as event_count \
         from app_runs ar \
         where ar.app_id = $1 \
           and ar.action_id = $2 \
           and ar.state = 'running' \
         order by ar.started_at asc nulls last, ar.created_at asc \
         limit 5",
    )
    .bind(app_id)
    .bind(action_id)
    .fetch_all(pool)
    .await?;
    let running_same_action = rows
        .into_iter()
        .map(|row| {
            let run_id = row.get::<String, _>(0);
            let input = row.get::<serde_json::Value, _>(4);
            let event_count = row.get::<i64, _>(8);
            AppRunListRow {
                id: run_id.clone(),
                app_id: row.get(1),
                action_id: row.get(2),
                state: row.get(3),
                review_id: app_run_review_id(&input),
                created_at: row.get(5),
                started_at: row.get(6),
                finished_at: row.get(7),
                observability: app_run_list_observability(&run_id, event_count),
            }
        })
        .collect();

    Ok(AppRunQueueSummary {
        older_queued_count,
        running_same_action,
    })
}

async fn app_cancel(run_id: Uuid, reason: Option<&str>, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let reason = reason
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Cancelled by operator");
    let row = sqlx::query(
        "update app_runs \
         set state = 'cancelled', finished_at = coalesce(finished_at, now()), updated_at = now(), \
             error_code = 'operator_cancelled', error_message = $2, error_retryable = false \
         where id = $1 and state in ('queued', 'running') \
         returning id::text, app_id, action_id, state, input, created_at, started_at, finished_at",
    )
    .bind(run_id)
    .bind(reason)
    .fetch_optional(&pool)
    .await
    .context("cancel app run")?;

    if row.is_some() {
        sqlx::query(
            "update worker_leases set state = 'failed', updated_at = now() \
             where app_run_id = $1 and state = 'leased'",
        )
        .bind(run_id)
        .execute(&pool)
        .await
        .context("release cancelled app run leases")?;
        insert_app_run_event(&pool, run_id, "info", "app_run.cancelled", reason).await?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "id": run_id,
                "cancelled": row.is_some(),
                "reason": reason,
            }))?
        );
    } else if let Some(row) = row {
        let input = row.get::<serde_json::Value, _>(4);
        let listed = AppRunListRow {
            id: row.get(0),
            app_id: row.get(1),
            action_id: row.get(2),
            state: row.get(3),
            review_id: app_run_review_id(&input),
            created_at: row.get(5),
            started_at: row.get(6),
            finished_at: row.get(7),
            observability: AppRunListObservability::default(),
        };
        println!("Cancelled app run:");
        print!("{}", format_app_run_table(&[listed], chrono::Utc::now()));
    } else {
        println!("No queued/running app run cancelled for id {run_id}.");
    }
    Ok(())
}

async fn app_cancel_queued(
    app: &str,
    action: Option<&str>,
    except: &[Uuid],
    older_than_mins: Option<i64>,
    dry_run: bool,
    reason: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let cutoff = older_than_mins
        .filter(|mins| *mins > 0)
        .map(|mins| chrono::Utc::now() - chrono::Duration::minutes(mins));
    let rows = sqlx::query(
        "select id::text, app_id, action_id, state, input, created_at, started_at, finished_at \
         from app_runs \
         where app_id = $1 \
           and state = 'queued' \
           and ($2::text is null or action_id = $2) \
           and ($3::timestamptz is null or created_at < $3) \
           and not (id = any($4::uuid[])) \
         order by created_at asc",
    )
    .bind(app)
    .bind(action)
    .bind(cutoff)
    .bind(except)
    .fetch_all(&pool)
    .await
    .context("load queued app runs to cancel")?;
    let listed = rows
        .iter()
        .map(|row| {
            let input = row.get::<serde_json::Value, _>(4);
            AppRunListRow {
                id: row.get(0),
                app_id: row.get(1),
                action_id: row.get(2),
                state: row.get(3),
                review_id: app_run_review_id(&input),
                created_at: row.get(5),
                started_at: row.get(6),
                finished_at: row.get(7),
                observability: AppRunListObservability::default(),
            }
        })
        .collect::<Vec<_>>();
    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "dry_run": true,
                    "matched": listed.len(),
                    "runs": app_run_rows_json(&listed),
                }))?
            );
        } else if listed.is_empty() {
            println!("No queued app runs matched.");
        } else {
            println!("Queued app runs that would be cancelled:");
            print!("{}", format_app_run_table(&listed, chrono::Utc::now()));
        }
        return Ok(());
    }

    let ids = listed
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.id).ok())
        .collect::<Vec<_>>();
    let reason = reason
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Cancelled stale queued app run");
    if !ids.is_empty() {
        sqlx::query(
            "update app_runs \
             set state = 'cancelled', finished_at = now(), updated_at = now(), \
                 error_code = 'operator_cancelled', error_message = $2, error_retryable = false \
             where id = any($1::uuid[])",
        )
        .bind(&ids)
        .bind(reason)
        .execute(&pool)
        .await
        .context("cancel queued app runs")?;
        for id in ids {
            insert_app_run_event(&pool, id, "info", "app_run.cancelled", reason).await?;
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "cancelled": listed.len(),
                "reason": reason,
                "runs": app_run_rows_json(&listed),
            }))?
        );
    } else if listed.is_empty() {
        println!("No queued app runs matched.");
    } else {
        println!("Cancelled queued app runs:");
        print!("{}", format_app_run_table(&listed, chrono::Utc::now()));
    }
    Ok(())
}

async fn insert_app_run_event(
    pool: &sqlx::PgPool,
    run_id: Uuid,
    level: &str,
    event_type: &str,
    message: &str,
) -> anyhow::Result<()> {
    let runtime_context = load_operator_event_runtime_context(pool, run_id)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                err = %err,
                event_type = "app_run.operator_event_context_load_failed",
                app_run_id = %run_id,
                dag_run_id = "",
                node_id = "",
                attempt = 0_i32,
                node_kind = "",
                tool_id = "",
                manifest_hash = "",
                artifact_id = "",
                lease_id = "",
                status = "degraded",
                exit_status = 0_i64,
                duration_ms = 0_u64,
                "failed to load operator event runtime context"
            );
            OperatorEventRuntimeContext::default()
        });
    crate::app_runs::insert_event(
        pool,
        run_id,
        level,
        event_type,
        Some(message),
        operator_app_run_event_payload(run_id, runtime_context),
    )
    .await
}

#[derive(Debug, Default, Clone, Copy)]
struct OperatorEventRuntimeContext {
    dag_run_id: Option<Uuid>,
    lease_id: Option<Uuid>,
}

async fn load_operator_event_runtime_context(
    pool: &sqlx::PgPool,
    run_id: Uuid,
) -> anyhow::Result<OperatorEventRuntimeContext> {
    let row = sqlx::query(
        "select \
            (select dr.id from dag_runs dr where dr.app_run_id = $1 order by dr.created_at desc limit 1) as dag_run_id, \
            (select wl.id from worker_leases wl where wl.app_run_id = $1 order by wl.updated_at desc limit 1) as lease_id",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await?;
    Ok(OperatorEventRuntimeContext {
        dag_run_id: row.try_get("dag_run_id")?,
        lease_id: row.try_get("lease_id")?,
    })
}

fn operator_app_run_event_payload(
    run_id: Uuid,
    runtime_context: OperatorEventRuntimeContext,
) -> serde_json::Value {
    agenthero_agent_runtime::agenthero_trace_payload(
        run_id,
        None,
        json!({
            "operator": "cli",
            "dag_run_id": runtime_context.dag_run_id.map(|id| id.to_string()),
            "lease_id": runtime_context.lease_id.map(|id| id.to_string()),
        }),
    )
}

fn app_run_review_id(input: &serde_json::Value) -> Option<String> {
    input
        .get("args")
        .and_then(|args| args.as_array())
        .and_then(|args| args.first())
        .and_then(|value| value.as_str())
        .filter(|value| Uuid::parse_str(value).is_ok())
        .map(ToOwned::to_owned)
}

fn app_run_list_row_from_value(value: &serde_json::Value) -> Option<AppRunListRow> {
    Some(AppRunListRow {
        id: value.get("id")?.as_str()?.to_string(),
        app_id: value.get("app_id")?.as_str()?.to_string(),
        action_id: value.get("action_id")?.as_str()?.to_string(),
        state: value.get("state")?.as_str()?.to_string(),
        review_id: value
            .get("review_id")
            .and_then(|review_id| review_id.as_str())
            .map(ToOwned::to_owned),
        created_at: value
            .get("created_at")?
            .as_str()?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .ok()?,
        started_at: value
            .get("started_at")
            .and_then(|ts| ts.as_str())
            .and_then(|ts| ts.parse::<chrono::DateTime<chrono::Utc>>().ok()),
        finished_at: value
            .get("finished_at")
            .and_then(|ts| ts.as_str())
            .and_then(|ts| ts.parse::<chrono::DateTime<chrono::Utc>>().ok()),
        observability: value
            .get("observability")
            .map(app_run_list_observability_from_json)
            .unwrap_or_default(),
    })
}

fn app_run_list_observability(run_id: &str, event_count: i64) -> AppRunListObservability {
    let event_count = usize::try_from(event_count).unwrap_or(0);
    let (log_exists, log_bytes) = Uuid::parse_str(run_id)
        .ok()
        .map(|run_id| {
            let path = crate::dag_apps::app_run_log_path(run_id);
            let metadata = std::fs::metadata(&path).ok();
            let log_exists = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
            let log_bytes = metadata.map(|metadata| metadata.len());
            (log_exists, log_bytes)
        })
        .unwrap_or((false, None));
    AppRunListObservability {
        event_count,
        log_exists,
        log_bytes,
    }
}

fn app_run_list_observability_from_json(value: &serde_json::Value) -> AppRunListObservability {
    AppRunListObservability {
        event_count: json_u64_field(value, "event_count")
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0),
        log_exists: value
            .get("log_exists")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        log_bytes: json_u64_field(value, "log_bytes"),
    }
}

fn app_run_list_observability_json(summary: &AppRunListObservability) -> serde_json::Value {
    json!({
        "event_count": summary.event_count,
        "log_exists": summary.log_exists,
        "log_bytes": summary.log_bytes,
    })
}

fn app_run_rows_json(rows: &[AppRunListRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            json!({
                "id": row.id,
                "app_id": row.app_id,
                "action_id": row.action_id,
                "state": row.state,
                "review_id": row.review_id,
                "created_at": row.created_at,
                "started_at": row.started_at,
                "finished_at": row.finished_at,
                "observability": app_run_list_observability_json(&row.observability),
            })
        })
        .collect()
}

fn app_run_lease_json(lease: &AppRunLeaseSummary) -> serde_json::Value {
    json!({
        "state": lease.state,
        "leased_until": lease.leased_until,
        "worker_name": lease.worker_name,
        "worker_state": lease.worker_state,
        "worker_last_heartbeat_at": lease.worker_last_heartbeat_at,
    })
}

fn app_run_event_json(event: &AppRunEventSummary) -> serde_json::Value {
    json!({
        "id": event.id,
        "level": event.level,
        "event_type": event.event_type,
        "node_id": event.payload.get("node_id").cloned(),
        "attempt": event.payload.get("attempt").cloned(),
        "message": event.message,
        "payload": event.payload,
        "created_at": event.created_at,
    })
}

fn format_live_node_status_line(node: &serde_json::Value) -> String {
    let mut line = format!(
        "  {:<32} {:<18} attempt={} event={} kind={}",
        node.get("node_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
        node.get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
        node.get("attempt")
            .map(ToString::to_string)
            .unwrap_or_else(|| "-".to_string()),
        node.get("event_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-"),
        node.get("node_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-")
    );
    if let Some(exit_status) = live_node_i64_field(node, "exit_status") {
        line.push_str(&format!(" exit_status={exit_status}"));
    }
    if let Some(duration_ms) =
        live_node_i64_field(node, "duration_ms").or_else(|| live_node_i64_field(node, "latency_ms"))
    {
        line.push_str(&format!(" duration_ms={duration_ms}"));
    }
    line.push_str(&format!(
        " updated={}",
        node.get("updated_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-")
    ));
    line
}

fn live_node_i64_field(node: &serde_json::Value, field: &str) -> Option<i64> {
    node.get(field)
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            node.get("payload")
                .and_then(|payload| payload.get(field))
                .and_then(serde_json::Value::as_i64)
        })
}

fn format_app_run_event_line(event: &AppRunEventSummary) -> String {
    let mut fields = Vec::new();
    if let Some(node_id) = event
        .payload
        .get("node_id")
        .and_then(serde_json::Value::as_str)
    {
        fields.push(format!("node={node_id}"));
    }
    if let Some(attempt) = event
        .payload
        .get("attempt")
        .and_then(serde_json::Value::as_i64)
    {
        fields.push(format!("attempt={attempt}"));
    }
    push_payload_string_alias(&mut fields, &event.payload, "kind", &["node_kind", "kind"]);
    push_payload_string_alias(&mut fields, &event.payload, "tool", &["tool_id", "tool"]);
    push_payload_string_alias(&mut fields, &event.payload, "model", &["model"]);
    push_payload_string_alias(&mut fields, &event.payload, "prompt_hash", &["prompt_hash"]);
    if let Some(command) = event.payload.get("command").and_then(compact_json_value) {
        fields.push(format!("command={command}"));
    }
    for field in [
        "next_attempt",
        "max_attempts",
        "backoff_ms",
        "exit_status",
        "duration_ms",
        "latency_ms",
    ] {
        push_payload_i64_field(&mut fields, &event.payload, field, field);
    }
    if let Some(status) = event
        .payload
        .get("status")
        .and_then(serde_json::Value::as_str)
    {
        fields.push(format!("status={status}"));
    }
    if let Some(state) = event
        .payload
        .get("state")
        .and_then(serde_json::Value::as_str)
    {
        fields.push(format!("state={state}"));
    }
    let suffix = if fields.is_empty() {
        String::new()
    } else {
        format!(" {}", fields.join(" "))
    };
    format!(
        "  {} {:<5} {:<28} {}{}",
        format_app_run_ts(Some(event.created_at)),
        event.level,
        event.event_type,
        event.message.as_deref().unwrap_or(""),
        suffix
    )
}

fn push_payload_string_alias(
    fields: &mut Vec<String>,
    payload: &serde_json::Value,
    label: &str,
    keys: &[&str],
) {
    for key in keys {
        if let Some(value) = payload.get(key).and_then(serde_json::Value::as_str) {
            fields.push(format!("{label}={value}"));
            return;
        }
    }
}

fn push_payload_i64_field(
    fields: &mut Vec<String>,
    payload: &serde_json::Value,
    label: &str,
    key: &str,
) {
    if let Some(value) = payload.get(key).and_then(serde_json::Value::as_i64) {
        fields.push(format!("{label}={value}"));
    }
}

fn compact_json_value(value: &serde_json::Value) -> Option<String> {
    if value.is_null() {
        None
    } else {
        serde_json::to_string(value).ok()
    }
}

fn app_run_queue_summary_json(summary: &AppRunQueueSummary) -> serde_json::Value {
    json!({
        "position": summary.older_queued_count.saturating_add(1),
        "older_queued_count": summary.older_queued_count,
        "running_same_action_count": summary.running_same_action.len(),
        "running_same_action": app_run_rows_json(&summary.running_same_action),
    })
}

fn format_app_run_table(rows: &[AppRunListRow], now: chrono::DateTime<chrono::Utc>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<14} {:<20} {:<36} {:<8} {:<17} {:<17} {:<17} {:<22} {}\n",
        "STATE",
        "APP:ACTION",
        "REVIEW_ID",
        "AGE",
        "QUEUED_AT",
        "STARTED_AT",
        "FINISHED_AT",
        "OBSERVABILITY",
        "RUN_ID"
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<14} {:<20} {:<36} {:<8} {:<17} {:<17} {:<17} {:<22} {}\n",
            row.state,
            format!("{}:{}", row.app_id, row.action_id),
            row.review_id.as_deref().unwrap_or("-"),
            format_app_run_age(row.created_at, now),
            format_app_run_ts(Some(row.created_at)),
            format_app_run_ts(row.started_at),
            format_app_run_ts(row.finished_at),
            format_app_run_list_observability(&row.observability),
            row.id
        ));
    }
    out
}

fn format_app_run_list_observability(summary: &AppRunListObservability) -> String {
    let log = if summary.log_exists {
        summary
            .log_bytes
            .map(|bytes| format!("{bytes}B"))
            .unwrap_or_else(|| "exists".to_string())
    } else {
        "missing".to_string()
    };
    format!("events={} log={log}", summary.event_count)
}

fn format_app_run_ts(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    ts.map(|ts| ts.format("%m-%d %H:%M:%SZ").to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn format_app_run_age(
    created_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = now.signed_duration_since(created_at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn diagnose_app_run_status(
    state: &str,
    lease_state: Option<&str>,
    leased_until: Option<chrono::DateTime<chrono::Utc>>,
    worker_name: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> &'static str {
    match state {
        "queued" => "queued_unclaimed",
        "running" => match (lease_state, leased_until, worker_name) {
            (Some("leased"), Some(until), Some(_)) if until > now => "running_with_active_lease",
            (Some("leased"), Some(_), Some(_)) => "running_with_expired_lease",
            (Some("leased"), _, None) => "running_with_lease_missing_worker",
            (Some(other), _, _) if other != "leased" => "running_with_inactive_lease",
            _ => "running_without_lease",
        },
        "done" | "partial" => "finished",
        "failed" | "system_failed" => "failed",
        "cancelled" => "cancelled",
        "awaiting_approval" => "awaiting_approval",
        _ => "unknown",
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

async fn run_dag_app_command(dag_type: &str, json: bool) -> anyhow::Result<()> {
    let report =
        crate::dag_apps::run_registered_dag_app(dag_type, agenthero_dag_executor::DagIo::default())
            .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "ok {} status={:?} nodes={}",
            report.dag_type,
            report.status,
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
        let rows = manifests
            .iter()
            .map(|manifest| {
                json!({
                    "id": manifest.id.as_str(),
                    "version": manifest.version,
                    "roles": manifest.roles.len(),
                    "nodes": manifest.nodes.len(),
                    "layers": manifest.execution_layers().map(|layers| layers.len()).unwrap_or(0),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "dags": rows }))?
        );
    } else {
        for manifest in &manifests {
            println!(
                "ok {} version={} roles={} nodes={} layers={}",
                manifest.id.as_str(),
                manifest.version,
                manifest.roles.len(),
                manifest.nodes.len(),
                manifest.execution_layers()?.len()
            );
        }
    }
    Ok(())
}

fn validate_declared_tools(manifest: &DagManifest) -> anyhow::Result<()> {
    for tool in &manifest.tools {
        match tool.executor {
            ToolExecutorKind::Rust => {}
            ToolExecutorKind::Cli
            | ToolExecutorKind::Shell
            | ToolExecutorKind::Python
            | ToolExecutorKind::RustBinary
            | ToolExecutorKind::Http
            | ToolExecutorKind::Lean
            | ToolExecutorKind::Haskell
            | ToolExecutorKind::Docker
            | ToolExecutorKind::Wasm => {
                if tool.command.as_ref().map(Vec::is_empty).unwrap_or(true) {
                    anyhow::bail!(
                        "DAG `{}` command-backed tool `{}` must declare command",
                        manifest.id,
                        tool.id
                    );
                }
            }
            ToolExecutorKind::Llm | ToolExecutorKind::ApprovalGate => {}
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
    let path = dag_manifest_path(dag_type)?;
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
        retry: None,
    });
    push_edges(&mut manifest, role_id, after, before);
    manifest.validate()?;
    emit_or_write_manifest(&path, &manifest, write, json)
}

fn remove_agent_from_dag(
    dag_type: &str,
    role_id: &str,
    write: bool,
    json: bool,
) -> anyhow::Result<()> {
    let path = dag_manifest_path(dag_type)?;
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let before_roles = manifest.roles.len();
    manifest.roles.retain(|role| role.id.as_str() != role_id);
    if before_roles == manifest.roles.len() {
        anyhow::bail!("DAG `{dag_type}` has no role `{role_id}`");
    }
    let removed = manifest
        .nodes
        .iter()
        .filter(|node| {
            node.role
                .as_ref()
                .map(|role| role.as_str() == role_id)
                .unwrap_or(false)
        })
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    remove_nodes_and_edges(&mut manifest, &removed);
    manifest.validate()?;
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
    let path = dag_manifest_path(dag_type)?;
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
        ToolExecutorKind::Cli
        | ToolExecutorKind::Shell
        | ToolExecutorKind::Python
        | ToolExecutorKind::RustBinary
        | ToolExecutorKind::Llm
        | ToolExecutorKind::Http
        | ToolExecutorKind::Lean
        | ToolExecutorKind::Haskell
        | ToolExecutorKind::Docker
        | ToolExecutorKind::Wasm
        | ToolExecutorKind::ApprovalGate => handler,
    };
    manifest.tools.push(DagTool {
        id: tool_id.to_string(),
        executor,
        handler,
        command: (!command.is_empty()).then_some(command),
        timeout_secs,
        input_schema: None,
        output_schema: None,
        policy: None,
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
        retry: None,
    });
    push_edges(&mut manifest, tool_id, after, before);
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
    let path = dag_manifest_path(dag_type)?;
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let before_tools = manifest.tools.len();
    manifest.tools.retain(|tool| tool.id != tool_id);
    if before_tools == manifest.tools.len() {
        anyhow::bail!("DAG `{dag_type}` has no tool `{tool_id}`");
    }
    let removed = manifest
        .nodes
        .iter()
        .filter(|node| node.tool.as_deref() == Some(tool_id))
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    remove_nodes_and_edges(&mut manifest, &removed);
    manifest.validate()?;
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

fn push_edges(manifest: &mut DagManifest, node_id: &str, after: Vec<String>, before: Vec<String>) {
    for source in after {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(source),
            to: OneOrMany::One(node_id.to_string()),
        });
    }
    for target in before {
        manifest.edges.push(DagEdge {
            from: OneOrMany::One(node_id.to_string()),
            to: OneOrMany::One(target),
        });
    }
}

fn remove_nodes_and_edges(manifest: &mut DagManifest, removed: &HashSet<String>) {
    manifest.nodes.retain(|node| !removed.contains(&node.id));
    manifest.edges = manifest
        .edges
        .drain(..)
        .filter_map(|edge| {
            Some(DagEdge {
                from: strip_one_or_many(edge.from, removed)?,
                to: strip_one_or_many(edge.to, removed)?,
            })
        })
        .collect();
}

fn strip_one_or_many(values: OneOrMany, needles: &HashSet<String>) -> Option<OneOrMany> {
    match values {
        OneOrMany::One(value) => (!needles.contains(&value)).then_some(OneOrMany::One(value)),
        OneOrMany::Many(values) => {
            let kept = values
                .into_iter()
                .filter(|value| !needles.contains(value))
                .collect::<Vec<_>>();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next().map(OneOrMany::One),
                _ => Some(OneOrMany::Many(kept)),
            }
        }
    }
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
                serde_json::to_string_pretty(&json!({
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
    let manifest_path = dag_manifest_path(manifest.id.as_str())?;
    let app_root = manifest_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    for role in &manifest.roles {
        let Some(config) = role.config.as_deref() else {
            continue;
        };
        let path = resolve_agent_config_path(app_root, config);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "DAG `{}` role `{}` config {}",
                manifest.id,
                role.id,
                path.display()
            )
        })?;
        let value = serde_yaml::from_str::<serde_yaml::Value>(&text)
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

fn resolve_agent_config_path(app_root: &Path, config: &str) -> PathBuf {
    let path = PathBuf::from(config);
    if path.is_absolute() {
        return path;
    }
    if let Ok(stripped) = path.strip_prefix("agents") {
        return app_root.join("agents").join(stripped);
    }
    app_root.join(path)
}

async fn agent_command(command: AgentCommand, json: bool) -> anyhow::Result<()> {
    match command {
        AgentCommand::Place { path } => place_agent(&path, json),
    }
}

fn place_agent(path: &Path, json: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value = serde_yaml::from_str::<serde_yaml::Value>(&text)
        .with_context(|| format!("parse {}", path.display()))?;
    let kind_value = value
        .get("kind")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("agent YAML {} is missing `kind`", path.display()))?;
    let kind: AgentKind = serde_yaml::from_value(kind_value)
        .with_context(|| format!("parse `kind` in {}", path.display()))?;
    let compatible = DagManifest::compatible_dag_ids(&load_repo_dag_manifests(None)?, kind.clone());
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
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
        paths.push(dag_manifest_path(id)?);
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

fn dag_manifest_path(dag_type: &str) -> anyhow::Result<PathBuf> {
    if let Some(app) = crate::dag_apps::registered_dag_app(dag_type)? {
        return Ok(app.manifest_path);
    }
    Ok(crate::dag_apps::apps_root()
        .join("unknown")
        .join("dags")
        .join(format!("{dag_type}.yaml")))
}

fn print_config(show_secrets: bool, json: bool) -> anyhow::Result<()> {
    let cfg = crate::config::Config::from_env()?;
    let redact = |value: Option<&str>| match value {
        Some(secret) if show_secrets => secret.to_string(),
        Some(_) => "***".to_string(),
        None => "<unset>".to_string(),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "bind": cfg.bind,
                "database_url": redact(cfg.database_url.as_deref()),
                "apps_root": crate::dag_apps::apps_root(),
            }))?
        );
    } else {
        println!("bind         = {}", cfg.bind);
        println!("database_url = {}", redact(cfg.database_url.as_deref()));
        println!("apps_root    = {}", crate::dag_apps::apps_root().display());
    }
    Ok(())
}

async fn jobs_command(command: JobsCommand, json: bool) -> anyhow::Result<()> {
    match command {
        JobsCommand::List { kind, state, limit } => {
            let pool = connect_db().await?;
            let rows = sqlx::query(
                "select id::text, kind, state, attempt, ref_id::text, error \
                 from jobs \
                 where ($1::text is null or kind = $1) \
                   and ($2::text is null or state = $2) \
                 order by created_at desc \
                 limit $3",
            )
            .bind(normalize_filter(kind))
            .bind(normalize_filter(state))
            .bind(limit as i64)
            .fetch_all(&pool)
            .await
            .context("list jobs")?;
            let values = rows
                .iter()
                .map(|row| {
                    json!({
                        "id": row.get::<String, _>(0),
                        "kind": row.get::<String, _>(1),
                        "state": row.get::<String, _>(2),
                        "attempt": row.get::<i32, _>(3),
                        "ref_id": row.get::<Option<String>, _>(4),
                        "error": row.get::<Option<String>, _>(5),
                    })
                })
                .collect::<Vec<_>>();
            if json {
                println!("{}", serde_json::to_string_pretty(&values)?);
            } else if values.is_empty() {
                println!("(no jobs)");
            } else {
                for value in values {
                    println!(
                        "{} {} {} attempt={}",
                        value["id"].as_str().unwrap_or_default(),
                        value["kind"].as_str().unwrap_or_default(),
                        value["state"].as_str().unwrap_or_default(),
                        value["attempt"]
                    );
                }
            }
            Ok(())
        }
    }
}

fn normalize_filter(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|raw| !raw.is_empty())
}

async fn connect_db() -> anyhow::Result<sqlx::PgPool> {
    let url = crate::config::Config::from_env()?
        .database_url
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL not configured"))?;
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn app_run_adapter_input_carries_status_and_debug_flags() {
        let input = app_run_adapter_input("demo", "run", "demo-dag", true, true);

        assert_eq!(input.values["app"], json!("demo"));
        assert_eq!(input.values["action"], json!("run"));
        assert_eq!(input.values["dag_type"], json!("demo-dag"));
        assert_eq!(input.values["stream_stderr"], json!(true));
        assert_eq!(input.values["debug_logs"], json!(true));
    }

    #[test]
    fn completed_foreground_app_run_replays_stored_adapter_response() {
        let response = agenthero_agent_runtime::AppAdapterResponse {
            protocol: agenthero_agent_runtime::APP_ADAPTER_PROTOCOL.to_string(),
            app: "demo".to_string(),
            action: "run".to_string(),
            dag_type: "demo-dag".to_string(),
            ok: true,
            report: None,
            output: Some(json!({"status": "ok"})),
            error: None,
        };
        let record = crate::app_runs::AppRunRecord {
            id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            app_id: "demo".to_string(),
            action_id: "run".to_string(),
            state: "done".to_string(),
            input: json!({}),
            output: serde_json::to_value(&response).unwrap(),
            error_code: None,
            error_message: None,
            error_retryable: None,
            attempt: 1,
            created_at: chrono::Utc::now(),
            started_at: Some(chrono::Utc::now()),
            finished_at: Some(chrono::Utc::now()),
        };

        let stored = successful_app_run_response(&record).expect("stored response");

        assert_eq!(stored.output, Some(json!({"status": "ok"})));
        assert!(stored.ok);
    }

    #[test]
    fn failed_foreground_app_run_reports_durable_failure_message() {
        let record = crate::app_runs::AppRunRecord {
            id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            app_id: "demo".to_string(),
            action_id: "run".to_string(),
            state: "system_failed".to_string(),
            input: json!({}),
            output: json!({}),
            error_code: Some("adapter_system_failed".to_string()),
            error_message: Some("network resolver stalled".to_string()),
            error_retryable: Some(true),
            attempt: 1,
            created_at: chrono::Utc::now(),
            started_at: Some(chrono::Utc::now()),
            finished_at: Some(chrono::Utc::now()),
        };

        let err = successful_app_run_response(&record).expect_err("failed run should error");

        assert!(format!("{err:#}").contains("network resolver stalled"));
        assert!(format!("{err:#}").contains("22222222-2222-2222-2222-222222222222"));
    }

    #[test]
    fn app_run_table_shows_review_id_and_queue_time() {
        let created_at = chrono::Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 6, 17, 12, 42, 0)
            .unwrap();
        let rows = vec![AppRunListRow {
            id: "68c3a3dd-4ae0-402a-82cc-953153b36702".to_string(),
            app_id: "demo".to_string(),
            action_id: "formalize".to_string(),
            state: "queued".to_string(),
            review_id: Some("dc06005a-9bc1-4222-8779-10d4c26dd7e2".to_string()),
            created_at,
            started_at: None,
            finished_at: None,
            observability: AppRunListObservability {
                event_count: 12,
                log_exists: true,
                log_bytes: Some(4096),
            },
        }];

        let table = format_app_run_table(&rows, now);

        assert!(table.contains("QUEUED_AT"));
        assert!(table.contains("OBSERVABILITY"));
        assert!(table.contains("events=12 log=4096B"));
        assert!(table.contains("dc06005a-9bc1-4222-8779-10d4c26dd7e2"));
        assert!(table.contains("42m"));
        assert!(table.contains("06-17 12:00:00Z"));

        let json = app_run_rows_json(&rows);
        assert_eq!(
            json[0]["observability"]["event_count"],
            serde_json::json!(12)
        );
        assert_eq!(
            json[0]["observability"]["log_exists"],
            serde_json::json!(true)
        );
        assert_eq!(
            json[0]["observability"]["log_bytes"],
            serde_json::json!(4096)
        );
    }

    #[test]
    fn app_run_status_diagnosis_distinguishes_unclaimed_and_expired_runs() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 6, 17, 12, 0, 0).unwrap();

        assert_eq!(
            diagnose_app_run_status("queued", None, None, None, now),
            "queued_unclaimed"
        );
        assert_eq!(
            diagnose_app_run_status(
                "running",
                Some("leased"),
                Some(now - chrono::Duration::minutes(1)),
                Some("worker-1"),
                now,
            ),
            "running_with_expired_lease"
        );
        assert_eq!(
            diagnose_app_run_status(
                "running",
                Some("leased"),
                Some(now + chrono::Duration::minutes(1)),
                Some("worker-1"),
                now,
            ),
            "running_with_active_lease"
        );
        assert_eq!(
            diagnose_app_run_status("awaiting_approval", None, None, None, now),
            "awaiting_approval"
        );
    }

    #[test]
    fn app_status_observability_block_points_to_platform_surfaces() {
        let run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let log_path = Path::new(".agenthero/app_runs/11111111-1111-1111-1111-111111111111.log");

        let observability = app_run_observability_json(
            run_id,
            log_path,
            Some("sample-app"),
            Some("run"),
            Some("sample-dag"),
            AppStatusObservabilitySummary {
                total_event_count: 12,
                recent_event_count: 5,
                log_exists: true,
                log_bytes: Some(4096),
            },
        );

        assert_eq!(
            observability["status_command"],
            json!("agh app status 11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            observability["logs_command"],
            json!("agh app logs 11111111-1111-1111-1111-111111111111 --follow")
        );
        assert_eq!(
            observability["events_command"],
            json!("agh app events 11111111-1111-1111-1111-111111111111 --follow")
        );
        assert_eq!(
            observability["events_path"],
            json!("/app-runs/11111111-1111-1111-1111-111111111111/events")
        );
        assert_eq!(
            observability["event_stream_path"],
            json!("/app-runs/11111111-1111-1111-1111-111111111111/events/stream")
        );
        assert_eq!(
            observability["logs_path"],
            json!("/app-runs/11111111-1111-1111-1111-111111111111/logs")
        );
        assert_eq!(observability["metrics_path"], json!("/metrics"));
        assert_eq!(observability["metrics_labels"]["app"], json!("sample-app"));
        assert_eq!(observability["metrics_labels"]["action"], json!("run"));
        assert_eq!(
            observability["metrics_labels"]["dag_type"],
            json!("sample-dag")
        );
        assert_eq!(observability["trace_fields"][0], json!("app_run_id"));
        assert_eq!(
            observability["event_contract"]["trace_fields"][0],
            json!("app_run_id")
        );
        assert_eq!(
            observability["log_contract"]["format"],
            json!("durable_text_log_with_agenthero_event_jsonl")
        );
        assert_eq!(
            observability["stream_contract"]["format"],
            json!("server_sent_events")
        );
        assert_eq!(
            observability["stream_contract"]["cursor_parameter"],
            json!("after_id")
        );
        assert_eq!(observability["summary"]["total_event_count"], json!(12));
        assert_eq!(observability["summary"]["recent_event_count"], json!(5));
        assert_eq!(observability["summary"]["log_exists"], json!(true));
        assert_eq!(observability["summary"]["log_bytes"], json!(4096));
    }

    #[test]
    fn app_status_formats_metric_label_scope_for_human_output() {
        let scope = format_metrics_label_scope(&json!({
                "metrics_labels": {
                "app": "sample-app",
                "action": "run",
                "dag_type": "sample-dag"
            }
        }));

        assert_eq!(
            scope.as_deref(),
            Some("app=sample-app action=run dag_type=sample-dag")
        );
    }

    #[test]
    fn app_status_formats_observability_summary_for_human_output() {
        let line = format_observability_summary_line(&json!({
            "summary": {
                "total_event_count": 14,
                "recent_event_count": 5,
                "log_exists": true,
                "log_bytes": 7898
            }
        }));

        assert_eq!(
            line.as_deref(),
            Some("events=14 recent=5 log_exists=true log_bytes=7898")
        );
    }

    #[test]
    fn app_status_formats_policy_summary_for_human_output() {
        let line = format_policy_summary_line(&json!({
            "node_attempts": 3,
            "timeout_limited_nodes": 2,
            "budget_limited_nodes": 1,
            "budget_units_requested": 42,
            "approval_gates": 1,
            "approval_required_tools": 1,
            "network_denied_nodes": 1,
            "filesystem_restricted_nodes": 1,
            "isolation_required_nodes": 2,
            "retry_policies": 2,
            "policy_denied_nodes": 1
        }));

        assert_eq!(
            line.as_deref(),
            Some(
                "nodes=3 timeout=2 budget=1 units=42 approval_gates=1 approval_required=1 network_denied=1 filesystem_restricted=1 isolation_required=2 retry=2 policy_denied=1"
            )
        );
    }

    #[test]
    fn app_compare_human_output_surfaces_work_product_match() {
        let left_run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let right_run_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let comparison = crate::app_runs::AppRunComparison {
            left: crate::app_runs::AppRunComparisonSide {
                run_id: left_run_id,
                app_id: "platform-smoke".to_string(),
                action_id: "verification-routing-smoke".to_string(),
                state: "done".to_string(),
                dag_run_id: None,
                dag_type: Some("verification-routing-smoke".to_string()),
                determinism: crate::app_runs::AppRunDeterminismSummary::default(),
                normalized_frozen_input_hash: Some("left-normalized-input".to_string()),
                normalized_dag_output_hash: Some("left-normalized-output".to_string()),
            },
            right: crate::app_runs::AppRunComparisonSide {
                run_id: right_run_id,
                app_id: "platform-smoke".to_string(),
                action_id: "verification-routing-smoke".to_string(),
                state: "done".to_string(),
                dag_run_id: None,
                dag_type: Some("verification-routing-smoke".to_string()),
                determinism: crate::app_runs::AppRunDeterminismSummary::default(),
                normalized_frozen_input_hash: Some("right-normalized-input".to_string()),
                normalized_dag_output_hash: Some("right-normalized-output".to_string()),
            },
            compare_ready: true,
            matches: false,
            work_product_matches: true,
            checks: crate::app_runs::AppRunComparisonChecks {
                same_app: true,
                same_action: true,
                same_dag_type: true,
                same_manifest_hash: true,
                same_frozen_input_hash: false,
                same_dag_output_hash: false,
                same_node_outputs: false,
                same_artifacts: true,
                same_normalized_frozen_input_hash: true,
                same_normalized_dag_output_hash: true,
                same_normalized_node_outputs: true,
            },
            differences: Vec::new(),
            work_product_differences: Vec::new(),
        };

        let output = format_app_run_comparison(&comparison);

        assert!(output.contains("matches       false"));
        assert!(output.contains("work_product true"));
        assert!(output.contains("normalized    input=true output=true node_outputs=true"));
    }

    #[test]
    fn operator_app_run_event_payload_includes_trace_field_contract() {
        let run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();

        let payload =
            operator_app_run_event_payload(run_id, OperatorEventRuntimeContext::default());

        assert_eq!(
            payload["app_run_id"],
            json!("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(payload["operator"], json!("cli"));
        for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
            assert!(
                payload.get(*field).is_some(),
                "operator app-run event payload should include mandatory AgentHero trace field `{field}`"
            );
        }
        assert_eq!(payload["node_id"], serde_json::Value::Null);
        assert_eq!(payload["attempt"], serde_json::Value::Null);
    }

    #[test]
    fn operator_app_run_event_payload_preserves_runtime_context_when_available() {
        let run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let dag_run_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let lease_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();

        let payload = operator_app_run_event_payload(
            run_id,
            OperatorEventRuntimeContext {
                dag_run_id: Some(dag_run_id),
                lease_id: Some(lease_id),
            },
        );

        assert_eq!(payload["app_run_id"], json!(run_id.to_string()));
        assert_eq!(payload["dag_run_id"], json!(dag_run_id.to_string()));
        assert_eq!(payload["lease_id"], json!(lease_id.to_string()));
        assert_eq!(payload["operator"], json!("cli"));
    }

    #[test]
    fn app_logs_tail_reads_last_requested_lines() {
        let run_id = Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-tail-test-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.log");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();

        assert_eq!(read_log_tail(&path, 2).unwrap(), "two\nthree\n");
        assert_eq!(read_log_tail(&path, 0).unwrap(), "one\ntwo\nthree\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_logs_json_snapshot_advertises_log_contract() {
        let run_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let value = app_log_snapshot_json(
            run_id,
            Path::new(".agenthero/app_runs/11111111-1111-1111-1111-111111111111.log"),
            true,
            Some("line one\n"),
            &[],
        );

        assert_eq!(value["run_id"], json!(run_id));
        assert_eq!(value["exists"], json!(true));
        assert_eq!(value["tail"], json!("line one\n"));
        assert_eq!(
            value["log_contract"]["format"],
            json!("durable_text_log_with_agenthero_event_jsonl")
        );
        assert_eq!(value["log_contract"]["tail_parameter"], json!("tail"));
        assert_eq!(
            value["log_contract"]["max_bytes_parameter"],
            json!("max_bytes")
        );
        assert_eq!(
            value["log_contract"]["trace_fields"][0],
            json!("app_run_id")
        );
    }

    #[test]
    fn app_logs_event_line_surfaces_node_attempt_and_status() {
        let event = AppRunEventSummary {
            id: 42,
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            message: Some("lean_check Ok".to_string()),
            payload: json!({
                "node_id": "lean_check",
                "attempt": 2,
                "status": "ok"
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let line = format_app_run_event_line(&event);

        assert!(line.contains("node.completed"));
        assert!(line.contains("node=lean_check"));
        assert!(line.contains("attempt=2"));
        assert!(line.contains("status=ok"));
    }

    #[test]
    fn app_logs_event_line_surfaces_terminal_provenance_fields() {
        let event = AppRunEventSummary {
            id: 44,
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            message: Some("lean_check ok".to_string()),
            payload: json!({
                "node_id": "lean_check",
                "attempt": 1,
                "node_kind": "lean",
                "tool_id": "lean_check",
                "model": "gpt-5.4",
                "prompt_hash": "sha256:prompt",
                "command": ["lean", "--run", "Proof.lean"],
                "exit_status": 0,
                "duration_ms": 4200,
                "status": "ok"
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let line = format_app_run_event_line(&event);

        assert!(line.contains("kind=lean"));
        assert!(line.contains("tool=lean_check"));
        assert!(line.contains("model=gpt-5.4"));
        assert!(line.contains("prompt_hash=sha256:prompt"));
        assert!(line.contains("command=[\"lean\",\"--run\",\"Proof.lean\"]"));
        assert!(line.contains("exit_status=0"));
        assert!(line.contains("duration_ms=4200"));
    }

    #[test]
    fn app_logs_event_line_surfaces_retry_schedule() {
        let event = AppRunEventSummary {
            id: 43,
            level: "warn".to_string(),
            event_type: "node.retry_scheduled".to_string(),
            message: Some("lean_check retry scheduled".to_string()),
            payload: json!({
                "node_id": "lean_check",
                "attempt": 1,
                "next_attempt": 2,
                "max_attempts": 3,
                "backoff_ms": 250
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let line = format_app_run_event_line(&event);

        assert!(line.contains("node.retry_scheduled"));
        assert!(line.contains("node=lean_check"));
        assert!(line.contains("attempt=1"));
        assert!(line.contains("next_attempt=2"));
        assert!(line.contains("max_attempts=3"));
        assert!(line.contains("backoff_ms=250"));
    }

    #[test]
    fn app_events_snapshot_surfaces_durable_events_without_logs() {
        let run_id = Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap();
        let event = AppRunEventSummary {
            id: 45,
            level: "info".to_string(),
            event_type: "node.started".to_string(),
            message: Some("extract started".to_string()),
            payload: json!({
                "node_id": "extract",
                "attempt": 1,
                "kind": "tool"
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let output = format_app_events_snapshot(run_id, &[event], false);

        assert!(output.contains("durable_events"));
        assert!(output.contains("node.started"));
        assert!(output.contains("node=extract"));
        assert!(output.contains("attempt=1"));
    }

    #[test]
    fn app_events_json_snapshot_is_monitor_friendly() {
        let run_id = Uuid::parse_str("66666666-6666-6666-6666-666666666666").unwrap();
        let event = AppRunEventSummary {
            id: 46,
            level: "warn".to_string(),
            event_type: "node.retry_scheduled".to_string(),
            message: Some("verify retry scheduled".to_string()),
            payload: json!({
                "node_id": "verify",
                "attempt": 1,
                "next_attempt": 2,
                "max_attempts": 3
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let value = app_events_snapshot_json(run_id, &[event]);

        assert_eq!(value["run_id"], "66666666-6666-6666-6666-666666666666");
        assert_eq!(value["events"][0]["event_type"], "node.retry_scheduled");
        assert_eq!(value["events"][0]["node_id"], "verify");
        assert_eq!(value["events"][0]["attempt"], 1);
        assert_eq!(value["events"][0]["payload"]["node_id"], "verify");
        assert_eq!(value["events"][0]["payload"]["next_attempt"], 2);
        assert_eq!(value["event_contract"]["trace_fields"][0], "app_run_id");
        assert!(value["event_contract"]["trace_fields"]
            .as_array()
            .expect("trace fields")
            .iter()
            .any(|field| field == "duration_ms"));
    }

    #[test]
    fn app_events_tail_is_rendered_chronologically() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let newest_first = vec![
            AppRunEventSummary {
                id: 12,
                level: "info".to_string(),
                event_type: "node.completed".to_string(),
                message: Some("verify ok".to_string()),
                payload: json!({"node_id": "verify", "attempt": 1, "status": "ok"}),
                created_at: base + chrono::TimeDelta::seconds(2),
            },
            AppRunEventSummary {
                id: 11,
                level: "info".to_string(),
                event_type: "node.started".to_string(),
                message: Some("verify started".to_string()),
                payload: json!({"node_id": "verify", "attempt": 1}),
                created_at: base + chrono::TimeDelta::seconds(1),
            },
            AppRunEventSummary {
                id: 10,
                level: "info".to_string(),
                event_type: "app_run.started".to_string(),
                message: Some("app run started".to_string()),
                payload: json!({}),
                created_at: base,
            },
        ];

        let events = chronological_event_tail(newest_first);

        assert_eq!(
            events.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
    }

    #[test]
    fn app_status_live_node_line_surfaces_latest_event_state() {
        let line = format_live_node_status_line(&json!({
            "node_id": "formalize",
            "state": "retry_scheduled",
            "attempt": 1,
            "event_type": "node.retry_scheduled",
            "node_kind": "llm",
            "payload": {
                "duration_ms": 1875,
                "exit_status": 0
            },
            "updated_at": "2026-06-24T12:00:00Z"
        }));

        assert!(line.contains("formalize"));
        assert!(line.contains("retry_scheduled"));
        assert!(line.contains("attempt=1"));
        assert!(line.contains("event=node.retry_scheduled"));
        assert!(line.contains("kind=llm"));
        assert!(line.contains("exit_status=0"));
        assert!(line.contains("duration_ms=1875"));
        assert!(line.contains("updated=2026-06-24T12:00:00Z"));
    }

    #[test]
    fn app_logs_snapshot_surfaces_durable_events_when_log_file_is_missing() {
        let run_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let event = AppRunEventSummary {
            id: 44,
            level: "info".to_string(),
            event_type: "node.completed".to_string(),
            message: Some("lean_check Ok".to_string()),
            payload: json!({
                "node_id": "lean_check",
                "attempt": 1,
                "status": "ok"
            }),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let output = format_app_log_snapshot(
            run_id,
            Path::new("/tmp/missing-agenthero-run.log"),
            None,
            &[event],
            false,
        );

        assert!(output.contains("No log file for app run 44444444-4444-4444-4444-444444444444."));
        assert!(output.contains("expected_log /tmp/missing-agenthero-run.log"));
        assert!(output.contains("durable_events"));
        assert!(output.contains("node.completed"));
        assert!(output.contains("node=lean_check"));
        assert!(output.contains("status=ok"));
    }

    #[test]
    fn app_logs_follow_stops_on_terminal_app_run_states() {
        assert!(app_run_state_is_terminal("done"));
        assert!(app_run_state_is_terminal("partial"));
        assert!(app_run_state_is_terminal("awaiting_approval"));
        assert!(app_run_state_is_terminal("failed"));
        assert!(app_run_state_is_terminal("system_failed"));
        assert!(app_run_state_is_terminal("cancelled"));
        assert!(!app_run_state_is_terminal("queued"));
        assert!(!app_run_state_is_terminal("running"));
    }
}
