//! YAML-backed agent configuration.
//!
//! Agent YAML files are the contract source for role identity, execution mode,
//! prompt/schema paths, verifier names, tool names, retry budgets, and timeouts.
//! Rust code must not encode per-agent behavior that belongs in these configs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use agenthero_dag_runtime::{AgentKind, DagExecutionMode, DagManifest, DagNodeKind};

use crate::agents::types::AgentRunnerKind;

/// YAML shape read from `agenthero/apps/<app>/agents/<dag-type>/<role-id>.yaml`.
#[derive(Debug, serde::Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Human-readable/local id. The DAG manifest role id is still authoritative.
    #[serde(default)]
    pub id: Option<String>,
    /// Capability kind. When present, it must match the manifest role kind.
    #[serde(default)]
    pub kind: Option<AgentKind>,
    /// Natural-language role description for prompt authors and agent UIs.
    #[serde(default)]
    pub role: Option<String>,
    /// Provider tag such as `claude`, `openai`, `gemini`, or `vllm`.
    pub provider: String,
    /// Provider model id.
    pub model: String,
    /// Runner backend for this agent.
    #[serde(default)]
    pub runner: Option<AgentRunnerKind>,
    /// One-shot JSON output or tool-loop execution.
    #[serde(default)]
    pub execution_mode: DagExecutionMode,
    /// Prompt template path, relative to the app root unless absolute.
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Input schema path, relative to the app root unless absolute.
    #[serde(default)]
    pub input_schema: Option<String>,
    /// Output schema path, relative to the app root unless absolute.
    #[serde(default)]
    pub output_schema: Option<String>,
    /// Named verifier ladder entries.
    #[serde(default)]
    pub verifiers: Vec<String>,
    /// Named callable tools for tool-loop agents.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Prompt input blocks and budgets. These select reusable Rust renderers
    /// by name instead of encoding behavior by role id.
    #[serde(default)]
    pub prompt_context: AgentPromptContext,
    /// Named system prompt overlays rendered by the DAG app.
    #[serde(default)]
    pub system_overlays: Vec<String>,
    /// Named output postprocessors applied after verifier execution.
    #[serde(default)]
    pub postprocessors: Vec<String>,
    /// Tool-loop iteration cap.
    #[serde(default)]
    pub max_iters: Option<u32>,
    /// Tool-loop cost cap.
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    /// Corrective retry count.
    #[serde(default)]
    pub max_retries: Option<u8>,
    /// Single-run timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u32>,
    /// Escalation policy name.
    #[serde(default)]
    pub escalation: Option<String>,
}

/// Declarative prompt-context selection for one agent.
#[derive(Debug, serde::Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentPromptContext {
    /// Character budget for the paper body block.
    #[serde(default)]
    pub body_budget_chars: Option<usize>,
    /// Bibliography rendering mode.
    #[serde(default)]
    pub bibliography: BibliographyMode,
    /// Max bibliography entries when `bibliography=limited`.
    #[serde(default)]
    pub max_bibliography_entries: Option<usize>,
    /// Character budget for extracted citation context sentences.
    #[serde(default)]
    pub citation_context_budget_chars: Option<usize>,
    /// Named deterministic fact blocks to inject.
    #[serde(default)]
    pub fact_blocks: Vec<String>,
    /// Whether this agent consumes the synthesized specialist bundle instead
    /// of the paper extract.
    #[serde(default)]
    pub meta_input: bool,
}

/// How to render bibliography data into a prompt.
#[derive(Debug, serde::Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BibliographyMode {
    /// Do not include bibliography text.
    None,
    /// Include every bibliography entry.
    #[default]
    Full,
    /// Include a bounded prefix; full data remains in artifacts/verifiers.
    Limited,
}

/// Reference to one manifest-declared agent config.
#[derive(Debug, Clone)]
pub struct AgentConfigRef {
    /// DAG-scoped role id.
    pub role_id: String,
    /// Kind declared in the manifest.
    pub expected_kind: AgentKind,
    /// Original config path as written in the manifest.
    pub label: String,
    /// Resolved config path.
    pub path: PathBuf,
}

/// Load all role config refs declared by one DAG manifest.
pub fn dag_agent_config_refs(dag_id: &str) -> anyhow::Result<Vec<AgentConfigRef>> {
    let manifest_path = dag_manifest_path(dag_id);
    let repo_root = repo_root_for_manifest_path(&manifest_path);
    let manifest = read_dag_manifest(dag_id)?;
    if manifest.id.as_str() != dag_id {
        anyhow::bail!("expected DAG id `{dag_id}`, found `{}`", manifest.id);
    }

    let mut refs = Vec::new();
    for role in manifest.roles {
        let Some(config) = role.config else {
            anyhow::bail!("manifest role `{}` has no config path", role.id);
        };
        let path = resolve_agent_config_path(&repo_root, &config);
        refs.push(AgentConfigRef {
            role_id: role.id.to_string(),
            expected_kind: role.kind,
            label: config,
            path,
        });
    }
    Ok(refs)
}

/// Read one DAG manifest by id from its owning app root.
pub fn read_dag_manifest(dag_id: &str) -> anyhow::Result<DagManifest> {
    let manifest_path = dag_manifest_path(dag_id);
    let manifest = DagManifest::from_path(&manifest_path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", manifest_path.display()))?;
    if manifest.id.as_str() != dag_id {
        anyhow::bail!("expected DAG id `{dag_id}`, found `{}`", manifest.id);
    }
    Ok(manifest)
}

/// Roles whose agent nodes are declared as inputs to the synthesis/meta layer.
pub fn dag_feeds_meta_roles(dag_id: &str) -> anyhow::Result<Vec<String>> {
    let manifest = read_dag_manifest(dag_id)?;
    Ok(manifest
        .nodes
        .into_iter()
        .filter(|node| node.kind == DagNodeKind::Agent && node.feeds_meta)
        .filter_map(|node| node.role.map(|role| role.to_string()))
        .collect())
}

/// Roles whose agent config consumes synthesized specialist/meta input.
pub fn dag_meta_input_roles(dag_id: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for config_ref in dag_agent_config_refs(dag_id)? {
        let cfg = read_agent_config(&config_ref.path)?;
        if cfg.prompt_context.meta_input {
            out.push(config_ref.role_id);
        }
    }
    Ok(out)
}

/// Roles that opt into a named output postprocessor in YAML.
pub fn dag_roles_with_postprocessor(
    dag_id: &str,
    postprocessor: &str,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for config_ref in dag_agent_config_refs(dag_id)? {
        let cfg = read_agent_config(&config_ref.path)?;
        if cfg
            .postprocessors
            .iter()
            .any(|configured| configured == postprocessor)
        {
            out.push(config_ref.role_id);
        }
    }
    Ok(out)
}

/// Read and parse one agent YAML config.
pub fn read_agent_config(path: &Path) -> anyhow::Result<AgentConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read agent YAML {}: {e}", path.display()))?;
    serde_yaml::from_str::<AgentConfig>(&text)
        .map_err(|e| anyhow::anyhow!("parse agent YAML {}: {e}", path.display()))
}

/// Validate one parsed agent config against its manifest-declared shape.
pub fn validate_agent_config_detail(
    label: &str,
    expected_kind: &AgentKind,
    cfg: &AgentConfig,
) -> anyhow::Result<()> {
    if let Some(kind) = &cfg.kind {
        if kind != expected_kind {
            anyhow::bail!(
                "agent role YAML for {label} declares kind={}, but DAG expects kind={}",
                kind,
                expected_kind
            );
        }
    }
    for (field, declared_path) in [
        ("prompt_template", cfg.prompt_template.as_deref()),
        ("input_schema", cfg.input_schema.as_deref()),
        ("output_schema", cfg.output_schema.as_deref()),
    ] {
        let Some(declared_path) = declared_path else {
            anyhow::bail!("agent role YAML for {label} is missing `{field}`");
        };
        let path = resolve_declared_runtime_path(declared_path);
        if !path.exists() {
            anyhow::bail!(
                "agent role YAML for {label} declares {field}={}, but {} does not exist",
                declared_path,
                path.display()
            );
        }
    }
    if cfg.execution_mode == DagExecutionMode::ToolLoop {
        if cfg.max_iters.unwrap_or(0) == 0 {
            anyhow::bail!(
                "agent role YAML for {label} uses execution_mode=tool_loop but max_iters is missing or zero"
            );
        }
        match cfg.max_cost_usd {
            Some(cost) if cost > 0.0 => {}
            _ => anyhow::bail!(
                "agent role YAML for {label} uses execution_mode=tool_loop but max_cost_usd is missing or non-positive"
            ),
        }
        if cfg.tools.is_empty() {
            anyhow::bail!(
                "agent role YAML for {label} uses execution_mode=tool_loop but declares no tools"
            );
        }
    }
    validate_unique_names(label, "verifiers", &cfg.verifiers)?;
    validate_unique_names(label, "tools", &cfg.tools)?;
    validate_unique_names(label, "fact_blocks", &cfg.prompt_context.fact_blocks)?;
    validate_unique_names(label, "system_overlays", &cfg.system_overlays)?;
    validate_unique_names(label, "postprocessors", &cfg.postprocessors)?;
    validate_known_names(
        label,
        "fact_blocks",
        &cfg.prompt_context.fact_blocks,
        &[
            "reproducibility_availability",
            "novelty_prior_art",
            "technical_structure",
        ],
    )?;
    validate_known_names(
        label,
        "system_overlays",
        &cfg.system_overlays,
        &[
            "proof_as_code_technical",
            "proof_as_code_reproducibility",
            "meta_recommendation_gate",
        ],
    )?;
    validate_known_names(
        label,
        "postprocessors",
        &cfg.postprocessors,
        &[
            "merge_citation_verifier",
            "merge_reproducibility_facts",
            "merge_novelty_facts",
        ],
    )?;
    if let Some(escalation) = cfg.escalation.as_deref() {
        match escalation {
            "skip" | "agent" | "retry" => {}
            other => anyhow::bail!(
                "agent role YAML for {label} declares unsupported escalation `{other}`"
            ),
        }
    }
    Ok(())
}

/// Resolve a manifest/config-declared runtime path relative to installed app roots.
pub fn resolve_declared_runtime_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        let repo_relative = default_repo_root().join(&path);
        if repo_relative.exists() {
            return repo_relative;
        }
        find_app_relative_path(&path).unwrap_or(repo_relative)
    }
}

/// Resolve an agent config path relative to the owning app root.
pub fn resolve_agent_config_path(repo_root: &Path, config: &str) -> PathBuf {
    let path = PathBuf::from(config);
    if path.is_absolute() {
        return path;
    }
    if let Some(agents_dir) = std::env::var_os("AGENTHERO_AGENTS_DIR").map(PathBuf::from) {
        if let Ok(stripped) = path.strip_prefix("agents") {
            let candidate = agents_dir.join(stripped);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    repo_root.join(path)
}

/// Repo root inferred from this crate location.
pub fn default_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Default checked-in `agents/` directory.
pub fn default_agents_dir() -> PathBuf {
    crate::dag_apps::app_root("grokrxiv").join("agents")
}

/// Resolve one DAG manifest path.
pub fn dag_manifest_path(dag_id: &str) -> PathBuf {
    if let Some(dags_dir) = std::env::var_os("AGENTHERO_DAGS_DIR").map(PathBuf::from) {
        return dags_dir.join(format!("{dag_id}.yaml"));
    }
    if let Some(descriptor) = crate::dag_apps::registered_dag_app(dag_id) {
        return crate::dag_apps::app_root(&descriptor.product_app)
            .join("dags")
            .join(format!("{dag_id}.yaml"));
    }
    find_app_relative_path(&PathBuf::from(format!("dags/{dag_id}.yaml"))).unwrap_or_else(|| {
        crate::dag_apps::apps_root()
            .join("unknown")
            .join("dags")
            .join(format!("{dag_id}.yaml"))
    })
}

fn repo_root_for_manifest_path(manifest_path: &Path) -> PathBuf {
    manifest_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(default_repo_root)
}

fn find_app_relative_path(path: &Path) -> Option<PathBuf> {
    let apps_root = crate::dag_apps::apps_root();
    let entries = std::fs::read_dir(apps_root).ok()?;
    let mut matches = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_dir())
        .map(|app_root| app_root.join(path))
        .filter(|candidate| candidate.exists())
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}

fn validate_unique_names(label: &str, field: &str, names: &[String]) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            anyhow::bail!("agent role YAML for {label} declares duplicate {field} entry `{name}`");
        }
    }
    Ok(())
}

fn validate_known_names(
    label: &str,
    field: &str,
    names: &[String],
    allowed: &[&str],
) -> anyhow::Result<()> {
    for name in names {
        if !allowed.contains(&name.as_str()) {
            anyhow::bail!("agent role YAML for {label} declares unknown {field} entry `{name}`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct EnvVarGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn clear(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    struct CurrentDirGuard {
        prev: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: &Path) -> Self {
            let prev = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self { prev }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.prev).expect("restore current dir");
        }
    }

    fn workspace_root_for_test() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|candidate| candidate.join("agenthero/apps/grokrxiv/app.yaml").is_file())
            .expect("workspace root")
            .to_path_buf()
    }

    #[test]
    fn parses_declarative_runtime_fields() {
        let config: AgentConfig = serde_yaml::from_str(
            r#"
kind: extractor
provider: gemini
model: gemini-2.5-flash
runner: cli
execution_mode: tool_loop
prompt_template: prompts/citation.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/extraction/citations.schema.json
verifiers: [json_schema, citation]
tools: [read_file, crossref_lookup, submit]
max_iters: 80
max_cost_usd: 0.5
max_retries: 2
timeout_secs: 240
escalation: agent
"#,
        )
        .unwrap();

        assert_eq!(config.kind, Some(AgentKind::Extractor));
        assert_eq!(config.execution_mode, DagExecutionMode::ToolLoop);
        assert_eq!(
            config.prompt_template.as_deref(),
            Some("prompts/citation.md")
        );
        assert_eq!(
            config.output_schema.as_deref(),
            Some("schemas/extraction/citations.schema.json")
        );
        assert_eq!(config.verifiers, vec!["json_schema", "citation"]);
        assert_eq!(config.tools, vec!["read_file", "crossref_lookup", "submit"]);
        assert_eq!(config.max_iters, Some(80));
        assert_eq!(config.max_cost_usd, Some(0.5));
        assert_eq!(config.escalation.as_deref(), Some("agent"));
    }

    #[test]
    fn parse_rejects_unknown_agent_config_fields() {
        let err = serde_yaml::from_str::<AgentConfig>(
            r#"
kind: extractor
provider: gemini
model: gemini-2.5-flash
runner: cli
prompt_template: prompts/extraction/macros.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/extraction/macros.schema.json
loop:
  max_iters: 20
"#,
        )
        .expect_err("unknown agent YAML fields must fail");

        assert!(err.to_string().contains("unknown field `loop`"));
    }

    #[test]
    fn validation_rejects_missing_declared_runtime_files() {
        let config: AgentConfig = serde_yaml::from_str(
            r#"
kind: critic
provider: claude
model: claude-haiku-4-5-20251001
runner: cli
prompt_template: prompts/summary.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/does-not-exist.schema.json
"#,
        )
        .unwrap();

        let err = validate_agent_config_detail("summary", &AgentKind::Critic, &config)
            .expect_err("missing output schema must fail startup validation");

        assert!(err.to_string().contains("output_schema"));
        assert!(err.to_string().contains("does-not-exist"));
    }

    #[test]
    fn paper_review_citation_uses_flash_for_bounded_review_latency() {
        let config: AgentConfig = serde_yaml::from_str(include_str!(
            "../../../../agents/paper-review/citation.yaml"
        ))
        .expect("citation agent config parses");

        assert_eq!(config.model, "gemini-2.5-flash");
        assert_eq!(config.timeout_secs, Some(360));
        assert_eq!(config.prompt_context.body_budget_chars, Some(0));
        assert_eq!(
            config.prompt_context.bibliography,
            BibliographyMode::Limited
        );
        assert_eq!(config.prompt_context.max_bibliography_entries, Some(24));
    }

    #[test]
    fn technical_correctness_does_not_own_citation_existence_verification() {
        let config: AgentConfig = serde_yaml::from_str(include_str!(
            "../../../../agents/paper-review/technical_correctness.yaml"
        ))
        .expect("technical correctness agent config parses");

        assert!(!config
            .verifiers
            .iter()
            .any(|verifier| verifier == "citation_existence"));
    }

    #[test]
    fn validation_rejects_invalid_tool_loop_config() {
        let config: AgentConfig = serde_yaml::from_str(
            r#"
kind: extractor
provider: gemini
model: gemini-2.5-flash
runner: cli
execution_mode: tool_loop
prompt_template: prompts/citation.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/extraction/citations.schema.json
tools: [read_file]
max_iters: 0
max_cost_usd: 0
"#,
        )
        .unwrap();

        let err =
            validate_agent_config_detail("citation_contextualizer", &AgentKind::Extractor, &config)
                .expect_err("invalid tool-loop config should fail validation");

        assert!(err.to_string().contains("max_iters"));
    }

    #[test]
    fn validation_rejects_kind_drift_from_manifest() {
        let config: AgentConfig = serde_yaml::from_str(
            r#"
kind: extractor
provider: claude
model: claude-haiku-4-5-20251001
runner: cli
prompt_template: prompts/summary.md
input_schema: schemas/paper_extract.schema.json
output_schema: schemas/summary_review.schema.json
"#,
        )
        .unwrap();

        let err = validate_agent_config_detail("summary", &AgentKind::Critic, &config)
            .expect_err("agent YAML kind must match DAG role kind");

        assert!(err.to_string().contains("kind=extractor"));
        assert!(err.to_string().contains("kind=critic"));
    }

    #[test]
    fn relative_apps_root_resolves_from_workspace_when_runtime_cwd_is_app_crate() {
        let workspace = workspace_root_for_test();
        let runtime_cwd = workspace.join("agenthero/apps/grokrxiv/crates/orchestrator");
        let _cwd = CurrentDirGuard::set(&runtime_cwd);
        let _apps_root = EnvVarGuard::set("AGENTHERO_APPS_ROOT", "agenthero/apps");
        let _dags_dir = EnvVarGuard::clear("AGENTHERO_DAGS_DIR");

        let manifest = dag_manifest_path("paper-review");

        assert_eq!(
            manifest,
            workspace.join("agenthero/apps/grokrxiv/dags/paper-review.yaml")
        );
        assert!(
            manifest.is_file(),
            "manifest should resolve to installed app root, got {}",
            manifest.display()
        );
    }
}
