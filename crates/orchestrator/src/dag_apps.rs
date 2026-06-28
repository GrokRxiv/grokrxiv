//! Manifest-driven DAGOps app registry and process adapter runner.
//!
//! The orchestrator owns discovery and scheduling. Product behavior lives
//! behind `agenthero/apps/<app>/app.yaml` and its declared adapter process.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agenthero_agent_runtime::{
    AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_EVENT_PREFIX, APP_ADAPTER_PROTOCOL,
};
use agenthero_dag_executor::{DagExecutionEvent, DagExecutionReport, DagIo};
use agenthero_dag_runtime::DagManifest;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

const DEFAULT_ADAPTER_TIMEOUT_SECS: u64 = 60 * 60 * 2;
const DEFAULT_ADAPTER_OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const ADAPTER_CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const ADAPTER_KILL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ADAPTER_STDERR_LINE_CHARS: usize = 4096;
/// `DagIo.values` key used to pass a durable app-run stderr log path to adapters.
pub const APP_RUN_LOG_PATH_INPUT_KEY: &str = "app_run_log_path";
const AGENTHERO_RUNTIME_ROOT_ENV: &str = "AGENTHERO_RUNTIME_ROOT";

type AdapterEventSender = mpsc::Sender<DagExecutionEvent>;
/// Channel used by queued app workers to persist raw adapter stderr lines.
pub type AdapterLogLineSender = mpsc::Sender<String>;

/// Metadata for one DAG type discovered from installed app manifests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagAppDescriptor {
    /// Product app slug that owns this DAG type.
    pub product_app: String,
    /// DAG type id.
    pub dag_type: String,
    /// Manifest path under the product app root.
    pub manifest_path: PathBuf,
}

/// Metadata for one product app command surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredAppDescriptor {
    /// Product app id used by `agh app run <app> ...`.
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Operator-facing description.
    pub description: String,
    /// Actions exposed by this app.
    pub actions: Vec<AppActionDescriptor>,
    /// Deployment targets exposed by this app.
    pub deployments: Vec<AppDeployment>,
    /// App-owned contracts discovered under the app root.
    pub contracts: AppContractsDescriptor,
    /// Audit and monitoring surfaces this app must emit through AgentHero.
    pub observability: AppObservabilityContract,
}

/// App-owned AgentApp contract files discovered under `agenthero/apps/<app>/`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AppContractsDescriptor {
    /// JSON Schema files under `state/`.
    pub state_schemas: Vec<String>,
    /// Optional app-level tool registry contract.
    #[serde(default)]
    pub tools: Option<String>,
    /// YAML policy contracts under `policies/`.
    pub policies: Vec<String>,
    /// YAML eval suite contracts under `evals/`.
    pub evals: Vec<String>,
}

/// Metadata for one app action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppActionDescriptor {
    /// Action id used by `agh app run <app> <action>`.
    pub id: String,
    /// Command path after the app slug, e.g. `["validate", "citations"]`.
    pub command: Vec<String>,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: String,
    /// Short operator-facing description.
    pub description: String,
    /// Action-specific positional and flag metadata.
    pub options: Vec<AppActionOption>,
    /// Scheduler retry policy for this action.
    pub retry: AppActionRetryPolicy,
}

/// YAML product app manifest loaded from `agenthero/apps/<app>/app.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppManifest {
    /// App manifest schema version. Version 1 is the current contract.
    pub version: u32,
    /// Product app slug used by `agh app run <app> ...`.
    pub slug: String,
    /// Human-readable app label.
    pub label: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Process or future runtime adapter for this app.
    pub adapter: AppAdapter,
    /// Required app-level audit and monitoring contract.
    pub observability: AppObservabilityContract,
    /// App actions exposed by this manifest.
    #[serde(default)]
    pub actions: Vec<AppManifestAction>,
    /// App-owned deployable surfaces, such as generated websites.
    #[serde(default)]
    pub deployments: Vec<AppDeployment>,
}

/// App-level observability contract required for every installed DAG app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppObservabilityContract {
    /// App emits durable structured events into `dag_events`.
    pub events: bool,
    /// App emits durable stderr/text logs captured by app-run log files.
    pub logs: bool,
    /// App supports app-run status summaries through persisted runtime state.
    pub status: bool,
    /// App supports tail/follow event streams through AgentHero monitor APIs.
    pub event_stream: bool,
    /// Lifecycle events every adapter must emit or let the scheduler synthesize.
    #[serde(default)]
    pub lifecycle_events: Vec<String>,
    /// Mandatory trace fields expected on emitted event payloads.
    #[serde(default)]
    pub trace_fields: Vec<String>,
}

/// Deployment metadata for an app-owned generated/runtime website.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppDeployment {
    /// Vercel project rooted inside an installed DAGOps app.
    Vercel {
        /// Stable deployment id inside the app.
        id: String,
        /// Vercel project name.
        project: String,
        /// Directory containing the Vercel app, relative to the app root.
        root: String,
        /// Optional framework hint, for example `nextjs` or `static`.
        #[serde(default)]
        framework: Option<String>,
        /// Build command run from `root`.
        #[serde(default)]
        build_command: Option<String>,
        /// Output directory produced by the build, relative to `root`.
        #[serde(default)]
        output_directory: Option<String>,
        /// Environment variables the deployment expects.
        #[serde(default)]
        env: Vec<String>,
    },
}

/// Adapter declaration for an installed DAGOps app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppAdapter {
    /// Spawn a local process and exchange JSON over stdin/stdout.
    Process {
        /// Executable name or absolute path.
        command: String,
        /// Fixed adapter arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Optional local-development fallback executable.
        #[serde(default)]
        fallback_command: Option<String>,
        /// Fixed fallback arguments.
        #[serde(default)]
        fallback_args: Vec<String>,
        /// Optional process timeout override in seconds.
        #[serde(default)]
        timeout_secs: Option<u64>,
        /// Optional stdout/stderr cap override in bytes.
        #[serde(default)]
        output_limit_bytes: Option<usize>,
    },
}

/// YAML action metadata for one product app command path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppManifestAction {
    /// Stable action id.
    pub id: String,
    /// Command path after the app slug, e.g. `["validate", "citations"]`.
    pub command: Vec<String>,
    /// DAG type bound to this action.
    pub dag_type: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Human/LLM-readable argument contract for this action.
    #[serde(default)]
    pub options: Vec<AppActionOption>,
    /// Scheduler retry policy for this action.
    #[serde(default)]
    pub retry: AppActionRetryPolicy,
}

/// Scheduler retry policy declared per app action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppActionRetryPolicy {
    /// Maximum worker attempts before the scheduler stops auto-retrying.
    pub max_attempts: i32,
}

impl Default for AppActionRetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 2 }
    }
}

/// Metadata for one app action positional argument or flag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppActionOption {
    /// Exact positional name or flag token, e.g. `source` or `--source-type`.
    pub name: String,
    /// Option kind, usually `positional` or `flag`.
    pub kind: String,
    /// Optional value placeholder, e.g. `URL_OR_PATH`.
    #[serde(default)]
    pub value_name: Option<String>,
    /// Whether the option is required.
    #[serde(default)]
    pub required: bool,
    /// Whether the option can be repeated.
    #[serde(default)]
    pub multiple: bool,
    /// Other option names that cannot be present at the same time.
    #[serde(default)]
    pub conflicts_with: Vec<String>,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
}

/// Resolved app action binding loaded from the YAML app manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppActionBinding {
    /// Product app slug.
    pub app: String,
    /// Stable action id.
    pub id: String,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: String,
    /// Operator-facing description.
    pub description: String,
}

/// Resolved app action plus action-specific trailing args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAppAction {
    /// Product app slug.
    pub app: String,
    /// Stable action id.
    pub id: String,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: String,
    /// Operator-facing description.
    pub description: String,
    /// Remaining action-specific arguments after the app command path.
    pub args: Vec<String>,
}

/// Return all installed product app descriptors.
pub fn registered_apps() -> anyhow::Result<Vec<RegisteredAppDescriptor>> {
    let mut apps = Vec::new();
    for manifest in load_app_manifests()? {
        let contracts = app_contracts(&manifest.slug)?;
        apps.push(RegisteredAppDescriptor {
            id: manifest.slug,
            label: manifest.label,
            description: manifest.description,
            deployments: manifest.deployments,
            contracts,
            observability: manifest.observability,
            actions: manifest
                .actions
                .into_iter()
                .map(|action| AppActionDescriptor {
                    id: action.id,
                    command: action.command,
                    dag_type: action.dag_type,
                    description: action.description,
                    options: action.options,
                    retry: action.retry,
                })
                .collect(),
        });
    }
    Ok(apps)
}

/// Return all installed product app ids in deterministic order.
pub fn registered_app_ids() -> anyhow::Result<Vec<String>> {
    Ok(registered_apps()?.into_iter().map(|app| app.id).collect())
}

/// Find one installed product app descriptor.
pub fn registered_app(app_id: &str) -> anyhow::Result<Option<RegisteredAppDescriptor>> {
    Ok(registered_apps()?.into_iter().find(|app| app.id == app_id))
}

/// Find one installed product app action.
pub fn registered_app_action(
    app_id: &str,
    action_id: &str,
) -> anyhow::Result<Option<AppActionDescriptor>> {
    Ok(registered_app(app_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown app `{app_id}`"))?
        .actions
        .into_iter()
        .find(|action| action.id == action_id))
}

/// Resolve one product app action from the YAML app manifest.
pub fn app_action_binding(app_id: &str, action_id: &str) -> anyhow::Result<AppActionBinding> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{app_id} {action_id}`"))?;
    Ok(AppActionBinding {
        app: manifest.slug,
        id: action.id.clone(),
        dag_type: action.dag_type.clone(),
        description: action.description.clone(),
    })
}

/// Return the scheduler retry policy for one app action.
pub fn app_action_retry_policy(
    app_id: &str,
    action_id: &str,
) -> anyhow::Result<AppActionRetryPolicy> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{app_id} {action_id}`"))?;
    Ok(action.retry)
}

/// Resolve raw app-run args against the app manifest command paths.
pub fn resolve_app_action_args(app_id: &str, args: &[String]) -> anyhow::Result<ResolvedAppAction> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    resolve_app_action_args_in_manifest(&manifest, args)
}

/// Resolve raw app-run args against an already loaded app manifest.
pub fn resolve_app_action_args_in_manifest(
    manifest: &AppManifest,
    args: &[String],
) -> anyhow::Result<ResolvedAppAction> {
    let (action, consumed_args) = manifest
        .actions
        .iter()
        .filter_map(|action| action_arg_match_len(action, args).map(|len| (action, len)))
        .max_by_key(|(_, len)| *len)
        .ok_or_else(|| {
            let requested = args.first().map(String::as_str).unwrap_or("<none>");
            anyhow::anyhow!("unknown app action `{} {requested}`", manifest.slug)
        })?;
    Ok(ResolvedAppAction {
        app: manifest.slug.clone(),
        id: action.id.clone(),
        dag_type: action.dag_type.clone(),
        description: action.description.clone(),
        args: args[consumed_args..].to_vec(),
    })
}

fn action_arg_match_len(action: &AppManifestAction, args: &[String]) -> Option<usize> {
    if command_path_matches(&action.command, args) {
        return Some(action.command.len());
    }
    if args.first().is_some_and(|arg| arg == &action.id) {
        return Some(1);
    }
    None
}

fn command_path_matches(command: &[String], args: &[String]) -> bool {
    args.len() >= command.len()
        && command
            .iter()
            .zip(args.iter())
            .all(|(expected, actual)| expected == actual)
}

/// Load all YAML app manifests in deterministic slug order.
pub fn load_app_manifests() -> anyhow::Result<Vec<AppManifest>> {
    let mut manifests = Vec::new();
    let root = apps_root();
    for entry in std::fs::read_dir(&root)
        .map_err(|err| anyhow::anyhow!("read app manifests root {}: {err}", root.display()))?
    {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("app.yaml");
        if manifest_path.is_file() {
            manifests.push(load_app_manifest(&manifest_path)?);
        }
    }
    manifests.sort_by(|a, b| a.slug.cmp(&b.slug));
    validate_unique_dag_types(&manifests)?;
    Ok(manifests)
}

/// Load one product app manifest by slug.
pub fn load_app_manifest_by_slug(slug: &str) -> anyhow::Result<AppManifest> {
    let path = app_root(slug).join("app.yaml");
    load_app_manifest(&path)
}

fn load_app_manifest(path: &Path) -> anyhow::Result<AppManifest> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| anyhow::anyhow!("read app manifest {}: {err}", path.display()))?;
    let manifest: AppManifest = serde_yaml::from_str(&text)
        .map_err(|err| anyhow::anyhow!("parse app manifest {}: {err}", path.display()))?;
    let app_root = path.parent().unwrap_or_else(|| Path::new("."));
    validate_app_manifest(&manifest, app_root)
        .map_err(|err| anyhow::anyhow!("validate app manifest {}: {err}", path.display()))?;
    validate_app_contracts(app_root)
        .map_err(|err| anyhow::anyhow!("validate app contracts {}: {err}", app_root.display()))?;
    Ok(manifest)
}

/// Return app-owned AgentApp contract files discovered under one app root.
pub fn app_contracts(app_id: &str) -> anyhow::Result<AppContractsDescriptor> {
    app_contracts_for_root(&app_root(app_id))
}

fn app_contracts_for_root(app_root: &Path) -> anyhow::Result<AppContractsDescriptor> {
    Ok(AppContractsDescriptor {
        state_schemas: collect_contract_files(app_root, "state", "schema.json")?,
        tools: app_root
            .join("tools.yaml")
            .is_file()
            .then(|| "tools.yaml".to_string()),
        policies: collect_contract_files(app_root, "policies", "yaml")?,
        evals: collect_contract_files(app_root, "evals", "yaml")?,
    })
}

fn collect_contract_files(
    app_root: &Path,
    directory: &str,
    extension_suffix: &str,
) -> anyhow::Result<Vec<String>> {
    let dir = app_root.join(directory);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|err| anyhow::anyhow!("read contract dir {}: {err}", dir.display()))?
    {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(extension_suffix) {
            files.push(format!("{directory}/{name}"));
        }
    }
    files.sort();
    Ok(files)
}

fn validate_app_contracts(app_root: &Path) -> anyhow::Result<()> {
    validate_state_contracts(app_root)?;
    validate_tools_contract(app_root)?;
    validate_yaml_contract_dir(app_root, "policies")?;
    validate_yaml_contract_dir(app_root, "evals")?;
    Ok(())
}

fn validate_state_contracts(app_root: &Path) -> anyhow::Result<()> {
    for rel in collect_contract_files(app_root, "state", "schema.json")? {
        let path = app_root.join(&rel);
        let text = std::fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|err| anyhow::anyhow!("parse {} as JSON schema: {err}", path.display()))?;
        if !parsed.is_object() {
            anyhow::bail!("state schema {} must be a JSON object", path.display());
        }
    }
    Ok(())
}

fn validate_tools_contract(app_root: &Path) -> anyhow::Result<()> {
    let path = app_root.join("tools.yaml");
    if !path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
    let parsed: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|err| anyhow::anyhow!("parse {}: {err}", path.display()))?;
    let tools = parsed
        .get("tools")
        .and_then(serde_yaml::Value::as_sequence)
        .ok_or_else(|| anyhow::anyhow!("tools.yaml must declare a `tools` list"))?;
    let allowed = ["read", "write", "external", "network", "none"];
    let mut ids = BTreeSet::new();
    for tool in tools {
        let id = tool
            .get("id")
            .and_then(serde_yaml::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("tools.yaml tool id is required"))?;
        if id.trim().is_empty() {
            anyhow::bail!("tools.yaml tool id is required");
        }
        if !ids.insert(id.to_string()) {
            anyhow::bail!("duplicate tools.yaml tool id `{id}`");
        }
        if let Some(permissions) = tool.get("permissions") {
            let permissions = permissions.as_sequence().ok_or_else(|| {
                anyhow::anyhow!("tools.yaml tool `{id}` permissions must be a list")
            })?;
            for permission in permissions {
                let permission = permission.as_str().ok_or_else(|| {
                    anyhow::anyhow!("tools.yaml tool `{id}` permission must be a string")
                })?;
                if !allowed.contains(&permission) {
                    anyhow::bail!(
                        "unknown tool permission `{permission}` in tools.yaml tool `{id}`"
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_yaml_contract_dir(app_root: &Path, directory: &str) -> anyhow::Result<()> {
    for rel in collect_contract_files(app_root, directory, "yaml")? {
        let path = app_root.join(&rel);
        let text = std::fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("read {}: {err}", path.display()))?;
        let parsed: serde_yaml::Value = serde_yaml::from_str(&text)
            .map_err(|err| anyhow::anyhow!("parse {}: {err}", path.display()))?;
        if !parsed.is_mapping() {
            anyhow::bail!("{} must be a YAML mapping", path.display());
        }
    }
    Ok(())
}

fn validate_app_manifest(manifest: &AppManifest, app_root: &Path) -> anyhow::Result<()> {
    if manifest.version != 1 {
        anyhow::bail!(
            "unsupported app manifest version `{}`; expected `1`",
            manifest.version
        );
    }
    if manifest.slug.trim().is_empty() {
        anyhow::bail!("slug is required");
    }
    match &manifest.adapter {
        AppAdapter::Process { command, .. } if command.trim().is_empty() => {
            anyhow::bail!("process adapter command is required");
        }
        AppAdapter::Process { .. } => {}
    }
    validate_app_observability(manifest)?;
    let mut ids = BTreeSet::new();
    let mut commands = BTreeSet::new();
    for action in &manifest.actions {
        if action.id.trim().is_empty() {
            anyhow::bail!("action id is required");
        }
        if !ids.insert(action.id.as_str()) {
            anyhow::bail!("duplicate action id `{}`", action.id);
        }
        if action.command.is_empty() || action.command.iter().any(|part| part.trim().is_empty()) {
            anyhow::bail!("action `{}` command path is required", action.id);
        }
        if !commands.insert(action.command.join(" ")) {
            anyhow::bail!("duplicate command path for action `{}`", action.id);
        }
        if action.dag_type.trim().is_empty() {
            anyhow::bail!("action `{}` dag_type is required", action.id);
        }
        if action.retry.max_attempts < 1 {
            anyhow::bail!(
                "action `{}` retry.max_attempts must be greater than zero",
                action.id
            );
        }
        let dag_path = app_root
            .join("dags")
            .join(format!("{}.yaml", action.dag_type));
        if !dag_path.is_file() {
            anyhow::bail!(
                "action `{}` references missing DAG manifest {}",
                action.id,
                dag_path.display()
            );
        }
        let dag_manifest = DagManifest::from_path(&dag_path)
            .map_err(|err| anyhow::anyhow!("load DAG manifest {}: {err}", dag_path.display()))?;
        if dag_manifest.id.as_str() != action.dag_type {
            anyhow::bail!(
                "action `{}` dag_type `{}` does not match DAG manifest id `{}` in {}",
                action.id,
                action.dag_type,
                dag_manifest.id,
                dag_path.display()
            );
        }
        let mut option_names = BTreeSet::new();
        for option in &action.options {
            if option.name.trim().is_empty() {
                anyhow::bail!("action `{}` option name is required", action.id);
            }
            if option.kind.trim().is_empty() {
                anyhow::bail!(
                    "action `{}` option `{}` kind is required",
                    action.id,
                    option.name
                );
            }
            if !option_names.insert(option.name.as_str()) {
                anyhow::bail!(
                    "duplicate option `{}` for action `{}`",
                    option.name,
                    action.id
                );
            }
        }
        for option in &action.options {
            for conflict in &option.conflicts_with {
                if conflict.trim().is_empty() {
                    anyhow::bail!(
                        "action `{}` option `{}` conflicts_with entries must be non-empty",
                        action.id,
                        option.name
                    );
                }
                if conflict == &option.name {
                    anyhow::bail!(
                        "action `{}` option `{}` cannot conflict with itself",
                        action.id,
                        option.name
                    );
                }
                if !option_names.contains(conflict.as_str()) {
                    anyhow::bail!(
                        "action `{}` option `{}` conflicts with unknown option `{conflict}`",
                        action.id,
                        option.name
                    );
                }
            }
        }
    }
    for deployment in &manifest.deployments {
        match deployment {
            AppDeployment::Vercel {
                id,
                project,
                root,
                env,
                ..
            } => {
                if id.trim().is_empty() {
                    anyhow::bail!("vercel deployment id is required");
                }
                if project.trim().is_empty() {
                    anyhow::bail!("vercel deployment `{id}` project is required");
                }
                if root.trim().is_empty() {
                    anyhow::bail!("vercel deployment `{id}` root is required");
                }
                if env.iter().any(|name| name.trim().is_empty()) {
                    anyhow::bail!("vercel deployment `{id}` env names must be non-empty");
                }
            }
        }
    }
    Ok(())
}

/// Validate action-specific argv against generic option contracts in app.yaml.
pub fn validate_app_action_args(action: &AppManifestAction, args: &[String]) -> anyhow::Result<()> {
    let parsed = parse_app_action_args(action, args);

    let required_positionals = action
        .options
        .iter()
        .filter(|option| option.kind == "positional" && option.required)
        .count();
    if parsed.positionals.len() < required_positionals {
        let names = action
            .options
            .iter()
            .filter(|option| option.kind == "positional" && option.required)
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "app action `{}` requires positional argument{} `{}`",
            action.id,
            if required_positionals == 1 { "" } else { "s" },
            names
        );
    }

    for option in &action.options {
        if option.kind == "positional" {
            continue;
        }
        if option.required && !parsed.present_flags.contains(&option.name) {
            anyhow::bail!(
                "app action `{}` requires option `{}`",
                action.id,
                option.name
            );
        }
        if parsed.missing_value_flags.contains(&option.name) {
            let placeholder = option.value_name.as_deref().unwrap_or("VALUE");
            anyhow::bail!(
                "app action `{}` option `{}` requires value `{placeholder}`",
                action.id,
                option.name
            );
        }
        if !parsed.present_flags.contains(&option.name) {
            continue;
        }
        for conflict in &option.conflicts_with {
            if parsed.present_flags.contains(conflict) {
                anyhow::bail!(
                    "app action `{}` option `{}` cannot be combined with `{conflict}`",
                    action.id,
                    option.name
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ParsedAppActionArgs {
    present_flags: BTreeSet<String>,
    missing_value_flags: BTreeSet<String>,
    positionals: Vec<String>,
}

fn parse_app_action_args(action: &AppManifestAction, args: &[String]) -> ParsedAppActionArgs {
    let declared = action
        .options
        .iter()
        .filter(|option| option.kind != "positional")
        .map(|option| (option.name.as_str(), option))
        .collect::<BTreeMap<_, _>>();

    let mut parsed = ParsedAppActionArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let Some(flag) = arg.strip_prefix("--") else {
            parsed.positionals.push(arg.clone());
            index += 1;
            continue;
        };
        let (flag, inline_value) = flag
            .split_once('=')
            .map_or((flag, None), |(flag, value)| (flag, Some(value)));
        let flag = format!("--{flag}");
        if let Some(option) = declared.get(flag.as_str()) {
            parsed.present_flags.insert(option.name.clone());
            if option.value_name.is_some() {
                let has_value = match inline_value {
                    Some(value) => !value.is_empty(),
                    None => args
                        .get(index + 1)
                        .is_some_and(|value| !value.starts_with("--")),
                };
                if has_value && inline_value.is_none() {
                    index += 1;
                } else if !has_value {
                    parsed.missing_value_flags.insert(option.name.clone());
                }
            }
        }
        index += 1;
    }
    parsed
}

fn validate_app_observability(manifest: &AppManifest) -> anyhow::Result<()> {
    let observability = &manifest.observability;
    for (field, enabled) in [
        ("events", observability.events),
        ("logs", observability.logs),
        ("status", observability.status),
        ("event_stream", observability.event_stream),
    ] {
        if !enabled {
            anyhow::bail!(
                "app `{}` observability.{field} must be true so app runs are auditable",
                manifest.slug
            );
        }
    }

    for event_type in [
        "app_action.started",
        "app_action.completed",
        "app_action.failed",
    ] {
        if !observability
            .lifecycle_events
            .iter()
            .any(|declared| declared == event_type)
        {
            anyhow::bail!(
                "app `{}` observability.lifecycle_events must include `{event_type}`",
                manifest.slug
            );
        }
    }

    for field in agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS {
        if !observability
            .trace_fields
            .iter()
            .any(|declared| declared == field)
        {
            anyhow::bail!(
                "app `{}` observability.trace_fields must include `{field}`",
                manifest.slug
            );
        }
    }
    Ok(())
}

fn validate_unique_dag_types(manifests: &[AppManifest]) -> anyhow::Result<()> {
    let mut owners = BTreeMap::<&str, &str>::new();
    for manifest in manifests {
        for action in &manifest.actions {
            if let Some(owner) = owners.insert(action.dag_type.as_str(), manifest.slug.as_str()) {
                if owner != manifest.slug {
                    anyhow::bail!(
                        "dag_type `{}` is declared by both app `{owner}` and app `{}`",
                        action.dag_type,
                        manifest.slug
                    );
                }
            }
        }
    }
    Ok(())
}

/// Return all discovered DAG type descriptors.
pub fn registered_dag_apps() -> anyhow::Result<Vec<DagAppDescriptor>> {
    let mut dag_to_app: BTreeMap<String, String> = BTreeMap::new();
    for manifest in load_app_manifests()? {
        for action in manifest.actions {
            dag_to_app
                .entry(action.dag_type)
                .or_insert_with(|| manifest.slug.clone());
        }
    }

    Ok(dag_to_app
        .into_iter()
        .map(|(dag_type, product_app)| DagAppDescriptor {
            manifest_path: app_root(&product_app)
                .join("dags")
                .join(format!("{dag_type}.yaml")),
            product_app,
            dag_type,
        })
        .collect())
}

/// Return all discovered DAG type ids in deterministic order.
pub fn registered_dag_app_ids() -> anyhow::Result<Vec<String>> {
    Ok(registered_dag_apps()?
        .into_iter()
        .map(|app| app.dag_type)
        .collect())
}

/// Find one discovered DAG type descriptor.
pub fn registered_dag_app(dag_type: &str) -> anyhow::Result<Option<DagAppDescriptor>> {
    Ok(registered_dag_apps()?
        .into_iter()
        .find(|app| app.dag_type == dag_type))
}

/// Run an action through its app's declared adapter process.
pub async fn run_app_action(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<AppAdapterResponse> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    run_app_action_with_manifest(&manifest, action_id, args, input, json, dry_run, false).await
}

/// Run an action with a caller-supplied idempotency key for durable retries.
pub async fn run_app_action_with_idempotency_key(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    idempotency_key: String,
) -> anyhow::Result<AppAdapterResponse> {
    run_app_action_with_idempotency_key_and_checkpoint(
        app_id,
        action_id,
        args,
        input,
        json,
        dry_run,
        idempotency_key,
        None,
    )
    .await
}

/// Run an action with durable retry identity and an optional replay checkpoint.
pub async fn run_app_action_with_idempotency_key_and_checkpoint(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    idempotency_key: String,
    checkpoint: Option<DagExecutionReport>,
) -> anyhow::Result<AppAdapterResponse> {
    run_app_action_with_idempotency_key_checkpoint_and_events(
        app_id,
        action_id,
        args,
        input,
        json,
        dry_run,
        idempotency_key,
        checkpoint,
        None,
        None,
    )
    .await
}

/// Run an action with durable retry identity, optional replay checkpoint, and
/// optional live adapter event capture.
pub async fn run_app_action_with_idempotency_key_checkpoint_and_events(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    idempotency_key: String,
    checkpoint: Option<DagExecutionReport>,
    adapter_events: Option<AdapterEventSender>,
    adapter_log_lines: Option<AdapterLogLineSender>,
) -> anyhow::Result<AppAdapterResponse> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let stream_stderr = app_action_stream_stderr_requested(&input);
    run_app_action_with_manifest_and_key(
        &manifest,
        action_id,
        args,
        input,
        json,
        dry_run,
        stream_stderr,
        Some(idempotency_key),
        checkpoint,
        adapter_events,
        adapter_log_lines,
        None,
    )
    .await
}

/// Run an action with durable retry identity, optional replay checkpoint,
/// optional live adapter event capture, and cooperative cancellation.
pub async fn run_app_action_with_idempotency_key_checkpoint_events_and_cancellation(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    idempotency_key: String,
    checkpoint: Option<DagExecutionReport>,
    adapter_events: Option<AdapterEventSender>,
    adapter_log_lines: Option<AdapterLogLineSender>,
    cancellation: Option<watch::Receiver<bool>>,
) -> anyhow::Result<AppAdapterResponse> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let stream_stderr = app_action_stream_stderr_requested(&input);
    run_app_action_with_manifest_and_key(
        &manifest,
        action_id,
        args,
        input,
        json,
        dry_run,
        stream_stderr,
        Some(idempotency_key),
        checkpoint,
        adapter_events,
        adapter_log_lines,
        cancellation,
    )
    .await
}

/// Run an action through an already loaded app manifest.
pub async fn run_app_action_with_manifest(
    manifest: &AppManifest,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    stream_stderr: bool,
) -> anyhow::Result<AppAdapterResponse> {
    run_app_action_with_manifest_and_key(
        manifest,
        action_id,
        args,
        input,
        json,
        dry_run,
        stream_stderr,
        None,
        None,
        None,
        None,
        None,
    )
    .await
}

async fn run_app_action_with_manifest_and_key(
    manifest: &AppManifest,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
    stream_stderr: bool,
    idempotency_key: Option<String>,
    checkpoint: Option<DagExecutionReport>,
    adapter_events: Option<AdapterEventSender>,
    adapter_log_lines: Option<AdapterLogLineSender>,
    cancellation: Option<watch::Receiver<bool>>,
) -> anyhow::Result<AppAdapterResponse> {
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{} {action_id}`", manifest.slug))?;
    validate_app_action_args(action, &args)?;
    let request = AppAdapterRequest::new(
        manifest.slug.clone(),
        action.id.clone(),
        action.dag_type.clone(),
        args,
        input,
        json,
        dry_run,
    );
    let request = match idempotency_key {
        Some(key) => request.with_idempotency_key(key),
        None => request,
    };
    let request = match checkpoint {
        Some(checkpoint) => request.with_checkpoint(checkpoint),
        None => request,
    };
    run_adapter_process(
        &manifest,
        &request,
        stream_stderr,
        adapter_events,
        adapter_log_lines,
        cancellation,
    )
    .await
}

/// Run a discovered DAG type through its owning app adapter.
pub async fn run_registered_dag_app(
    dag_type: &str,
    input: DagIo,
) -> anyhow::Result<DagExecutionReport> {
    let (manifest, action) = find_action_for_dag(dag_type)?;
    let request = AppAdapterRequest::new(
        manifest.slug.clone(),
        action.id.clone(),
        action.dag_type.clone(),
        Vec::new(),
        input,
        true,
        false,
    );
    let response = run_adapter_process(&manifest, &request, false, None, None, None).await?;
    if !response.ok {
        anyhow::bail!(
            "{}",
            response.error.unwrap_or_else(|| format!(
                "app `{}` action `{}` failed",
                manifest.slug, action.id
            ))
        );
    }
    response.report.ok_or_else(|| {
        anyhow::anyhow!(
            "app `{}` action `{}` did not return a DAG report",
            manifest.slug,
            action.id
        )
    })
}

fn find_action_for_dag(dag_type: &str) -> anyhow::Result<(AppManifest, AppManifestAction)> {
    for manifest in load_app_manifests()? {
        if let Some(action) = manifest
            .actions
            .iter()
            .find(|action| action.dag_type == dag_type)
            .cloned()
        {
            return Ok((manifest, action));
        }
    }
    anyhow::bail!("unknown DAG app `{dag_type}`")
}

async fn run_adapter_process(
    manifest: &AppManifest,
    request: &AppAdapterRequest,
    stream_stderr: bool,
    adapter_events: Option<AdapterEventSender>,
    adapter_log_lines: Option<AdapterLogLineSender>,
    cancellation: Option<watch::Receiver<bool>>,
) -> anyhow::Result<AppAdapterResponse> {
    let AppAdapter::Process {
        command,
        args,
        fallback_command,
        fallback_args,
        timeout_secs,
        output_limit_bytes,
    } = &manifest.adapter;
    let timeout = adapter_timeout_duration(*timeout_secs);
    let output_limit = output_limit_bytes
        .or_else(adapter_output_limit_from_env)
        .unwrap_or(DEFAULT_ADAPTER_OUTPUT_LIMIT_BYTES);
    let workdir = adapter_workdir();

    let prefer_fallback = cfg!(debug_assertions)
        && fallback_command.is_some()
        && std::env::var_os("AGENTHERO_ADAPTER_BIN_DIR").is_none()
        && adapter_fallback_allowed();

    let mut child = if prefer_fallback {
        let fallback = fallback_command.as_deref().expect("checked fallback");
        spawn_adapter(fallback, fallback_args, &workdir)
            .await
            .map_err(|err| {
                anyhow::anyhow!(
                    "spawn app `{}` fallback adapter `{fallback}`: {err}",
                    manifest.slug
                )
            })?
    } else {
        match spawn_adapter(command, args, &workdir).await {
            Ok(child) => child,
            Err(err)
                if err
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == ErrorKind::NotFound)
                    && fallback_command.is_some()
                    && adapter_fallback_allowed() =>
            {
                let fallback = fallback_command.as_deref().expect("checked fallback");
                spawn_adapter(fallback, fallback_args, &workdir).await.map_err(|fallback_err| {
                anyhow::anyhow!(
                    "spawn app `{}` adapter `{command}` failed and fallback `{fallback}` failed: {fallback_err}",
                    manifest.slug
                )
            })?
            }
            Err(err) => {
                return Err(anyhow::anyhow!(
                    "spawn app `{}` adapter `{command}`: {err}",
                    manifest.slug
                ));
            }
        }
    };

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("adapter `{}` stdout unavailable", manifest.slug))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("adapter `{}` stderr unavailable", manifest.slug))?;
    let stdout_task = tokio::spawn(async move { read_limited(&mut stdout, output_limit).await });
    let stderr_log_path = app_action_log_path_requested(&request.input);
    let stderr_task = tokio::spawn(async move {
        if stream_stderr
            || stderr_log_path.is_some()
            || adapter_events.is_some()
            || adapter_log_lines.is_some()
        {
            read_limited_and_tee_stderr(
                &mut stderr,
                output_limit,
                stream_stderr,
                stderr_log_path,
                adapter_events,
                adapter_log_lines,
            )
            .await
        } else {
            read_limited(&mut stderr, output_limit).await
        }
    });

    let payload = serde_json::to_vec(request)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("adapter `{}` stdin unavailable", manifest.slug))?;
    stdin.write_all(&payload).await?;
    stdin.shutdown().await?;
    drop(stdin);

    let wait_outcome = if let Some(timeout) = timeout {
        tokio::select! {
            result = child.wait() => AdapterWaitOutcome::Exited(result),
            _ = tokio::time::sleep(timeout) => AdapterWaitOutcome::TimedOut,
            _ = wait_for_adapter_cancellation(cancellation) => AdapterWaitOutcome::Cancelled,
        }
    } else {
        tokio::select! {
            result = child.wait() => AdapterWaitOutcome::Exited(result),
            _ = wait_for_adapter_cancellation(cancellation) => AdapterWaitOutcome::Cancelled,
        }
    };
    let (status, wait_error) = match wait_outcome {
        AdapterWaitOutcome::Exited(result) => {
            let status = result.map_err(|err| {
                anyhow::anyhow!("wait for app `{}` adapter: {err}", manifest.slug)
            })?;
            (Some(status), None)
        }
        AdapterWaitOutcome::TimedOut => {
            kill_adapter_child(&mut child, manifest, "timeout").await?;
            (
                None,
                Some(anyhow::anyhow!(
                    "app `{}` adapter timed out after {}s",
                    manifest.slug,
                    timeout
                        .expect("timed out outcome requires a configured adapter timeout")
                        .as_secs()
                )),
            )
        }
        AdapterWaitOutcome::Cancelled => {
            kill_adapter_child(&mut child, manifest, "cancellation").await?;
            (
                None,
                Some(anyhow::anyhow!(
                    "app `{}` adapter cancelled by AgentHero",
                    manifest.slug
                )),
            )
        }
    };
    let cleanup_after_cancel = wait_error.is_some();
    let stdout =
        join_adapter_output_task(manifest, "stdout", stdout_task, cleanup_after_cancel).await?;
    let stderr =
        join_adapter_output_task(manifest, "stderr", stderr_task, cleanup_after_cancel).await?;
    if stdout.truncated {
        anyhow::bail!(
            "app `{}` adapter stdout exceeded {} bytes",
            manifest.slug,
            output_limit
        );
    }
    if stderr.truncated {
        anyhow::bail!(
            "app `{}` adapter stderr exceeded {} bytes",
            manifest.slug,
            output_limit
        );
    }
    let stdout = stdout.bytes;
    let stderr = stderr.bytes;

    if let Some(err) = wait_error {
        return Err(err);
    }
    let status = status.expect("adapter status exists when no wait error");
    if !status.success() {
        if let Ok(response) = parse_adapter_response(manifest, request, &stdout) {
            if !response.ok {
                return Ok(response);
            }
        }
        let stderr = String::from_utf8_lossy(&stderr);
        anyhow::bail!(
            "app `{}` adapter exited with {}: {}",
            manifest.slug,
            status,
            truncate_for_error(stderr.trim(), 2048)
        );
    }

    parse_adapter_response(manifest, request, &stdout)
}

enum AdapterWaitOutcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

async fn wait_for_adapter_cancellation(cancellation: Option<watch::Receiver<bool>>) {
    let Some(mut cancellation) = cancellation else {
        std::future::pending::<()>().await;
        return;
    };
    if *cancellation.borrow() {
        return;
    }
    while cancellation.changed().await.is_ok() {
        if *cancellation.borrow() {
            return;
        }
    }
    std::future::pending::<()>().await;
}

async fn kill_adapter_child(
    child: &mut tokio::process::Child,
    manifest: &AppManifest,
    reason: &str,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        kill_adapter_process_group(pid, manifest, reason)?;
        match tokio::time::timeout(ADAPTER_KILL_WAIT_TIMEOUT, child.wait()).await {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(err)) => {
                tracing::warn!(
                    app = %manifest.slug,
                    reason,
                    error = %err,
                    "wait for killed adapter process group failed; falling back to direct child kill"
                );
            }
            Err(_) => {
                tracing::warn!(
                    app = %manifest.slug,
                    reason,
                    "timed out waiting for killed adapter process group; falling back to direct child kill"
                );
            }
        }
    }

    child.kill().await.map_err(|err| {
        anyhow::anyhow!("kill app `{}` adapter after {reason}: {err}", manifest.slug)
    })
}

#[cfg(unix)]
fn kill_adapter_process_group(
    pid: u32,
    manifest: &AppManifest,
    reason: &str,
) -> anyhow::Result<()> {
    let pgid = nix::unistd::Pid::from_raw(pid as i32);
    match nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(err) => Err(anyhow::anyhow!(
            "kill app `{}` adapter process group after {reason}: {err}",
            manifest.slug
        )),
    }
}

async fn join_adapter_output_task(
    manifest: &AppManifest,
    stream: &'static str,
    task: tokio::task::JoinHandle<anyhow::Result<LimitedOutput>>,
    cleanup_after_cancel: bool,
) -> anyhow::Result<LimitedOutput> {
    if !cleanup_after_cancel {
        return adapter_output_join_result(manifest, stream, task.await);
    }

    let mut task = task;
    tokio::select! {
        result = &mut task => adapter_output_join_result(manifest, stream, result),
        _ = tokio::time::sleep(ADAPTER_CANCEL_DRAIN_TIMEOUT) => {
            task.abort();
            let _ = task.await;
            tracing::warn!(
                app = %manifest.slug,
                stream,
                "aborted adapter output reader after cancellation drain timeout"
            );
            Ok(LimitedOutput {
                bytes: Vec::new(),
                truncated: false,
            })
        }
    }
}

fn adapter_output_join_result(
    manifest: &AppManifest,
    stream: &'static str,
    result: Result<anyhow::Result<LimitedOutput>, tokio::task::JoinError>,
) -> anyhow::Result<LimitedOutput> {
    result.map_err(|err| anyhow::anyhow!("join app `{}` {stream} task: {err}", manifest.slug))?
}

fn parse_adapter_response(
    manifest: &AppManifest,
    request: &AppAdapterRequest,
    stdout: &[u8],
) -> anyhow::Result<AppAdapterResponse> {
    let response: AppAdapterResponse = serde_json::from_slice(stdout).map_err(|err| {
        let stdout = String::from_utf8_lossy(&stdout);
        anyhow::anyhow!(
            "parse app `{}` adapter response as JSON: {err}; stdout={}",
            manifest.slug,
            truncate_for_error(stdout.trim(), 2048)
        )
    })?;
    validate_adapter_response(request, &response)?;
    Ok(response)
}

async fn spawn_adapter(
    command: &str,
    args: &[String],
    workdir: &Path,
) -> anyhow::Result<tokio::process::Child> {
    let command_path = resolve_process_command(command, "AGENTHERO_ADAPTER_BIN_DIR");
    let mut command = tokio::process::Command::new(command_path);
    command
        .args(args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    configure_adapter_process_group(&mut command);
    command.spawn().map_err(Into::into)
}

#[cfg(unix)]
fn configure_adapter_process_group(command: &mut tokio::process::Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_adapter_process_group(_command: &mut tokio::process::Command) {}

async fn read_limited<R>(reader: &mut R, limit: usize) -> anyhow::Result<LimitedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buf).await?;
        if read == 0 {
            return Ok(LimitedOutput {
                bytes: out,
                truncated,
            });
        }
        let remaining = limit.saturating_sub(out.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        if read > remaining {
            out.extend_from_slice(&buf[..remaining]);
            truncated = true;
        } else {
            out.extend_from_slice(&buf[..read]);
        }
    }
}

async fn read_limited_and_tee_stderr<R>(
    reader: &mut R,
    limit: usize,
    stream_stderr: bool,
    log_path: Option<PathBuf>,
    adapter_events: Option<AdapterEventSender>,
    adapter_log_lines: Option<AdapterLogLineSender>,
) -> anyhow::Result<LimitedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut log_file = if let Some(path) = log_path {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Some(
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await?,
        )
    } else {
        None
    };
    if adapter_events.is_some() || adapter_log_lines.is_some() {
        return read_limited_and_filter_adapter_events(
            reader,
            limit,
            stream_stderr,
            log_file,
            adapter_events.as_ref(),
            adapter_log_lines.as_ref(),
        )
        .await;
    }
    let mut err = tokio::io::stderr();
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buf).await?;
        if read == 0 {
            if let Some(file) = &mut log_file {
                file.flush().await?;
            }
            return Ok(LimitedOutput {
                bytes: out,
                truncated,
            });
        }
        if stream_stderr {
            err.write_all(&buf[..read]).await?;
        }
        if let Some(file) = &mut log_file {
            file.write_all(&buf[..read]).await?;
        }
        let remaining = limit.saturating_sub(out.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        if read > remaining {
            out.extend_from_slice(&buf[..remaining]);
            truncated = true;
        } else {
            out.extend_from_slice(&buf[..read]);
        }
    }
}

async fn read_limited_and_filter_adapter_events<R>(
    reader: &mut R,
    limit: usize,
    stream_stderr: bool,
    mut log_file: Option<tokio::fs::File>,
    adapter_events: Option<&AdapterEventSender>,
    adapter_log_lines: Option<&AdapterLogLineSender>,
) -> anyhow::Result<LimitedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut err = tokio::io::stderr();
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];
    let mut pending_line = String::new();
    loop {
        let read = reader.read(&mut buf).await?;
        if read == 0 {
            if !pending_line.is_empty() {
                let line = std::mem::take(&mut pending_line);
                handle_stderr_line(
                    &line,
                    adapter_events,
                    &mut err,
                    &mut log_file,
                    stream_stderr,
                    limit,
                    &mut out,
                    &mut truncated,
                    adapter_log_lines,
                )
                .await?;
            }
            if let Some(file) = &mut log_file {
                file.flush().await?;
            }
            return Ok(LimitedOutput {
                bytes: out,
                truncated,
            });
        }
        pending_line.push_str(&String::from_utf8_lossy(&buf[..read]));
        while let Some(newline) = pending_line.find('\n') {
            let line = pending_line.drain(..=newline).collect::<String>();
            handle_stderr_line(
                &line,
                adapter_events,
                &mut err,
                &mut log_file,
                stream_stderr,
                limit,
                &mut out,
                &mut truncated,
                adapter_log_lines,
            )
            .await?;
        }
        while pending_line.chars().count() >= MAX_ADAPTER_STDERR_LINE_CHARS {
            let split_at = byte_index_after_chars(&pending_line, MAX_ADAPTER_STDERR_LINE_CHARS)
                .unwrap_or(pending_line.len());
            let line = pending_line.drain(..split_at).collect::<String>();
            handle_stderr_line(
                &line,
                adapter_events,
                &mut err,
                &mut log_file,
                stream_stderr,
                limit,
                &mut out,
                &mut truncated,
                adapter_log_lines,
            )
            .await?;
        }
    }
}

async fn handle_stderr_line(
    line: &str,
    adapter_events: Option<&AdapterEventSender>,
    err: &mut tokio::io::Stderr,
    log_file: &mut Option<tokio::fs::File>,
    stream_stderr: bool,
    limit: usize,
    out: &mut Vec<u8>,
    truncated: &mut bool,
    adapter_log_lines: Option<&AdapterLogLineSender>,
) -> anyhow::Result<()> {
    let mut trimmed = line.to_string();
    trim_line_ending(&mut trimmed);
    if emit_adapter_event_line(adapter_events, &trimmed).await {
        return Ok(());
    }
    if !trimmed.is_empty() {
        if let Some(sender) = adapter_log_lines {
            let _ = sender.send(trimmed.clone()).await;
        }
    }
    let bytes = line.as_bytes();
    if stream_stderr {
        err.write_all(bytes).await?;
    }
    if let Some(file) = log_file {
        file.write_all(bytes).await?;
    }
    append_limited_output(out, truncated, limit, bytes);
    Ok(())
}

fn append_limited_output(out: &mut Vec<u8>, truncated: &mut bool, limit: usize, bytes: &[u8]) {
    let remaining = limit.saturating_sub(out.len());
    if remaining == 0 {
        *truncated = true;
    } else if bytes.len() > remaining {
        out.extend_from_slice(&bytes[..remaining]);
        *truncated = true;
    } else {
        out.extend_from_slice(bytes);
    }
}

async fn emit_adapter_event_line(adapter_events: Option<&AdapterEventSender>, line: &str) -> bool {
    let Some(payload) = line.strip_prefix(APP_ADAPTER_EVENT_PREFIX) else {
        return false;
    };
    let Some(adapter_events) = adapter_events else {
        return false;
    };
    match serde_json::from_str::<DagExecutionEvent>(payload.trim()) {
        Ok(event) => {
            let _ = adapter_events.send(event).await;
        }
        Err(err) => {
            tracing::warn!(err = %err, "ignored malformed AgentHero adapter event");
        }
    }
    true
}

fn trim_line_ending(line: &mut String) {
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
}

fn byte_index_after_chars(value: &str, count: usize) -> Option<usize> {
    if count == 0 {
        return Some(0);
    }
    value.char_indices().nth(count).map(|(index, _)| index)
}

struct LimitedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn validate_adapter_response(
    request: &AppAdapterRequest,
    response: &AppAdapterResponse,
) -> anyhow::Result<()> {
    if response.protocol != APP_ADAPTER_PROTOCOL {
        anyhow::bail!(
            "app `{}` adapter returned protocol `{}`, expected `{}`",
            request.app,
            response.protocol,
            APP_ADAPTER_PROTOCOL
        );
    }
    if response.app != request.app {
        anyhow::bail!(
            "adapter response app `{}` did not match request app `{}`",
            response.app,
            request.app
        );
    }
    if response.action != request.action {
        anyhow::bail!(
            "adapter response action `{}` did not match request action `{}`",
            response.action,
            request.action
        );
    }
    if response.dag_type != request.dag_type {
        anyhow::bail!(
            "adapter response dag_type `{}` did not match request dag_type `{}`",
            response.dag_type,
            request.dag_type
        );
    }
    Ok(())
}

fn adapter_timeout_from_env() -> Option<u64> {
    let value = std::env::var("AGENTHERO_ADAPTER_TIMEOUT_SECS").ok()?;
    let trimmed = value.trim();
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "none" | "no_timeout" | "unbounded" | "off"
    ) {
        return Some(0);
    }
    trimmed.parse::<u64>().ok()
}

fn adapter_timeout_duration(manifest_timeout_secs: Option<u64>) -> Option<Duration> {
    let timeout_secs = manifest_timeout_secs
        .or_else(adapter_timeout_from_env)
        .unwrap_or(DEFAULT_ADAPTER_TIMEOUT_SECS);
    (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs))
}

fn adapter_output_limit_from_env() -> Option<usize> {
    std::env::var("AGENTHERO_ADAPTER_OUTPUT_LIMIT_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn adapter_fallback_allowed() -> bool {
    cfg!(debug_assertions)
        || std::env::var("AGENTHERO_ALLOW_ADAPTER_FALLBACK")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn adapter_workdir() -> PathBuf {
    std::env::var_os("AGENTHERO_ADAPTER_CWD")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            discover_workspace_root()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        })
}

/// Root directory for AgentHero runtime-local state such as app-run logs.
pub fn agenthero_runtime_root() -> PathBuf {
    if let Some(path) = std::env::var_os(AGENTHERO_RUNTIME_ROOT_ENV) {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
    }
    discover_workspace_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".agenthero")
}

/// Deterministic stderr log path for one queued app run.
pub fn app_run_log_path(run_id: Uuid) -> PathBuf {
    app_run_log_path_for_runtime_root(&agenthero_runtime_root(), run_id)
}

fn app_run_log_path_for_runtime_root(runtime_root: &Path, run_id: Uuid) -> PathBuf {
    runtime_root.join("app_runs").join(format!("{run_id}.log"))
}

fn truncate_for_error(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_string()
    } else {
        format!(
            "{}...<truncated>",
            value.chars().take(limit).collect::<String>()
        )
    }
}

/// Root directory containing installed DAGOps apps.
pub fn apps_root() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENTHERO_APPS_ROOT") {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
    }
    discover_workspace_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("agenthero")
        .join("apps")
}

/// Root directory for one installed DAGOps app.
pub fn app_root(app: &str) -> PathBuf {
    apps_root().join(app)
}

fn app_action_stream_stderr_requested(input: &DagIo) -> bool {
    ["stream_stderr", "debug_logs"]
        .iter()
        .any(|key| input.values.get(*key).and_then(|value| value.as_bool()) == Some(true))
}

fn app_action_log_path_requested(input: &DagIo) -> Option<PathBuf> {
    input
        .values
        .get(APP_RUN_LOG_PATH_INPUT_KEY)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn required_observability_contract() -> AppObservabilityContract {
        AppObservabilityContract {
            events: true,
            logs: true,
            status: true,
            event_stream: true,
            lifecycle_events: vec![
                "app_action.started".to_string(),
                "app_action.completed".to_string(),
                "app_action.failed".to_string(),
            ],
            trace_fields: agenthero_agent_runtime::AGENTHERO_EVENT_TRACE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
        }
    }

    #[test]
    fn queued_app_run_stream_flag_requests_outer_adapter_stderr_streaming() {
        let mut input = DagIo::default();
        input
            .values
            .insert("stream_stderr".to_string(), json!(true));

        assert!(app_action_stream_stderr_requested(&input));
    }

    #[test]
    fn queued_app_run_debug_logs_requests_outer_adapter_stderr_streaming() {
        let mut input = DagIo::default();
        input.values.insert("debug_logs".to_string(), json!(true));

        assert!(app_action_stream_stderr_requested(&input));
    }

    #[test]
    fn app_run_log_path_lives_under_runtime_app_runs_dir() {
        let run_id = uuid::Uuid::parse_str("2d0a1d88-b9f9-4e8f-848e-605b86717330").unwrap();
        let runtime_root = Path::new("/tmp/agenthero-runtime");

        assert_eq!(
            app_run_log_path_for_runtime_root(runtime_root, run_id),
            runtime_root
                .join("app_runs")
                .join("2d0a1d88-b9f9-4e8f-848e-605b86717330.log")
        );
    }

    #[tokio::test]
    async fn stderr_reader_writes_durable_log_file_without_streaming() {
        let run_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-log-test-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("run.log");
        let mut reader: &[u8] = b"line one\nline two\n";

        let output = read_limited_and_tee_stderr(
            &mut reader,
            1024,
            false,
            Some(log_path.clone()),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(output.bytes, b"line one\nline two\n");
        assert_eq!(
            std::fs::read_to_string(&log_path).unwrap(),
            "line one\nline two\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stderr_reader_emits_prefixed_adapter_events() {
        let payload = serde_json::json!({
            "level": "info",
            "event_type": "node.started",
            "node_id": "compile",
            "message": "compile started",
            "payload": {
                "node_id": "compile",
                "attempt": 1
            }
        });
        let line = format!(
            "plain stderr\n{}{}\n",
            agenthero_agent_runtime::APP_ADAPTER_EVENT_PREFIX,
            serde_json::to_string(&payload).unwrap()
        );
        let mut reader = tokio::io::BufReader::new(line.as_bytes());
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(8);

        let output =
            read_limited_and_tee_stderr(&mut reader, 4096, false, None, Some(tx), Some(log_tx))
                .await
                .unwrap();

        let event = rx.recv().await.expect("adapter event should be emitted");
        assert_eq!(event.event_type, "node.started");
        assert_eq!(event.node_id.as_deref(), Some("compile"));
        assert_eq!(event.payload["attempt"], serde_json::json!(1));
        assert_eq!(
            log_rx
                .recv()
                .await
                .expect("raw stderr line should be emitted"),
            "plain stderr"
        );
        assert!(log_rx.try_recv().is_err());
        let stderr = String::from_utf8(output.bytes).unwrap();
        assert!(stderr.contains("plain stderr"));
        assert!(!stderr.contains(agenthero_agent_runtime::APP_ADAPTER_EVENT_PREFIX));
    }

    #[tokio::test]
    async fn stderr_reader_flushes_long_newline_free_lines() {
        let input = vec![b'x'; 20_000];
        let mut reader = tokio::io::BufReader::new(input.as_slice());
        let (log_tx, mut log_rx) = tokio::sync::mpsc::channel(8);

        let output =
            read_limited_and_tee_stderr(&mut reader, 4096, false, None, None, Some(log_tx))
                .await
                .unwrap();

        let mut chunks = Vec::new();
        while let Ok(chunk) = log_rx.try_recv() {
            chunks.push(chunk);
        }
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 4096));
        assert_eq!(output.bytes.len(), 4096);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn adapter_process_cancellation_kills_child_and_drains_observable_output() {
        let manifest = AppManifest {
            version: 1,
            slug: "cancel-smoke".to_string(),
            label: "Cancel Smoke".to_string(),
            description: String::new(),
            observability: required_observability_contract(),
            adapter: AppAdapter::Process {
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    format!(
                        "printf '{}{}\\n' >&2; sleep 10",
                        agenthero_agent_runtime::APP_ADAPTER_EVENT_PREFIX,
                        serde_json::json!({
                            "level": "info",
                            "event_type": "node.started",
                            "node_id": "sleep",
                            "message": "sleep started",
                            "payload": {"attempt": 1}
                        })
                    ),
                ],
                fallback_command: None,
                fallback_args: Vec::new(),
                timeout_secs: Some(30),
                output_limit_bytes: Some(4096),
            },
            actions: vec![AppManifestAction {
                id: "run".to_string(),
                command: vec!["run".to_string()],
                dag_type: "cancel-smoke".to_string(),
                description: String::new(),
                options: Vec::new(),
                retry: AppActionRetryPolicy::default(),
            }],
            deployments: Vec::new(),
        };
        let request = AppAdapterRequest::new(
            "cancel-smoke",
            "run",
            "cancel-smoke",
            Vec::new(),
            DagIo::default(),
            true,
            false,
        );
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);

        let task = tokio::spawn(async move {
            run_adapter_process(
                &manifest,
                &request,
                false,
                Some(event_tx),
                None,
                Some(cancel_rx),
            )
            .await
        });

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("adapter event should be drained before cancellation completes")
            .expect("adapter event should be forwarded");
        assert_eq!(event.event_type, "node.started");
        cancel_tx
            .send(true)
            .expect("cancellation receiver is alive");

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("adapter cancellation cleanup should return promptly")
            .expect("adapter task should join");
        let err = result.expect_err("cancelled adapter should return an error");
        assert!(format!("{err:#}").contains("cancelled"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn adapter_process_cancellation_kills_descendant_processes_before_late_side_effect() {
        let run_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("agenthero-adapter-cancel-test-{run_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("descendant-survived");
        let script = format!(
            "MARKER='{}'; printf '{}{}\\n' >&2; /bin/sh -c 'sleep 5; printf leaked > \"$1\"' child \"$MARKER\"",
            marker.display(),
            agenthero_agent_runtime::APP_ADAPTER_EVENT_PREFIX,
            serde_json::json!({
                "level": "info",
                "event_type": "node.started",
                "node_id": "descendant",
                "message": "descendant started",
                "payload": {"attempt": 1}
            })
        );
        let manifest = AppManifest {
            version: 1,
            slug: "cancel-descendant-smoke".to_string(),
            label: "Cancel Descendant Smoke".to_string(),
            description: String::new(),
            observability: required_observability_contract(),
            adapter: AppAdapter::Process {
                command: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), script],
                fallback_command: None,
                fallback_args: Vec::new(),
                timeout_secs: Some(30),
                output_limit_bytes: Some(4096),
            },
            actions: vec![AppManifestAction {
                id: "run".to_string(),
                command: vec!["run".to_string()],
                dag_type: "cancel-descendant-smoke".to_string(),
                description: String::new(),
                options: Vec::new(),
                retry: AppActionRetryPolicy::default(),
            }],
            deployments: Vec::new(),
        };
        let request = AppAdapterRequest::new(
            "cancel-descendant-smoke",
            "run",
            "cancel-descendant-smoke",
            Vec::new(),
            DagIo::default(),
            true,
            false,
        );
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);

        let task = tokio::spawn(async move {
            run_adapter_process(
                &manifest,
                &request,
                false,
                Some(event_tx),
                None,
                Some(cancel_rx),
            )
            .await
        });

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .expect("adapter event should be drained before cancellation completes")
            .expect("adapter event should be forwarded");
        assert_eq!(event.event_type, "node.started");
        cancel_tx
            .send(true)
            .expect("cancellation receiver is alive");

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("adapter cancellation cleanup should return promptly")
            .expect("adapter task should join");
        let err = result.expect_err("cancelled adapter should return an error");
        assert!(format!("{err:#}").contains("cancelled"));

        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        assert!(
            !marker.exists(),
            "adapter cancellation left a descendant process alive long enough to write {marker:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn discover_workspace_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    starts.into_iter().find_map(|start| {
        start.ancestors().find_map(|candidate| {
            candidate
                .join("agenthero")
                .join("apps")
                .is_dir()
                .then(|| candidate.to_path_buf())
        })
    })
}

fn resolve_process_command(command: &str, bin_dir_env: &str) -> PathBuf {
    let command_path = PathBuf::from(command);
    if command_path.is_absolute() || command_path.components().count() > 1 {
        return command_path;
    }
    if let Some(path) = std::env::var_os(bin_dir_env).map(PathBuf::from) {
        let candidate = path.join(command);
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join(command);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    command_path
}
