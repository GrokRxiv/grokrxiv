//! Tier 1 — `grokrxiv-data` Git repo.
//!
//! Clones (or opens) the data repo, writes per-paper directories under
//! `papers/<arxiv_id>/`, validates `.json` files against `schemas/<base>.schema.json`,
//! and commits + (optionally) pushes.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use git2::{
    build::RepoBuilder, Cred, FetchOptions, IndexAddOption, ObjectType, PushOptions,
    RemoteCallbacks, Repository, Signature,
};
use serde_json::Value;
use tracing::{debug, info};

/// Tier-1 git-backed artifact store.
pub struct GitArtifactStore {
    pub repo_path: PathBuf,
    pub remote: Option<String>,
}

impl GitArtifactStore {
    /// Open `repo_path` if it already contains a Git repo. If empty and
    /// `remote` is set, clone it. If empty and `remote` is None, initialise
    /// an empty repo with a `main` branch.
    pub fn open_or_clone(repo_path: PathBuf, remote: Option<String>) -> Result<Self> {
        if repo_path.join(".git").exists() {
            debug!(?repo_path, "opening existing grokrxiv-data repo");
            Repository::open(&repo_path).context("opening existing grokrxiv-data repo")?;
            return Ok(Self { repo_path, remote });
        }

        fs::create_dir_all(&repo_path).context("creating grokrxiv-data dir")?;

        if let Some(url) = remote.as_deref() {
            info!(%url, ?repo_path, "cloning grokrxiv-data");
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(default_credentials);
            let mut fetch_opts = FetchOptions::new();
            fetch_opts.remote_callbacks(callbacks);
            let mut builder = RepoBuilder::new();
            builder.fetch_options(fetch_opts);
            builder
                .clone(url, &repo_path)
                .with_context(|| format!("cloning {url} to {}", repo_path.display()))?;
        } else {
            info!(?repo_path, "initialising empty grokrxiv-data repo");
            let repo = Repository::init(&repo_path).context("git init")?;
            let sig = signature(&repo)?;
            let mut index = repo.index()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            let commit_oid = repo.commit(
                Some("HEAD"),
                &sig,
                &sig,
                "chore: initialise grokrxiv-data",
                &tree,
                &[],
            )?;
            // Pin the default branch to `main` regardless of the user's
            // global init.defaultBranch.
            let commit = repo.find_commit(commit_oid)?;
            repo.branch("main", &commit, true)?;
            repo.set_head("refs/heads/main")?;
        }

        Ok(Self { repo_path, remote })
    }

    /// Fast-forward `main` from `origin/main`. No-op if there is no remote.
    pub fn pull(&self) -> Result<()> {
        let Some(_) = self.remote.as_deref() else {
            return Ok(());
        };
        let repo = Repository::open(&self.repo_path)?;
        let mut remote = repo
            .find_remote("origin")
            .or_else(|_| repo.remote("origin", self.remote.as_deref().unwrap()))?;
        let mut callbacks = RemoteCallbacks::new();
        callbacks.credentials(default_credentials);
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        remote.fetch(&["main"], Some(&mut fetch_opts), None)?;
        let fetch_head = repo.find_reference("FETCH_HEAD")?;
        let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;
        let analysis = repo.merge_analysis(&[&fetch_commit])?;
        if analysis.0.is_fast_forward() {
            let refname = "refs/heads/main";
            let mut reference = repo.find_reference(refname)?;
            reference.set_target(fetch_commit.id(), "fast-forward")?;
            repo.set_head(refname)?;
            repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
        } else if analysis.0.is_up_to_date() {
            debug!("grokrxiv-data already up to date");
        } else {
            return Err(anyhow!(
                "grokrxiv-data: non-fast-forward, refusing to merge"
            ));
        }
        Ok(())
    }

    /// Write `files` (relative paths) under `papers/<arxiv_id>/`. Every `.json`
    /// file is validated against `schemas/<basename>.schema.json` if such a
    /// schema is present in the repo.
    pub fn write_paper_artifacts(
        &self,
        arxiv_id: &str,
        files: HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        validate_artifact_arxiv_id(arxiv_id)?;
        let paper_dir = self.repo_path.join("papers").join(arxiv_id);
        fs::create_dir_all(&paper_dir)
            .with_context(|| format!("creating {}", paper_dir.display()))?;

        for (rel_path, bytes) in &files {
            let safe_rel = validate_artifact_rel_path(rel_path)?;
            let full = paper_dir.join(&safe_rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent)?;
            }
            if rel_path.ends_with(".json") {
                self.validate_json(rel_path, bytes)
                    .with_context(|| format!("validating {rel_path} for paper {arxiv_id}"))?;
            }
            fs::write(&full, bytes).with_context(|| format!("writing {}", full.display()))?;
        }
        Ok(())
    }

    fn validate_json(&self, rel_path: &str, bytes: &[u8]) -> Result<()> {
        let base = Path::new(rel_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid filename {rel_path}"))?;
        let schema_path = self
            .repo_path
            .join("schemas")
            .join(format!("{base}.schema.json"));
        if !schema_path.exists() {
            debug!(rel_path, "no schema for file, skipping validation");
            return Ok(());
        }
        let schema_bytes = fs::read(&schema_path)?;
        let schema_value: Value = serde_json::from_slice(&schema_bytes)
            .with_context(|| format!("parsing schema {}", schema_path.display()))?;
        let validator = jsonschema::validator_for(&schema_value)
            .map_err(|e| anyhow!("compiling schema {}: {e}", schema_path.display()))?;
        let instance: Value =
            serde_json::from_slice(bytes).with_context(|| format!("parsing JSON {rel_path}"))?;
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(anyhow!(
                "schema validation failed for {rel_path}: {}",
                errors.join("; ")
            ));
        }
        Ok(())
    }

    /// Stage `papers/<arxiv_id>/*`, commit with a conventional message, and
    /// push if `remote` is configured. Returns the new commit SHA.
    pub fn commit_and_push(&self, arxiv_id: &str, stages: &[&str]) -> Result<String> {
        validate_artifact_arxiv_id(arxiv_id)?;
        let repo = Repository::open(&self.repo_path)?;
        let sig = signature(&repo)?;
        let mut index = repo.index()?;
        let pattern = format!("papers/{arxiv_id}");
        index.add_all([&pattern].iter(), IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        let parents: Vec<git2::Commit> = match repo.head() {
            Ok(head) => match head.peel(ObjectType::Commit) {
                Ok(obj) => vec![obj
                    .into_commit()
                    .map_err(|_| anyhow!("HEAD not a commit"))?],
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        let parents_refs: Vec<&git2::Commit> = parents.iter().collect();

        let stages_str = if stages.is_empty() {
            "extracted".to_string()
        } else {
            stages.join(",")
        };
        let message = format!("paper({arxiv_id}): extracted {stages_str}");

        let commit_oid = repo.commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents_refs)?;
        let sha = commit_oid.to_string();

        if let Some(url) = self.remote.as_deref() {
            let mut remote = repo
                .find_remote("origin")
                .or_else(|_| repo.remote("origin", url))?;
            let mut push_opts = PushOptions::new();
            // file:// and ssh-via-agent push paths converge here; SSH is the
            // common case for the production GitHub remote.
            if !url.starts_with("file://") {
                let mut callbacks = RemoteCallbacks::new();
                callbacks.credentials(default_credentials);
                push_opts.remote_callbacks(callbacks);
            }
            match remote.push(&["refs/heads/main:refs/heads/main"], Some(&mut push_opts)) {
                Ok(()) => {
                    info!(%sha, %url, "pushed grokrxiv-data");
                }
                Err(err) if should_retry_push_with_git_cli(&err, url) => {
                    debug!(%sha, %url, error = %err, "libgit2 push unsupported; retrying with native git");
                    push_with_git_cli(&self.repo_path, url, &sha).with_context(|| {
                        format!(
                            "push grokrxiv-data commit {sha} to configured remote {url} using native git fallback"
                        )
                    })?;
                    info!(%sha, %url, "pushed grokrxiv-data with native git fallback");
                }
                Err(err) => {
                    return Err(anyhow::Error::new(err)).with_context(|| {
                        format!("push grokrxiv-data commit {sha} to configured remote {url}")
                    });
                }
            }
        }

        Ok(sha)
    }
}

fn validate_artifact_arxiv_id(arxiv_id: &str) -> Result<()> {
    if arxiv_id.is_empty()
        || arxiv_id.starts_with('.')
        || arxiv_id.contains("..")
        || arxiv_id.contains('/')
        || arxiv_id.contains('\\')
        || !arxiv_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(anyhow!(
            "unsafe arxiv id `{arxiv_id}` for git artifact path"
        ));
    }
    Ok(())
}

fn validate_artifact_rel_path(rel_path: &str) -> Result<PathBuf> {
    let path = Path::new(rel_path);
    if rel_path.is_empty() {
        return Err(anyhow!("unsafe artifact path `{rel_path}`: empty path"));
    }

    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part == ".git" {
                    return Err(anyhow!(
                        "unsafe artifact path `{rel_path}`: .git component is not allowed"
                    ));
                }
                safe.push(part);
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(anyhow!(
                    "unsafe artifact path `{rel_path}`: must be relative and stay inside paper directory"
                ));
            }
        }
    }

    if safe.as_os_str().is_empty() {
        return Err(anyhow!("unsafe artifact path `{rel_path}`: empty path"));
    }
    Ok(safe)
}

fn signature(repo: &Repository) -> Result<Signature<'static>> {
    if let Ok(cfg) = repo.config() {
        let name = cfg.get_string("user.name").ok();
        let email = cfg.get_string("user.email").ok();
        if let (Some(n), Some(e)) = (name, email) {
            return Ok(Signature::now(&n, &e)?);
        }
    }
    Ok(Signature::now("grokrxiv-bot", "bot@grokrxiv.local")?)
}

fn default_credentials(
    url: &str,
    username_from_url: Option<&str>,
    allowed_types: git2::CredentialType,
) -> std::result::Result<Cred, git2::Error> {
    if allowed_types.contains(git2::CredentialType::SSH_KEY) {
        let user = username_from_url.unwrap_or("git");
        return Cred::ssh_key_from_agent(user);
    }
    if allowed_types.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            return Cred::userpass_plaintext("x-access-token", &token);
        }
    }
    Err(git2::Error::from_str(&format!(
        "no credentials available for {url}"
    )))
}

fn should_retry_push_with_git_cli(err: &git2::Error, url: &str) -> bool {
    let message = err.message().to_ascii_lowercase();
    message.contains("unsupported url protocol") && is_ssh_remote_url(url)
}

fn is_ssh_remote_url(url: &str) -> bool {
    url.starts_with("ssh://") || (url.contains('@') && url.contains(':') && !url.contains("://"))
}

fn push_with_git_cli(repo_path: &Path, remote_url: &str, sha: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("push")
        .arg(remote_url)
        .arg("refs/heads/main:refs/heads/main")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("spawning native git push for {}", repo_path.display()))?;

    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut details = Vec::new();
    if !stdout.trim().is_empty() {
        details.push(format!("stdout: {}", stdout.trim()));
    }
    if !stderr.trim().is_empty() {
        details.push(format!("stderr: {}", stderr.trim()));
    }
    if details.is_empty() {
        details.push("native git returned no output".to_string());
    }

    Err(anyhow!(
        "native git push failed for grokrxiv-data commit {sha} with status {}: {}",
        output.status,
        details.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_bare_remote(dir: &Path) -> Result<()> {
        let repo = Repository::init_bare(dir)?;
        // Ensure the bare repo's symbolic HEAD points at main, not master,
        // so libgit2 clone selects main as the default branch.
        repo.set_head("refs/heads/main")?;
        Ok(())
    }

    #[test]
    fn open_or_clone_then_write() -> Result<()> {
        // Set up bare repo as a "remote".
        let remote_dir = TempDir::new()?;
        init_bare_remote(remote_dir.path())?;
        let remote_url = format!("file://{}", remote_dir.path().display());

        // Seed the bare remote with an initial commit on `main`, so cloning
        // finds a HEAD. We do that by creating a workdir, committing schemas,
        // then pushing.
        let seed_dir = TempDir::new()?;
        let seed_path = seed_dir.path().to_path_buf();
        let repo = Repository::init(&seed_path).context("seed init")?;
        let schemas_dir = seed_path.join("schemas");
        fs::create_dir_all(&schemas_dir)?;
        let schema = serde_json::json!({
            "$id": "metadata.schema.json",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Metadata",
            "type": "object",
            "additionalProperties": false,
            "required": ["arxiv_id"],
            "properties": {
                "arxiv_id": { "type": "string" }
            }
        });
        fs::write(
            schemas_dir.join("metadata.schema.json"),
            serde_json::to_vec_pretty(&schema)?,
        )?;
        let sig = Signature::now("seed", "seed@test")?;
        let mut index = repo.index()?;
        index
            .add_all(["."].iter(), IndexAddOption::DEFAULT, None)
            .context("seed add_all")?;
        index.write().context("seed index write")?;
        let tree_id = index.write_tree().context("seed write_tree")?;
        let tree = repo.find_tree(tree_id)?;
        let commit_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
            .context("seed commit")?;
        let seed_commit = repo.find_commit(commit_oid)?;
        repo.branch("main", &seed_commit, true)
            .context("seed branch")?;
        repo.set_head("refs/heads/main").context("seed set_head")?;
        let mut origin = repo.remote("origin", &remote_url).context("seed remote")?;
        origin
            .push(&["refs/heads/main:refs/heads/main"], None)
            .context("seed push")?;

        // Clone to a fresh path.
        let work_dir = TempDir::new()?;
        let work_path = work_dir.path().join("grokrxiv-data");
        let store = GitArtifactStore::open_or_clone(work_path.clone(), Some(remote_url.clone()))
            .context("first clone")?;

        // Write a paper directory with a valid metadata.json.
        let mut files = HashMap::new();
        files.insert(
            "metadata.json".to_string(),
            serde_json::to_vec(&serde_json::json!({ "arxiv_id": "2605.00403" }))?,
        );
        files.insert("body.md".to_string(), b"# title\n".to_vec());
        store
            .write_paper_artifacts("2605.00403", files)
            .context("write_paper_artifacts")?;
        let sha = store
            .commit_and_push("2605.00403", &["stage1", "stage2"])
            .context("commit_and_push")?;
        assert_eq!(sha.len(), 40);

        // Re-clone and assert content.
        let work2 = TempDir::new()?;
        let work2_path = work2.path().join("grokrxiv-data");
        let _ = GitArtifactStore::open_or_clone(work2_path.clone(), Some(remote_url))
            .context("re-clone")?;
        let body = fs::read_to_string(work2_path.join("papers/2605.00403/body.md"))
            .context("read re-cloned body.md")?;
        assert!(body.contains("# title"));
        Ok(())
    }

    #[test]
    fn invalid_json_fails_validation() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().to_path_buf();
        let store = GitArtifactStore::open_or_clone(path.clone(), None)?;
        fs::create_dir_all(path.join("schemas"))?;
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["arxiv_id"],
            "properties": { "arxiv_id": { "type": "string" } },
            "additionalProperties": false
        });
        fs::write(
            path.join("schemas/metadata.schema.json"),
            serde_json::to_vec(&schema)?,
        )?;

        let mut files = HashMap::new();
        files.insert("metadata.json".to_string(), b"{}".to_vec());
        let err = store.write_paper_artifacts("XX", files).unwrap_err();
        assert!(format!("{err:#}").contains("validation failed"));
        Ok(())
    }

    #[test]
    fn write_paper_artifacts_rejects_unsafe_arxiv_id() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().to_path_buf();
        let store = GitArtifactStore::open_or_clone(path.clone(), None)?;

        let mut files = HashMap::new();
        files.insert("body.md".to_string(), b"# escaped\n".to_vec());
        let err = store
            .write_paper_artifacts("../escaped", files)
            .expect_err("unsafe arxiv id should be rejected");

        assert!(format!("{err:#}").contains("unsafe arxiv id"));
        assert!(!path.join("escaped/body.md").exists());
        Ok(())
    }

    #[test]
    fn write_paper_artifacts_rejects_escaping_relative_path() -> Result<()> {
        let dir = TempDir::new()?;
        let path = dir.path().to_path_buf();
        let store = GitArtifactStore::open_or_clone(path.clone(), None)?;

        let mut files = HashMap::new();
        files.insert("../outside.md".to_string(), b"# escaped\n".to_vec());
        let err = store
            .write_paper_artifacts("2605.00403", files)
            .expect_err("escaping rel path should be rejected");

        assert!(format!("{err:#}").contains("unsafe artifact path"));
        assert!(!path.join("papers/outside.md").exists());
        Ok(())
    }

    #[test]
    fn commit_and_push_rejects_unsafe_arxiv_id() -> Result<()> {
        let dir = TempDir::new()?;
        let store = GitArtifactStore::open_or_clone(dir.path().to_path_buf(), None)?;

        let err = store
            .commit_and_push("../escaped", &["review"])
            .expect_err("unsafe arxiv id should be rejected before staging");

        assert!(format!("{err:#}").contains("unsafe arxiv id"));
        Ok(())
    }

    #[test]
    fn ssh_remote_protocol_errors_use_native_git_fallback() {
        let err = git2::Error::from_str("unsupported URL protocol");

        assert!(should_retry_push_with_git_cli(
            &err,
            "git@github.com:GrokRxiv/grokrxiv-data.git"
        ));
        assert!(should_retry_push_with_git_cli(
            &err,
            "ssh://git@github.com/GrokRxiv/grokrxiv-data.git"
        ));
        assert!(!should_retry_push_with_git_cli(
            &err,
            "file:///tmp/grokrxiv-data.git"
        ));
    }

    #[test]
    fn non_protocol_push_errors_do_not_use_native_git_fallback() {
        let err = git2::Error::from_str("authentication required");

        assert!(!should_retry_push_with_git_cli(
            &err,
            "git@github.com:GrokRxiv/grokrxiv-data.git"
        ));
    }
}
