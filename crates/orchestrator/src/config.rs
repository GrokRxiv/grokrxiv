//! Platform configuration loading.

use std::net::SocketAddr;

/// Generic AgentHero runtime config.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP bind address for `agh serve`.
    pub bind: SocketAddr,
    /// Optional platform database URL for app run/job state.
    pub database_url: Option<String>,
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
        Ok(Self { bind, database_url })
    }
}

/// Load the root dotenv file when present.
pub fn load_env() -> anyhow::Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
