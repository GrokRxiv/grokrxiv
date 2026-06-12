//! Reusable helpers for process-backed AgentHero DAGOps app adapters.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use agenthero_agent_runtime::{AppAdapterRequest, AppAdapterResponse};
use agenthero_dag_runtime::DagManifest;

/// Read and parse a process-adapter request from a blocking input stream.
pub fn read_adapter_request(mut reader: impl std::io::Read) -> anyhow::Result<AppAdapterRequest> {
    let mut payload = String::new();
    reader
        .read_to_string(&mut payload)
        .map_err(|err| anyhow::anyhow!("read adapter request: {err}"))?;
    parse_adapter_request(&payload)
}

/// Parse the JSON request sent to an app process adapter over stdin.
pub fn parse_adapter_request(payload: &str) -> anyhow::Result<AppAdapterRequest> {
    serde_json::from_str(payload).map_err(|err| anyhow::anyhow!("parse adapter request: {err}"))
}

/// Serialize and write an app process-adapter response to a blocking stream.
pub fn write_adapter_response(
    mut writer: impl std::io::Write,
    response: &AppAdapterResponse,
) -> anyhow::Result<()> {
    let json = response_to_json(response)?;
    writeln!(writer, "{json}").map_err(|err| anyhow::anyhow!("write adapter response: {err}"))
}

/// Serialize an app process adapter response as the stdout JSON payload.
pub fn response_to_json(response: &AppAdapterResponse) -> anyhow::Result<String> {
    serde_json::to_string(response)
        .map_err(|err| anyhow::anyhow!("serialize adapter response: {err}"))
}

/// Resolve an app root from an adapter crate manifest directory.
///
/// App adapter crates conventionally live directly under
/// `agenthero/apps/<app>/rust`, so the app root is the parent directory.
pub fn app_root_from_manifest_dir(manifest_dir: impl AsRef<Path>) -> PathBuf {
    manifest_dir
        .as_ref()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolve an app root using standard AgentHero adapter environment variables.
pub fn resolve_app_root(app_slug: &str, manifest_dir: impl AsRef<Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("AGENTHERO_APP_ROOT").map(PathBuf::from) {
        return if path.is_absolute() {
            path
        } else {
            resolve_relative_path(app_slug, &path)
        };
    }
    if let Some(path) = std::env::var_os("AGENTHERO_APPS_ROOT").map(PathBuf::from) {
        let apps_root = if path.is_absolute() {
            path
        } else {
            resolve_relative_path(app_slug, &path)
        };
        let candidate = apps_root.join(app_slug);
        if candidate.join("app.yaml").is_file() {
            return candidate;
        }
    }
    discover_app_root(app_slug).unwrap_or_else(|| app_root_from_manifest_dir(manifest_dir))
}

/// Return the path to a DAG manifest under an app root.
pub fn dag_manifest_path(app_root: impl AsRef<Path>, dag_type: &str) -> PathBuf {
    app_root
        .as_ref()
        .join("dags")
        .join(format!("{dag_type}.yaml"))
}

/// Load one DAG manifest by type from an app root and verify the id matches.
pub fn load_dag_manifest(
    app_root: impl AsRef<Path>,
    dag_type: &str,
) -> anyhow::Result<DagManifest> {
    let path = dag_manifest_path(app_root, dag_type);
    let manifest = DagManifest::from_path(&path)
        .map_err(|err| anyhow::anyhow!("load {}: {err}", path.display()))?;
    if manifest.id.as_str() != dag_type {
        anyhow::bail!(
            "manifest {} id `{}` does not match requested dag_type `{dag_type}`",
            path.display(),
            manifest.id
        );
    }
    Ok(manifest)
}

/// Resolve an adapter-owned runtime binary from an environment override or default name.
pub fn resolve_binary(env_var: &str, default_name: &str) -> String {
    std::env::var(env_var).unwrap_or_else(|_| default_name.to_string())
}

/// Resolve an app runtime binary from an explicit env var, app bin dir, current
/// executable dir, or finally the executable name.
pub fn resolve_runtime_binary(env_var: &str, default_name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_var).map(PathBuf::from) {
        return path;
    }
    if let Some(path) = std::env::var_os("AGENTHERO_APP_BIN_DIR").map(PathBuf::from) {
        let candidate = path.join(default_name);
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join(default_name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(default_name)
}

fn resolve_relative_path(app_slug: &str, path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_candidate = cwd.join(path);
    if cwd_candidate.exists() {
        return cwd_candidate;
    }
    if let Some(workspace) = discover_workspace_root(app_slug) {
        let workspace_candidate = workspace.join(path);
        if workspace_candidate.exists() {
            return workspace_candidate;
        }
    }
    cwd_candidate
}

fn discover_app_root(app_slug: &str) -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    starts.into_iter().find_map(|start| {
        start.ancestors().find_map(|candidate| {
            let direct = candidate.join("app.yaml");
            if direct.is_file() && candidate.file_name().and_then(|n| n.to_str()) == Some(app_slug)
            {
                return Some(candidate.to_path_buf());
            }
            let nested = candidate.join("agenthero").join("apps").join(app_slug);
            nested.join("app.yaml").is_file().then_some(nested)
        })
    })
}

fn discover_workspace_root(app_slug: &str) -> Option<PathBuf> {
    discover_app_root(app_slug).and_then(|app| {
        app.parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    })
}
