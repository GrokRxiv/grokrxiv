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
    /// List or inspect installed DAGOps apps.
    Apps {
        /// App registry operation.
        #[command(subcommand)]
        command: AppsCommand,
    },
    /// List, inspect, or run installed DAGOps apps.
    App {
        /// App registry operation.
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Run the HTTP API + Tokio supervisor + scheduler.
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

/// Product app registry operations.
#[derive(Debug, Subcommand)]
pub enum AppsCommand {
    /// List installed DAGOps apps.
    List,
    /// Show one app's available actions.
    Show {
        /// Installed app id.
        app: String,
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
    set_status_enabled(cli.status || (!cli.no_status && std::io::stderr().is_terminal()));
    let json = cli.json;
    let dry_run = cli.dry_run;
    match cli.command {
        Command::Apps { command } => match command {
            AppsCommand::List => app_list(json),
            AppsCommand::Show { app } => app_show(&app, json),
        },
        Command::App { command } => app_command(command, json, dry_run).await,
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

fn set_status_enabled(_enabled: bool) {}

async fn app_command(command: AppCommand, json: bool, dry_run: bool) -> anyhow::Result<()> {
    match command {
        AppCommand::List => app_list(json),
        AppCommand::Show { app } => app_show(&app, json),
        AppCommand::Run { app, args } => {
            if args.is_empty() {
                return app_show(&app, json);
            }
            let resolved = crate::dag_apps::resolve_app_action_args(&app, &args)?;
            app_run_command(&app, &resolved.id, resolved.args, json, dry_run).await
        }
        AppCommand::Runs { app } => app_runs(app.as_deref(), json).await,
        AppCommand::Status { run_id } => app_status(run_id, json).await,
    }
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
        for app in &apps {
            println!("{} - {}", app.slug, app.label);
            for action in &app.actions {
                println!("  {} -> {}", action.command.join(" "), action.dag_type);
            }
        }
    }
    Ok(())
}

fn app_show(app_id: &str, json: bool) -> anyhow::Result<()> {
    let app = crate::dag_apps::load_app_manifest_by_slug(app_id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&app_catalog_json(&app))?);
    } else {
        println!("{} - {}", app.slug, app.label);
        for action in &app.actions {
            println!(
                "{}\n  command={}\n  dag_type={}\n  {}",
                action.id,
                action.command.join(" "),
                action.dag_type,
                action.description
            );
            for option in &action.options {
                let value = option
                    .value_name
                    .as_deref()
                    .map(|name| format!(" <{name}>"))
                    .unwrap_or_default();
                let required = if option.required { " required" } else { "" };
                let repeat = if option.multiple { " repeatable" } else { "" };
                println!(
                    "  option={}{} kind={}{}{} {}",
                    option.name, value, option.kind, required, repeat, option.description
                );
            }
        }
    }
    Ok(())
}

fn app_catalog_json(app: &crate::dag_apps::AppManifest) -> serde_json::Value {
    json!({
        "id": app.slug,
        "label": app.label,
        "description": app.description,
        "actions": app.actions.iter().map(|action| json!({
            "id": action.id,
            "command": action.command,
            "dag_type": action.dag_type,
            "description": action.description,
            "options": action.options,
        })).collect::<Vec<_>>(),
    })
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
    input.values.insert("app".into(), json!(app));
    input.values.insert("action".into(), json!(action));
    input.values.insert("dag_type".into(), json!(binding.dag_type));
    input.values.insert("args".into(), json!(args));
    input.values.insert("dry_run".into(), json!(dry_run));

    let response = crate::dag_apps::run_app_action(app, action, args, input, json).await?;
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

async fn app_runs(app: Option<&str>, json: bool) -> anyhow::Result<()> {
    let pool = connect_db().await?;
    let rows = sqlx::query(
        "select id::text, app_id, action_id, state, created_at, started_at, finished_at \
         from app_runs \
         where ($1::text is null or app_id = $1) \
         order by created_at desc \
         limit 50",
    )
    .bind(app)
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
        println!("{}", serde_json::to_string_pretty(&json!({ "runs": values }))?);
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
        println!("{}", serde_json::to_string_pretty(&json!({ "dags": rows }))?);
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
    let path = dag_manifest_path(dag_type);
    let mut manifest = DagManifest::from_path(&path)
        .with_context(|| format!("load DAG manifest {}", path.display()))?;
    let kind = parse_agent_kind_arg(kind)?;
    if manifest.roles.iter().any(|role| role.id.as_str() == role_id) {
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
    let path = dag_manifest_path(dag_type);
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
        .filter(|node| node.role.as_ref().map(|role| role.as_str() == role_id).unwrap_or(false))
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
    let path = dag_manifest_path(dag_type);
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
    let manifest_path = dag_manifest_path(manifest.id.as_str());
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
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
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
    if let Some(app) = crate::dag_apps::registered_dag_app(dag_type) {
        return app.manifest_path;
    }
    crate::dag_apps::apps_root()
        .join("unknown")
        .join("dags")
        .join(format!("{dag_type}.yaml"))
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
