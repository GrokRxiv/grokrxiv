//! Pluggable review-source preparation for non-arXiv paper inputs.
//!
//! The legacy arXiv ingest path remains [`crate::pipeline::ingest_staged`].
//! This module prepares the same review-facing `PaperExtract` shape from local
//! files and git repositories that contain a PDF or TeX manuscript.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::process::Command;

use crate::extract::{
    extract_bibliography, infer_pdf_title, normalize_pdf_text, pdf_to_text, split_sections,
};
use crate::pipeline::ingest_staged;
use crate::tex::parse_bundle;
use crate::types::{Author, PaperExtract};

/// Supported source families for review ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Canonical arXiv input.
    Arxiv,
    /// Local PDF or TeX manuscript file.
    LocalFile,
    /// Git repository containing a PDF or TeX manuscript.
    GitRepo,
}

/// Supported local manuscript formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSourceFormat {
    /// Portable Document Format.
    Pdf,
    /// LaTeX source.
    Tex,
}

impl LocalSourceFormat {
    /// Detect a supported source format from a path extension.
    pub fn from_path(path: &Path) -> Result<Self> {
        match path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
        {
            Some(ext) if ext == "pdf" => Ok(Self::Pdf),
            Some(ext) if ext == "tex" => Ok(Self::Tex),
            Some(ext) => {
                bail!("unsupported source file extension .{ext}; only .pdf and .tex are supported")
            }
            None => bail!("source file has no extension; only .pdf and .tex are supported"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Tex => "tex",
        }
    }
}

/// User-supplied review source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSourceSpec {
    /// Existing arXiv pipeline input.
    Arxiv {
        /// arXiv id, with or without version suffix.
        id: String,
    },
    /// Local PDF or TeX manuscript.
    LocalFile {
        /// Path to the local manuscript.
        path: PathBuf,
        /// Explicit format. When absent, the extension must be `.pdf` or `.tex`.
        #[serde(default)]
        format: Option<LocalSourceFormat>,
        /// Optional title override for local inputs.
        #[serde(default)]
        title: Option<String>,
        /// Optional author metadata for local inputs.
        #[serde(default)]
        authors: Vec<Author>,
        /// Optional field/category metadata.
        #[serde(default)]
        field: Option<String>,
    },
    /// Git repository containing a PDF or TeX manuscript.
    GitRepo {
        /// Git remote URL or local repository path.
        repo: String,
        /// Optional revision to check out.
        #[serde(default)]
        rev: Option<String>,
        /// Optional explicit manuscript path inside the repository.
        #[serde(default)]
        paper_path: Option<PathBuf>,
        /// Optional title override.
        #[serde(default)]
        title: Option<String>,
        /// Optional author metadata.
        #[serde(default)]
        authors: Vec<Author>,
        /// Optional field/category metadata.
        #[serde(default)]
        field: Option<String>,
    },
}

/// Stable identity for a prepared review source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Source family.
    pub source_kind: SourceKind,
    /// Stable local id for this exact manuscript content.
    pub source_id: String,
    /// Human-readable label for logs and UI.
    pub display_label: String,
    /// Canonical URI for the source when available.
    pub canonical_uri: String,
    /// Full sha256 hex digest of the manuscript bytes.
    pub content_hash: String,
    /// arXiv id for arXiv inputs.
    pub arxiv_id: Option<String>,
}

/// Fully prepared deterministic source payload for review.
#[derive(Debug, Clone)]
pub struct PreparedReviewSource {
    /// Stable source identity.
    pub identity: SourceIdentity,
    /// Review-facing paper extraction.
    pub extract: PaperExtract,
    /// PDF bytes when the source included a PDF artifact.
    pub pdf_bytes: Option<Bytes>,
    /// Source bytes when the source included TeX.
    pub source_tarball: Option<Bytes>,
    /// Optional semantic AST emitted by TeX processing.
    pub semantic_ast: Option<Value>,
    /// Source-specific acquisition metadata.
    pub source_metadata: Value,
}

/// Prepare any supported review source.
pub async fn prepare_review_source(spec: ReviewSourceSpec) -> Result<PreparedReviewSource> {
    match spec {
        ReviewSourceSpec::Arxiv { id } => prepare_arxiv_source(&id).await,
        ReviewSourceSpec::LocalFile {
            path,
            format,
            title,
            authors,
            field,
        } => prepare_local_file_source(&path, format, title, authors, field).await,
        ReviewSourceSpec::GitRepo {
            repo,
            rev,
            paper_path,
            title,
            authors,
            field,
        } => {
            prepare_git_repo_source(
                &repo,
                rev.as_deref(),
                paper_path.as_deref(),
                title,
                authors,
                field,
            )
            .await
        }
    }
}

/// Prepare a local PDF or TeX manuscript.
pub async fn prepare_local_file_source(
    path: &Path,
    format: Option<LocalSourceFormat>,
    title: Option<String>,
    authors: Vec<Author>,
    field: Option<String>,
) -> Result<PreparedReviewSource> {
    let format = explicit_or_detect_format(path, format)?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("read local source file {}", path.display()))?;
    let content = Bytes::from(bytes);
    let label = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("local manuscript")
        .to_string();
    let canonical_uri = format!(
        "file://{}",
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
    );
    prepare_bytes_source(
        SourceKind::LocalFile,
        &format!("local-{}", format.as_str()),
        canonical_uri,
        label,
        format,
        content,
        title,
        authors,
        field,
        json!({ "path": path.display().to_string(), "format": format.as_str() }),
    )
    .await
}

/// Clone a git repository into a tempdir and prepare its PDF or TeX manuscript.
pub async fn prepare_git_repo_source(
    repo: &str,
    rev: Option<&str>,
    paper_path: Option<&Path>,
    title: Option<String>,
    authors: Vec<Author>,
    field: Option<String>,
) -> Result<PreparedReviewSource> {
    let tmp = TempDir::new().context("create temp dir for git source")?;
    let checkout = tmp.path().join("repo");

    run_git(&["clone", "--quiet", repo, path_str(&checkout)?], None)
        .await
        .with_context(|| format!("clone git source {repo}"))?;
    if let Some(rev) = rev {
        run_git(&["checkout", "--quiet", rev], Some(&checkout))
            .await
            .with_context(|| format!("checkout git revision {rev}"))?;
    }

    let resolved_commit = git_output(&["rev-parse", "HEAD"], &checkout)
        .await
        .context("resolve git commit")?;
    let selected = select_git_manuscript(&checkout, paper_path)?;
    let rel_path = selected
        .strip_prefix(&checkout)
        .unwrap_or(&selected)
        .to_path_buf();
    let format = LocalSourceFormat::from_path(&selected)?;
    let content = Bytes::from(
        tokio::fs::read(&selected)
            .await
            .with_context(|| format!("read git manuscript {}", rel_path.display()))?,
    );
    let manuscript_hash = sha256_hex(&content);
    let short = short_hash(&manuscript_hash);
    let canonical_uri = format!(
        "git+{repo}@{}:{}",
        resolved_commit.trim(),
        rel_path.display()
    );
    let label = format!("{}:{}", repo, rel_path.display());
    let id_prefix = format!("git-{}", format.as_str());

    prepare_bytes_source(
        SourceKind::GitRepo,
        &id_prefix,
        canonical_uri,
        label,
        format,
        content,
        title,
        authors,
        field,
        json!({
            "repo": repo,
            "rev": rev,
            "resolved_commit": resolved_commit.trim(),
            "paper_path": rel_path.display().to_string(),
            "format": format.as_str(),
            "manuscript_hash": manuscript_hash,
            "source_id_hint": format!("{id_prefix}-{short}")
        }),
    )
    .await
}

async fn prepare_arxiv_source(arxiv_id: &str) -> Result<PreparedReviewSource> {
    let staged = ingest_staged(arxiv_id).await?;
    let content_bytes = staged
        .source_tarball
        .as_ref()
        .or(staged.pdf_bytes.as_ref())
        .map(|b| b.as_ref())
        .unwrap_or_else(|| arxiv_id.as_bytes());
    let content_hash = sha256_hex(content_bytes);
    let identity = SourceIdentity {
        source_kind: SourceKind::Arxiv,
        source_id: arxiv_id.to_string(),
        display_label: staged.meta.title.clone(),
        canonical_uri: format!("https://arxiv.org/abs/{arxiv_id}"),
        content_hash,
        arxiv_id: Some(staged.meta.arxiv_id.clone()),
    };
    Ok(PreparedReviewSource {
        identity,
        extract: staged.extract,
        pdf_bytes: staged.pdf_bytes,
        source_tarball: staged.source_tarball,
        semantic_ast: staged.semantic_ast,
        source_metadata: serde_json::to_value(staged.meta).unwrap_or_else(|_| json!({})),
    })
}

async fn prepare_bytes_source(
    source_kind: SourceKind,
    id_prefix: &str,
    canonical_uri: String,
    display_label: String,
    format: LocalSourceFormat,
    bytes: Bytes,
    title: Option<String>,
    authors: Vec<Author>,
    field: Option<String>,
    source_metadata: Value,
) -> Result<PreparedReviewSource> {
    let content_hash = sha256_hex(&bytes);
    let source_id = format!("{id_prefix}-{}", short_hash(&content_hash));
    let identity = SourceIdentity {
        source_kind,
        source_id: source_id.clone(),
        display_label,
        canonical_uri,
        content_hash,
        arxiv_id: None,
    };

    let (extract, pdf_bytes, source_tarball, semantic_ast) = match format {
        LocalSourceFormat::Pdf => {
            let text = pdf_to_text(&bytes).context("extract text from local pdf source")?;
            let normalized = normalize_pdf_text(&text);
            let sections = split_sections(&normalized.text);
            let bibliography = extract_bibliography(&normalized.text);
            let resolved_title = title
                .or_else(|| infer_pdf_title(&normalized.text))
                .unwrap_or_else(|| identity.display_label.clone());
            let extract = PaperExtract {
                arxiv_id: source_id.clone(),
                title: resolved_title,
                authors,
                abstract_: String::new(),
                field,
                sections,
                figures: Vec::new(),
                bibliography,
                source_format: Some("pdf".to_string()),
            };
            (extract, Some(bytes), None, None)
        }
        LocalSourceFormat::Tex => {
            let tex = parse_bundle(&bytes)
                .await
                .context("parse local tex source")?;
            let extract = PaperExtract {
                arxiv_id: source_id.clone(),
                title: title.unwrap_or(tex.title),
                authors,
                abstract_: tex.abstract_text,
                field,
                sections: tex.sections,
                figures: Vec::new(),
                bibliography: tex.bibliography,
                source_format: Some("tex".to_string()),
            };
            (extract, None, Some(bytes), tex.semantic_ast)
        }
    };

    Ok(PreparedReviewSource {
        identity,
        extract,
        pdf_bytes,
        source_tarball,
        semantic_ast,
        source_metadata,
    })
}

fn explicit_or_detect_format(
    path: &Path,
    format: Option<LocalSourceFormat>,
) -> Result<LocalSourceFormat> {
    let detected = LocalSourceFormat::from_path(path)?;
    if let Some(format) = format {
        if format != detected {
            bail!(
                "explicit source format {} does not match path extension for {}",
                format.as_str(),
                path.display()
            );
        }
        Ok(format)
    } else {
        Ok(detected)
    }
}

fn select_git_manuscript(repo_root: &Path, paper_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = paper_path {
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("git paper_path must be a relative path inside the repository");
        }
        let full = repo_root.join(path);
        if !full.is_file() {
            bail!(
                "git paper_path {} does not exist or is not a file",
                path.display()
            );
        }
        LocalSourceFormat::from_path(&full)?;
        return Ok(full);
    }

    let mut tex = Vec::new();
    let mut pdf = Vec::new();
    collect_candidates(repo_root, repo_root, &mut tex, &mut pdf)?;
    let manuscript_tex: Vec<PathBuf> = tex
        .into_iter()
        .filter(|p| looks_like_main_tex(p).unwrap_or(false))
        .collect();
    match (manuscript_tex.len(), pdf.len()) {
        (1, _) => Ok(manuscript_tex
            .into_iter()
            .next()
            .expect("one tex candidate")),
        (0, 1) => Ok(pdf.into_iter().next().expect("one pdf candidate")),
        (0, 0) => bail!("git source contains no .tex or .pdf manuscript"),
        (n, _) if n > 1 => {
            bail!(
                "git source contains multiple plausible .tex manuscripts; pass --paper-path with one of: {}",
                format_relative_candidates(repo_root, &manuscript_tex)
            )
        }
        _ => bail!(
            "git source contains multiple .pdf files and no main .tex; pass --paper-path with one of: {}",
            format_relative_candidates(repo_root, &pdf)
        ),
    }
}

fn collect_candidates(
    root: &Path,
    dir: &Path,
    tex: &mut Vec<PathBuf>,
    pdf: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("read git source dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == ".git" || file_name == "target" {
            continue;
        }
        if path.is_dir() {
            collect_candidates(root, &path, tex, pdf)?;
            continue;
        }
        match LocalSourceFormat::from_path(&path) {
            Ok(LocalSourceFormat::Tex) => tex.push(path),
            Ok(LocalSourceFormat::Pdf) => pdf.push(path),
            Err(_) => {}
        }
    }
    tex.sort_by_key(|p| p.strip_prefix(root).unwrap_or(p).to_path_buf());
    pdf.sort_by_key(|p| p.strip_prefix(root).unwrap_or(p).to_path_buf());
    Ok(())
}

fn looks_like_main_tex(path: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read tex candidate {}", path.display()))?;
    Ok(text.contains("\\documentclass") && text.contains("\\begin{document}"))
}

fn format_relative_candidates(root: &Path, candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn run_git(args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd.output().await.context("run git command")?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn git_output(args: &[&str], cwd: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .context("run git command")?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn short_hash(hash: &str) -> &str {
    &hash[..12]
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_local_formats() {
        assert_eq!(
            LocalSourceFormat::from_path(Path::new("paper.pdf")).unwrap(),
            LocalSourceFormat::Pdf
        );
        assert_eq!(
            LocalSourceFormat::from_path(Path::new("paper.TeX")).unwrap(),
            LocalSourceFormat::Tex
        );
    }

    #[test]
    fn rejects_unsupported_local_formats() {
        let err = LocalSourceFormat::from_path(Path::new("paper.docx")).unwrap_err();
        assert!(err.to_string().contains("only .pdf and .tex"));
    }

    #[test]
    fn stable_local_source_ids_are_content_hash_based() {
        let bytes =
            Bytes::from_static(b"\\documentclass{article}\\begin{document}Hello\\end{document}");
        let hash = sha256_hex(&bytes);
        let source_id = format!("local-tex-{}", short_hash(&hash));
        assert_eq!(
            source_id,
            format!("local-tex-{}", short_hash(&sha256_hex(&bytes)))
        );
        assert_eq!(source_id.len(), "local-tex-".len() + 12);
    }

    #[test]
    fn explicit_format_must_match_extension() {
        let err = explicit_or_detect_format(Path::new("paper.pdf"), Some(LocalSourceFormat::Tex))
            .unwrap_err();
        assert!(err.to_string().contains("does not match path extension"));
    }

    #[test]
    fn source_enums_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&SourceKind::LocalFile).unwrap(),
            "\"local_file\""
        );
        assert_eq!(
            serde_json::to_string(&LocalSourceFormat::Pdf).unwrap(),
            "\"pdf\""
        );
    }

    #[test]
    fn ambiguous_git_tex_error_lists_paper_path_candidates() {
        let tmp = TempDir::new().unwrap();
        let sources = tmp.path().join("sources");
        std::fs::create_dir(&sources).unwrap();
        std::fs::write(
            tmp.path().join("main.tex"),
            "\\documentclass{article}\\begin{document}Main\\end{document}",
        )
        .unwrap();
        std::fs::write(
            sources.join("paper.tex"),
            "\\documentclass{article}\\begin{document}Paper\\end{document}",
        )
        .unwrap();

        let err = select_git_manuscript(tmp.path(), None).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("pass --paper-path"));
        assert!(message.contains("main.tex"));
        assert!(message.contains("sources/paper.tex"));
    }

    #[test]
    fn ambiguous_git_pdf_error_lists_paper_path_candidates() {
        let tmp = TempDir::new().unwrap();
        let sources = tmp.path().join("sources");
        std::fs::create_dir(&sources).unwrap();
        std::fs::write(tmp.path().join("first.pdf"), b"%PDF-1.7").unwrap();
        std::fs::write(sources.join("second.pdf"), b"%PDF-1.7").unwrap();

        let err = select_git_manuscript(tmp.path(), None).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("pass --paper-path"));
        assert!(message.contains("first.pdf"));
        assert!(message.contains("sources/second.pdf"));
    }
}
