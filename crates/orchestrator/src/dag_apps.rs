//! Manifest-driven DAGOps app registry and process adapter runner.
//!
//! The orchestrator owns discovery and scheduling. Product behavior lives
//! behind `agenthero/apps/<app>/app.yaml` and its declared adapter process.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_PROTOCOL};
use agenthero_dag_executor::{DagExecutionReport, DagIo};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

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
    /// Actions exposed by this app.
    pub actions: Vec<AppActionDescriptor>,
}

/// Metadata for one app action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppActionDescriptor {
    /// Action id used by `agh app run <app> <action>`.
    pub id: String,
    /// DAG type that supplies this action's runtime topology.
    pub dag_type: String,
    /// Short operator-facing description.
    pub description: String,
    /// Action-specific positional and flag metadata.
    pub options: Vec<AppActionOption>,
}

/// YAML product app manifest loaded from `agenthero/apps/<app>/app.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AppManifest {
    /// Product app slug used by `agh app run <app> ...`.
    pub slug: String,
    /// Human-readable app label.
    pub label: String,
    /// Operator-facing description.
    #[serde(default)]
    pub description: String,
    /// Process or future runtime adapter for this app.
    pub adapter: AppAdapter,
    /// App actions exposed by this manifest.
    #[serde(default)]
    pub actions: Vec<AppManifestAction>,
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
    Ok(load_app_manifests()?
        .into_iter()
        .map(|manifest| RegisteredAppDescriptor {
            id: manifest.slug,
            label: manifest.label,
            actions: manifest
                .actions
                .into_iter()
                .map(|action| AppActionDescriptor {
                    id: action.id,
                    dag_type: action.dag_type,
                    description: action.description,
                    options: action.options,
                })
                .collect(),
        })
        .collect())
}

/// Return all installed product app ids in deterministic order.
pub fn registered_app_ids() -> Vec<String> {
    registered_apps()
        .map(|apps| apps.into_iter().map(|app| app.id).collect())
        .unwrap_or_default()
}

/// Find one installed product app descriptor.
pub fn registered_app(app_id: &str) -> Option<RegisteredAppDescriptor> {
    registered_apps()
        .ok()?
        .into_iter()
        .find(|app| app.id == app_id)
}

/// Find one installed product app action.
pub fn registered_app_action(app_id: &str, action_id: &str) -> Option<AppActionDescriptor> {
    registered_app(app_id)?
        .actions
        .into_iter()
        .find(|action| action.id == action_id)
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

/// Resolve raw app-run args against the app manifest command paths.
pub fn resolve_app_action_args(app_id: &str, args: &[String]) -> anyhow::Result<ResolvedAppAction> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let action = manifest
        .actions
        .iter()
        .filter(|action| command_path_matches(&action.command, args))
        .max_by_key(|action| action.command.len())
        .ok_or_else(|| {
            let requested = args.first().map(String::as_str).unwrap_or("<none>");
            anyhow::anyhow!("unknown app action `{app_id} {requested}`")
        })?;
    Ok(ResolvedAppAction {
        app: manifest.slug,
        id: action.id.clone(),
        dag_type: action.dag_type.clone(),
        description: action.description.clone(),
        args: args[action.command.len()..].to_vec(),
    })
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
    validate_app_manifest(&manifest)
        .map_err(|err| anyhow::anyhow!("validate app manifest {}: {err}", path.display()))?;
    Ok(manifest)
}

fn validate_app_manifest(manifest: &AppManifest) -> anyhow::Result<()> {
    if manifest.slug.trim().is_empty() {
        anyhow::bail!("slug is required");
    }
    match &manifest.adapter {
        AppAdapter::Process { command, .. } if command.trim().is_empty() => {
            anyhow::bail!("process adapter command is required");
        }
        AppAdapter::Process { .. } => {}
    }
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
        let mut option_names = BTreeSet::new();
        for option in &action.options {
            if option.name.trim().is_empty() {
                anyhow::bail!("action `{}` option name is required", action.id);
            }
            if option.kind.trim().is_empty() {
                anyhow::bail!("action `{}` option `{}` kind is required", action.id, option.name);
            }
            if !option_names.insert(option.name.as_str()) {
                anyhow::bail!(
                    "duplicate option `{}` for action `{}`",
                    option.name,
                    action.id
                );
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
pub fn registered_dag_app_ids() -> Vec<String> {
    registered_dag_apps()
        .map(|apps| apps.into_iter().map(|app| app.dag_type).collect())
        .unwrap_or_default()
}

/// Find one discovered DAG type descriptor.
pub fn registered_dag_app(dag_type: &str) -> Option<DagAppDescriptor> {
    registered_dag_apps()
        .ok()?
        .into_iter()
        .find(|app| app.dag_type == dag_type)
}

/// Run an action through its app's declared adapter process.
pub async fn run_app_action(
    app_id: &str,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
) -> anyhow::Result<AppAdapterResponse> {
    let manifest = load_app_manifest_by_slug(app_id)?;
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{app_id} {action_id}`"))?;
    let request = AppAdapterRequest::new(
        manifest.slug.clone(),
        action.id.clone(),
        action.dag_type.clone(),
        args,
        input,
        json,
    );
    run_adapter_process(&manifest, &request).await
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
    );
    let response = run_adapter_process(&manifest, &request).await?;
    if !response.ok {
        anyhow::bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| format!("app `{}` action `{}` failed", manifest.slug, action.id))
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
) -> anyhow::Result<AppAdapterResponse> {
    let AppAdapter::Process { command, args } = &manifest.adapter;
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .current_dir(workspace_root())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| anyhow::anyhow!("spawn app `{}` adapter `{command}`: {err}", manifest.slug))?;

    let payload = serde_json::to_vec(request)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("adapter `{}` stdin unavailable", manifest.slug))?;
    stdin.write_all(&payload).await?;
    stdin.shutdown().await?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .map_err(|err| anyhow::anyhow!("wait for app `{}` adapter: {err}", manifest.slug))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "app `{}` adapter exited with {}: {}",
            manifest.slug,
            output.status,
            stderr.trim()
        );
    }

    let response: AppAdapterResponse = serde_json::from_slice(&output.stdout).map_err(|err| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::anyhow!(
            "parse app `{}` adapter response as JSON: {err}; stdout={}",
            manifest.slug,
            stdout.trim()
        )
    })?;
    if response.protocol != APP_ADAPTER_PROTOCOL {
        anyhow::bail!(
            "app `{}` adapter returned protocol `{}`, expected `{}`",
            manifest.slug,
            response.protocol,
            APP_ADAPTER_PROTOCOL
        );
    }
    Ok(response)
}

/// Root directory containing installed DAGOps apps.
pub fn apps_root() -> PathBuf {
    if let Some(path) = std::env::var_os("AGENTHERO_APPS_ROOT") {
        return PathBuf::from(path);
    }
    workspace_root().join("agenthero").join("apps")
}

/// Root directory for one installed DAGOps app.
pub fn app_root(app: &str) -> PathBuf {
    apps_root().join(app)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
