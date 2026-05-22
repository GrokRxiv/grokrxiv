//! Runtime configuration loaded from environment variables.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::scheduler::SchedulerConfig;

/// Result of loading the root `.env` plus any split env files referenced by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedEnv {
    /// Root dotenv file loaded through dotenvy's normal upward search.
    pub root: Option<PathBuf>,
    /// Purpose-specific env files loaded from `AGENTHERO_ENV_FILES`.
    pub includes: Vec<PathBuf>,
}

/// Load the repo dotenv contract.
///
/// The root `.env` remains the entry point. If it defines
/// `AGENTHERO_ENV_FILES=.env_core,.env_review`, those files are loaded relative
/// to the root `.env` directory. Existing process variables and values from the
/// root `.env` win over included files, matching dotenvy's default behavior.
pub fn load_env() -> anyhow::Result<LoadedEnv> {
    let root = match dotenvy::dotenv() {
        Ok(path) => Some(path),
        Err(dotenvy::Error::Io(err)) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(anyhow::anyhow!("load .env: {err}")),
    };
    load_env_includes(root)
}

#[cfg(test)]
fn load_env_from_path(path: &Path) -> anyhow::Result<LoadedEnv> {
    dotenvy::from_path(path).map_err(|err| anyhow::anyhow!("load {}: {err}", path.display()))?;
    load_env_includes(Some(path.to_path_buf()))
}

fn load_env_includes(root: Option<PathBuf>) -> anyhow::Result<LoadedEnv> {
    let raw = match env::var("AGENTHERO_ENV_FILES") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(env::VarError::NotPresent) => {
            return Ok(LoadedEnv {
                root,
                includes: Vec::new(),
            })
        }
        Err(err) => return Err(anyhow::anyhow!("read AGENTHERO_ENV_FILES: {err}")),
    };

    let base_dir = root
        .as_ref()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(env::current_dir()?);
    let mut includes = Vec::new();
    for entry in split_env_files(&raw) {
        let path = resolve_env_path(&base_dir, entry);
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "AGENTHERO_ENV_FILES references missing file {}",
                path.display()
            ));
        }
        if !fs::metadata(&path)?.is_file() {
            return Err(anyhow::anyhow!(
                "AGENTHERO_ENV_FILES entry {} is not a file",
                path.display()
            ));
        }
        dotenvy::from_path(&path)
            .map_err(|err| anyhow::anyhow!("load included env {}: {err}", path.display()))?;
        includes.push(path);
    }
    Ok(LoadedEnv { root, includes })
}

fn split_env_files(raw: &str) -> impl Iterator<Item = &str> {
    raw.split([',', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
}

fn resolve_env_path(base_dir: &Path, entry: &str) -> PathBuf {
    let path = PathBuf::from(entry);
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

/// Configuration loaded once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    /// Bind address for the HTTP server, e.g. `0.0.0.0:8080`.
    pub bind: String,
    /// Optional Supabase Postgres URL. When `None` the orchestrator runs in
    /// "stateless" mode (no DB writes); useful for local M1 demos before
    /// migrations land.
    pub database_url: Option<String>,
    /// Required bearer token for admin endpoints (`/ingest`).
    pub admin_token: Option<String>,
    /// HMAC secret used to verify GitHub webhook signatures.
    pub github_webhook_secret: Option<String>,
    /// Frontend revalidation URL hit on publish.
    pub web_revalidate_url: Option<String>,
    /// Shared secret sent to the revalidate endpoint.
    pub revalidate_secret: Option<String>,
    /// User-Agent string used when talking to arXiv. Defaults to a stable
    /// string that includes the project contact.
    pub arxiv_user_agent: String,
    /// Model name used by the single-pass `/preview` path. Overridable via
    /// `GROKRXIV_PREVIEW_MODEL` (preferred) or legacy `PREVIEW_MODEL`.
    pub preview_model: String,
    /// Scheduler tuning (categories, backfill window, auto-review cutoff). The
    /// scheduler task uses this directly; the supervisor reads
    /// `scheduler.auto_review_from` when deciding whether a freshly-ingested
    /// paper auto-enqueues a Review job.
    pub scheduler: SchedulerConfig,
}

impl Config {
    /// Construct from process environment.
    pub fn from_env() -> Self {
        Self {
            bind: env::var("ORCHESTRATOR_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("DATABASE_URL").ok(),
            admin_token: env::var("ADMIN_TOKEN").ok(),
            github_webhook_secret: env::var("GITHUB_WEBHOOK_SECRET").ok(),
            web_revalidate_url: env::var("WEB_REVALIDATE_URL").ok(),
            revalidate_secret: env::var("REVALIDATE_SECRET").ok(),
            arxiv_user_agent: env::var("ARXIV_USER_AGENT")
                .unwrap_or_else(|_| "GrokRxiv/0.1 (mailto:mlong168@gmail.com)".into()),
            preview_model: env::var("GROKRXIV_PREVIEW_MODEL")
                .or_else(|_| env::var("PREVIEW_MODEL"))
                .unwrap_or_else(|_| "claude-haiku-4-5-20251001".into()),
            scheduler: SchedulerConfig::from_env(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    const TEST_KEYS: &[&str] = &[
        "AGENTHERO_ENV_FILES",
        "GROKRXIV_TEST_ROOT",
        "GROKRXIV_TEST_CORE",
        "GROKRXIV_TEST_REVIEW",
        "GROKRXIV_TEST_SHARED",
    ];

    #[test]
    fn load_env_reads_split_files_listed_by_root_env() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".env");
        fs::write(
            &root,
            "AGENTHERO_ENV_FILES=.env_core,.env_review\nGROKRXIV_TEST_ROOT=root\nGROKRXIV_TEST_SHARED=root\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join(".env_core"),
            "GROKRXIV_TEST_CORE=core\nGROKRXIV_TEST_SHARED=core\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join(".env_review"),
            "GROKRXIV_TEST_REVIEW=review\n",
        )
        .unwrap();

        let loaded = load_env_from_path(&root).unwrap();

        assert_eq!(loaded.root, Some(root));
        assert_eq!(
            loaded.includes,
            vec![tmp.path().join(".env_core"), tmp.path().join(".env_review")]
        );
        assert_eq!(env::var("GROKRXIV_TEST_ROOT").as_deref(), Ok("root"));
        assert_eq!(env::var("GROKRXIV_TEST_CORE").as_deref(), Ok("core"));
        assert_eq!(env::var("GROKRXIV_TEST_REVIEW").as_deref(), Ok("review"));
        assert_eq!(
            env::var("GROKRXIV_TEST_SHARED").as_deref(),
            Ok("root"),
            "root .env values should win over included env files"
        );
        clear_test_env();
    }

    #[test]
    fn load_env_reports_missing_split_env_files() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        clear_test_env();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".env");
        fs::write(&root, "AGENTHERO_ENV_FILES=.env_missing\n").unwrap();

        let err = load_env_from_path(&root).unwrap_err();

        assert!(
            err.to_string().contains(".env_missing"),
            "unexpected error: {err:#}"
        );
        clear_test_env();
    }

    fn clear_test_env() {
        for key in TEST_KEYS {
            env::remove_var(key);
        }
    }
}
