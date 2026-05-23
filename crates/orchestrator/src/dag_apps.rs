//! Manifest-driven DAGOps app registry and process adapter runner.
//!
//! The orchestrator owns discovery and scheduling. Product behavior lives
//! behind `agenthero/apps/<app>/app.yaml` and its declared adapter process.

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_PROTOCOL};
use agenthero_dag_executor::{DagExecutionReport, DagIo};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const DEFAULT_ADAPTER_TIMEOUT_SECS: u64 = 60 * 60 * 2;
const DEFAULT_ADAPTER_OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;

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
    /// App-owned deployable surfaces, such as generated websites.
    #[serde(default)]
    pub deployments: Vec<AppDeployment>,
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
            description: manifest.description,
            deployments: manifest.deployments,
            actions: manifest
                .actions
                .into_iter()
                .map(|action| AppActionDescriptor {
                    id: action.id,
                    command: action.command,
                    dag_type: action.dag_type,
                    description: action.description,
                    options: action.options,
                })
                .collect(),
        })
        .collect())
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
    let action = manifest
        .actions
        .iter()
        .filter(|action| command_path_matches(&action.command, args))
        .max_by_key(|action| action.command.len())
        .ok_or_else(|| {
            let requested = args.first().map(String::as_str).unwrap_or("<none>");
            anyhow::anyhow!("unknown app action `{} {requested}`", manifest.slug)
        })?;
    Ok(ResolvedAppAction {
        app: manifest.slug.clone(),
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
    run_app_action_with_manifest(&manifest, action_id, args, input, json, dry_run).await
}

/// Run an action through an already loaded app manifest.
pub async fn run_app_action_with_manifest(
    manifest: &AppManifest,
    action_id: &str,
    args: Vec<String>,
    input: DagIo,
    json: bool,
    dry_run: bool,
) -> anyhow::Result<AppAdapterResponse> {
    let action = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
        .ok_or_else(|| anyhow::anyhow!("unknown app action `{} {action_id}`", manifest.slug))?;
    let request = AppAdapterRequest::new(
        manifest.slug.clone(),
        action.id.clone(),
        action.dag_type.clone(),
        args,
        input,
        json,
        dry_run,
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
        false,
    );
    let response = run_adapter_process(&manifest, &request).await?;
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
) -> anyhow::Result<AppAdapterResponse> {
    let AppAdapter::Process {
        command,
        args,
        fallback_command,
        fallback_args,
        timeout_secs,
        output_limit_bytes,
    } = &manifest.adapter;
    let timeout = Duration::from_secs(
        timeout_secs
            .or_else(adapter_timeout_from_env)
            .unwrap_or(DEFAULT_ADAPTER_TIMEOUT_SECS),
    );
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
    let stderr_task = tokio::spawn(async move { read_limited(&mut stderr, output_limit).await });

    let payload = serde_json::to_vec(request)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("adapter `{}` stdin unavailable", manifest.slug))?;
    stdin.write_all(&payload).await?;
    stdin.shutdown().await?;
    drop(stdin);

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result
            .map_err(|err| anyhow::anyhow!("wait for app `{}` adapter: {err}", manifest.slug))?,
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!(
                "app `{}` adapter timed out after {}s",
                manifest.slug,
                timeout.as_secs()
            );
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|err| anyhow::anyhow!("join app `{}` stdout task: {err}", manifest.slug))??;
    let stderr = stderr_task
        .await
        .map_err(|err| anyhow::anyhow!("join app `{}` stderr task: {err}", manifest.slug))??;
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

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        anyhow::bail!(
            "app `{}` adapter exited with {}: {}",
            manifest.slug,
            status,
            truncate_for_error(stderr.trim(), 2048)
        );
    }

    let response: AppAdapterResponse = serde_json::from_slice(&stdout).map_err(|err| {
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
    tokio::process::Command::new(command_path)
        .args(args)
        .current_dir(workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(Into::into)
}

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
    std::env::var("AGENTHERO_ADAPTER_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
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
