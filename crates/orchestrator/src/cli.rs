//! `agh` CLI surface.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

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
    /// Run one installed app action. With no action, prints that app's action catalog.
    Run {
        /// Installed app id.
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
        AppCommand::Runs { app, state, limit } => {
            app_runs(app.as_deref(), state.as_deref(), limit, json).await
        }
        AppCommand::Status { run_id } => app_status(run_id, json).await,
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
    let action_args = trailing[..help_index]
        .iter()
        .filter(|arg| !is_agenthero_control_flag(arg))
        .cloned()
        .collect::<Vec<_>>();
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
        "--json" | "--status" | "--no-status" | "--debug-logs" | "--dry-run" | "--show-secrets"
    )
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
        "actions": app.actions.iter().map(|action| json!({
            "id": action.id,
            "command": action.command,
            "dag_type": action.dag_type,
            "description": action.description,
            "options": action.options,
        })).collect::<Vec<_>>(),
    })
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
    let input = app_run_adapter_input(
        &app.slug,
        action,
        &binding.dag_type,
        stream_app_stderr,
        debug_logs,
    );

    let response = crate::dag_apps::run_app_action_with_manifest(
        app,
        action,
        args,
        input,
        json,
        dry_run,
        stream_app_stderr,
    )
    .await?;
    if !response.ok {
        anyhow::bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| format!("app `{}` action `{action}` failed", app.slug))
        );
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(report) = response.report {
        println!(
            "app={} action={} dag_type={} status={:?} nodes={}",
            app.slug,
            action,
            report.dag_type,
            report.status,
            report.nodes.len()
        );
    } else if let Some(output) = response.output {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("app={} action={} ok", app.slug, action);
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

async fn app_runs(
    app: Option<&str>,
    state: Option<&str>,
    limit: u32,
    json: bool,
) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let limit = limit.clamp(1, 500) as i64;
    let rows = sqlx::query(
        "select id::text, app_id, action_id, state, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
           and ($2::text is null or state = $2) \
         order by created_at desc \
         limit $3",
    )
    .bind(app)
    .bind(state)
    .bind(limit)
    .fetch_all(&pool)
    .await
    .context("list app runs")?;

    let values = rows
        .iter()
        .map(|row| {
            json!({
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
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "runs": values }))?
        );
    } else if values.is_empty() {
        println!("No app runs found.");
    } else {
        for value in values {
            println!(
                "{} {}:{} {}",
                value["id"].as_str().unwrap_or_default(),
                value["app_id"].as_str().unwrap_or_default(),
                value["action_id"].as_str().unwrap_or_default(),
                value["state"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(())
}

async fn app_status(run_id: Uuid, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let row = sqlx::query(
        "select id::text, app_id, action_id, state, input, output, error_code, error_message, \
                created_at, started_at, finished_at \
         from app_runs where id = $1",
    )
    .bind(run_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("app run not found: {run_id}"))?;
    let payload = json!({
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
            ToolExecutorKind::Cli => {
                if tool.command.as_ref().map(Vec::is_empty).unwrap_or(true) {
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
        ToolExecutorKind::Cli => handler,
    };
    manifest.tools.push(DagTool {
        id: tool_id.to_string(),
        executor,
        handler,
        command: (!command.is_empty()).then_some(command),
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

    #[test]
    fn app_run_adapter_input_carries_status_and_debug_flags() {
        let input = app_run_adapter_input("grokrxiv", "review", "review-loop", true, true);

        assert_eq!(input.values["app"], json!("grokrxiv"));
        assert_eq!(input.values["action"], json!("review"));
        assert_eq!(input.values["dag_type"], json!("review-loop"));
        assert_eq!(input.values["stream_stderr"], json!(true));
        assert_eq!(input.values["debug_logs"], json!(true));
    }
}
