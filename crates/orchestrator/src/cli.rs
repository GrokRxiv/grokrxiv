//! `agh` CLI surface.

use std::collections::HashSet;
use std::io::{IsTerminal, Read, Seek, SeekFrom};
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
        AppCommand::Logs {
            run_id,
            tail,
            follow,
        } => app_logs(run_id, tail, follow, json).await,
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

async fn app_enqueue(
    app: &str,
    args: Vec<String>,
    json: bool,
    dry_run: bool,
    debug_logs: bool,
) -> anyhow::Result<()> {
    let manifest = crate::dag_apps::load_app_manifest_by_slug(app)?;
    let resolved = crate::dag_apps::resolve_app_action_args_in_manifest(&manifest, &args)?;
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
    level: String,
    event_type: String,
    message: Option<String>,
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
        "select id::text, app_id, action_id, state, input, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
           and ($2::text is null or action_id = $2) \
           and ($3::text is null or state = $3) \
         order by created_at desc \
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
            let input = row.get::<serde_json::Value, _>(4);
            json!({
                "id": row.get::<String, _>(0),
                "app_id": row.get::<String, _>(1),
                "action_id": row.get::<String, _>(2),
                "state": row.get::<String, _>(3),
                "review_id": app_run_review_id(&input),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>(5),
                "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(6),
                "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(7),
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
    let events = load_recent_app_run_events(&pool, run_id).await?;
    let input = row.get::<serde_json::Value, _>(4);
    let state = row.get::<String, _>(3);
    let app_id = row.get::<String, _>(1);
    let action_id = row.get::<String, _>(2);
    let created_at = row.get::<chrono::DateTime<chrono::Utc>, _>(8);
    let started_at = row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(9);
    let finished_at = row.get::<Option<chrono::DateTime<chrono::Utc>>, _>(10);
    let log_path = crate::dag_apps::app_run_log_path(run_id);
    let log_exists = log_path.is_file();
    let now = chrono::Utc::now();
    let queue_summary = if state == "queued" {
        Some(load_app_run_queue_summary(&pool, run_id, &app_id, &action_id, created_at).await?)
    } else {
        None
    };
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
        "output": row.get::<serde_json::Value, _>(5),
        "error_code": row.get::<Option<String>, _>(6),
        "error_message": row.get::<Option<String>, _>(7),
        "created_at": created_at,
        "started_at": started_at,
        "finished_at": finished_at,
        "log_path": log_path.to_string_lossy(),
        "log_exists": log_exists,
        "latest_lease": lease.as_ref().map(app_run_lease_json),
        "queue": queue_summary.as_ref().map(app_run_queue_summary_json),
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

async fn app_logs(run_id: Uuid, tail: usize, follow: bool, json: bool) -> anyhow::Result<()> {
    if json && follow {
        anyhow::bail!("agh app logs --json cannot be combined with --follow");
    }
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
            serde_json::to_string_pretty(&json!({
                "run_id": run_id,
                "log_path": log_path.to_string_lossy(),
                "exists": exists,
                "tail": tail_text.unwrap_or_default(),
            }))?
        );
        return Ok(());
    }

    if let Some(text) = tail_text {
        print!("{text}");
    } else if !follow {
        println!("No log file for app run {run_id}.");
        println!("expected_log {}", log_path.display());
        return Ok(());
    } else {
        eprintln!("Waiting for app run log {}", log_path.display());
    }

    if follow {
        let offset = std::fs::metadata(&log_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        follow_app_run_log(run_id, &log_path, offset).await?;
    }
    Ok(())
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

async fn follow_app_run_log(run_id: Uuid, path: &Path, mut offset: u64) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        offset = print_log_since(path, offset)?;
        if let Some(state) = load_app_run_state(&pool, run_id).await? {
            if app_run_state_is_terminal(&state) {
                let _ = print_log_since(path, offset)?;
                break;
            }
        }
    }
    Ok(())
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
        "done" | "partial" | "failed" | "system_failed" | "cancelled"
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
) -> anyhow::Result<Vec<AppRunEventSummary>> {
    let rows = sqlx::query(
        "select level, event_type, message, created_at \
         from dag_events \
         where app_run_id = $1 \
         order by created_at desc, id desc \
         limit 10",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AppRunEventSummary {
            level: row.get(0),
            event_type: row.get(1),
            message: row.get(2),
            created_at: row.get(3),
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
        "select id::text, app_id, action_id, state, input, created_at, started_at, finished_at \
         from app_runs \
         where app_id = $1 \
           and action_id = $2 \
           and state = 'running' \
         order by started_at asc nulls last, created_at asc \
         limit 5",
    )
    .bind(app_id)
    .bind(action_id)
    .fetch_all(pool)
    .await?;
    let running_same_action = rows
        .into_iter()
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
    sqlx::query(
        "insert into dag_events (app_run_id, level, event_type, message, payload) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind(level)
    .bind(event_type)
    .bind(message)
    .bind(json!({"operator": "cli"}))
    .execute(pool)
    .await?;
    Ok(())
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
        "level": event.level,
        "event_type": event.event_type,
        "message": event.message,
        "created_at": event.created_at,
    })
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
        "{:<14} {:<20} {:<36} {:<8} {:<17} {:<17} {:<17} {}\n",
        "STATE",
        "APP:ACTION",
        "REVIEW_ID",
        "AGE",
        "QUEUED_AT",
        "STARTED_AT",
        "FINISHED_AT",
        "RUN_ID"
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<14} {:<20} {:<36} {:<8} {:<17} {:<17} {:<17} {}\n",
            row.state,
            format!("{}:{}", row.app_id, row.action_id),
            row.review_id.as_deref().unwrap_or("-"),
            format_app_run_age(row.created_at, now),
            format_app_run_ts(Some(row.created_at)),
            format_app_run_ts(row.started_at),
            format_app_run_ts(row.finished_at),
            row.id
        ));
    }
    out
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
        loop_policy: None,
        branch: None,
        map: None,
        approval: None,
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
        loop_policy: None,
        branch: None,
        map: None,
        approval: None,
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
        }];

        let table = format_app_run_table(&rows, now);

        assert!(table.contains("QUEUED_AT"));
        assert!(table.contains("dc06005a-9bc1-4222-8779-10d4c26dd7e2"));
        assert!(table.contains("42m"));
        assert!(table.contains("06-17 12:00:00Z"));
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
    fn app_logs_follow_stops_on_terminal_app_run_states() {
        assert!(app_run_state_is_terminal("done"));
        assert!(app_run_state_is_terminal("partial"));
        assert!(app_run_state_is_terminal("failed"));
        assert!(app_run_state_is_terminal("system_failed"));
        assert!(app_run_state_is_terminal("cancelled"));
        assert!(!app_run_state_is_terminal("queued"));
        assert!(!app_run_state_is_terminal("running"));
    }
}
