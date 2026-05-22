//! Platform configuration loading.

use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

/// Generic AgentHero runtime config.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP bind address for `agh serve`.
    pub bind: SocketAddr,
    /// Optional platform database URL for app run/job state.
    pub database_url: Option<String>,
    /// Optional bearer token for private app-run write routes.
    pub service_token: Option<String>,
    /// Number of local scheduler workers started by `agh serve`.
    pub scheduler_workers: usize,
}

impl Config {
    /// Resolve config from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = std::env::var("AGENTHERO_BIND")
            .or_else(|_| std::env::var("GROKRXIV_BIND"))
            .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
            .parse()?;
        let database_url = std::env::var("DATABASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let service_token = std::env::var("AGENTHERO_SERVICE_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let scheduler_workers = std::env::var("AGENTHERO_SCHEDULER_WORKERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1);
        Ok(Self {
            bind,
            database_url,
            service_token,
            scheduler_workers,
        })
    }
}

/// Load root `.env` and optional files listed in `AGENTHERO_ENV_FILES`.
pub fn load_env() -> anyhow::Result<()> {
    let root = match dotenvy::dotenv() {
        Ok(path) => Some(path),
        Err(dotenvy::Error::Io(err)) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let raw = match std::env::var("AGENTHERO_ENV_FILES") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(std::env::VarError::NotPresent) => return Ok(()),
        Err(err) => return Err(anyhow::anyhow!("read AGENTHERO_ENV_FILES: {err}")),
    };
    let base_dir = root
        .as_ref()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(std::env::current_dir()?);
    for entry in raw
        .split([',', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let path = resolve_env_path(&base_dir, entry);
        if !path.exists() {
            anyhow::bail!(
                "AGENTHERO_ENV_FILES references missing file {}",
                path.display()
            );
        }
        dotenvy::from_path(&path)
            .map_err(|err| anyhow::anyhow!("load included env {}: {err}", path.display()))?;
    }
    Ok(())
}

fn resolve_env_path(base_dir: &Path, entry: &str) -> PathBuf {
    let path = PathBuf::from(entry);
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}
