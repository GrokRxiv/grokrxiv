//! AgentHero platform telemetry setup.

use anyhow::Context as _;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

const DEFAULT_TELEMETRY_FILTER: &str = "info,agenthero_orchestrator=debug";

/// Resolved platform telemetry settings for one AgentHero process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetrySettings {
    /// Whether tracing output should be emitted.
    pub enabled: bool,
    /// Whether tracing output should be structured JSON.
    pub json: bool,
    /// Optional JSONL tracing destination.
    pub log_file: Option<PathBuf>,
    /// EnvFilter expression used for enabled telemetry.
    pub filter: String,
}

impl TelemetrySettings {
    /// Resolve settings from CLI-level telemetry intent.
    pub fn from_parts(debug_logs: bool, serve: bool, log_file: Option<PathBuf>) -> Self {
        Self::from_sources(debug_logs, serve, log_file, None)
    }

    /// Resolve settings from CLI and environment telemetry intent.
    pub fn from_process(debug_logs: bool, serve: bool, log_file: Option<PathBuf>) -> Self {
        Self::from_sources(
            debug_logs,
            serve,
            log_file,
            std::env::var_os("AGENTHERO_LOG_FILE").map(PathBuf::from),
        )
    }

    /// Resolve settings from explicit source values.
    pub fn from_sources(
        debug_logs: bool,
        serve: bool,
        cli_log_file: Option<PathBuf>,
        env_log_file: Option<PathBuf>,
    ) -> Self {
        let log_file = cli_log_file.or(env_log_file);
        let enabled = debug_logs || serve || log_file.is_some();
        Self {
            enabled,
            json: enabled,
            log_file,
            filter: if enabled {
                DEFAULT_TELEMETRY_FILTER.to_string()
            } else {
                "off".to_string()
            },
        }
    }
}

/// Keeps background telemetry writers alive until process shutdown.
#[derive(Debug)]
pub struct TelemetryGuard {
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl TelemetryGuard {
    fn stderr_only() -> Self {
        Self { _file_guard: None }
    }

    fn file(file_guard: tracing_appender::non_blocking::WorkerGuard) -> Self {
        Self {
            _file_guard: Some(file_guard),
        }
    }
}

/// Initialize AgentHero process telemetry.
pub fn init(settings: &TelemetrySettings) -> anyhow::Result<TelemetryGuard> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(settings.filter.clone()));
    if !settings.enabled {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|err| {
                anyhow::anyhow!("initialize disabled AgentHero tracing subscriber: {err}")
            })?;
        return Ok(TelemetryGuard::stderr_only());
    }

    let Some(log_file) = settings.log_file.as_ref() else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .with_current_span(false)
            .with_writer(std::io::stderr)
            .try_init()
            .map_err(|err| {
                anyhow::anyhow!("initialize AgentHero stderr tracing subscriber: {err}")
            })?;
        return Ok(TelemetryGuard::stderr_only());
    };

    if let Some(parent) = log_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create AgentHero log directory {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("open AgentHero log file {}", log_file.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .with_current_span(false)
        .with_writer(writer)
        .try_init()
        .map_err(|err| anyhow::anyhow!("initialize AgentHero file tracing subscriber: {err}"))?;
    Ok(TelemetryGuard::file(guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_enables_structured_json_telemetry_for_audit_logs() {
        let settings = TelemetrySettings::from_parts(
            false,
            false,
            Some(std::path::PathBuf::from(".agenthero/logs/agenthero.jsonl")),
        );

        assert!(settings.enabled);
        assert!(settings.json);
        assert_eq!(
            settings.log_file.as_deref(),
            Some(std::path::Path::new(".agenthero/logs/agenthero.jsonl"))
        );
        assert_eq!(settings.filter, "info,agenthero_orchestrator=debug");
    }

    #[test]
    fn env_log_file_enables_structured_json_telemetry_when_cli_path_is_absent() {
        let settings = TelemetrySettings::from_sources(
            false,
            false,
            None,
            Some(std::path::PathBuf::from(".agenthero/logs/from-env.jsonl")),
        );

        assert!(settings.enabled);
        assert!(settings.json);
        assert_eq!(
            settings.log_file.as_deref(),
            Some(std::path::Path::new(".agenthero/logs/from-env.jsonl"))
        );
    }
}
