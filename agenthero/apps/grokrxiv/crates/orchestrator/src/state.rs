//! Shared `AppState` injected into every axum handler.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use grokrxiv_llm_adapter::{provider_by_name, LLMProvider, ProviderConfig};
use reqwest::Client;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::agents::config::{self, AgentConfig};
use crate::agents::runners::api::ApiRunner;
use crate::agents::{
    build_agent, AgentRunner, AgentRunnerKind, AgentSchema, AgentSpec, ConfiguredAgent,
    SandboxPolicy,
};
use crate::arxiv_rate_limit::ArxivGate;
use crate::config::Config;
use crate::runtime_config::{direct_provider_api_allowed_from_env, model_override_for_role};

/// Providers keyed by short name (`claude`, `openai`, `gemini`, `vllm`).
pub type ProviderMap = HashMap<&'static str, Arc<dyn LLMProvider>>;
/// Per-role configured-agent registry, keyed by DAG-scoped role id.
pub type AgentRegistry = HashMap<String, Arc<ConfiguredAgent>>;
/// Per-kind `AgentRunner` registry, keyed by runner backend.
pub type RunnerRegistry = HashMap<AgentRunnerKind, Arc<dyn AgentRunner>>;
/// Parsed agent YAML configs, keyed by DAG-scoped role id.
pub type AgentConfigMap = HashMap<String, AgentConfig>;
/// Role-specific JSON schema documents, keyed by DAG-scoped role id.
#[cfg(feature = "grokrxiv-verifier")]
pub type AgentSchemaMap = HashMap<String, AgentSchema>;
/// Role-specific verifier ladders, keyed by DAG-scoped role id.
#[cfg(feature = "grokrxiv-verifier")]
pub type VerifierMap = HashMap<String, grokrxiv_verifier::VerifierLadder>;

/// Registry of all configured LLM providers, keyed by short name.
///
/// The registry is built from whichever provider API keys are available in
/// the environment at boot. CLI-only review/extraction runs do not require
/// this registry; API-backed routes use it to dispatch direct provider calls.
#[derive(Clone)]
pub struct ProviderRegistry {
    /// All providers reachable from this process, keyed by short name
    /// (`"claude" | "openai" | "gemini" | "vllm"`).
    pub by_name: Arc<ProviderMap>,
    /// Default provider used by the `/preview` route and API fallback paths.
    /// Present only when a Claude API key was available at boot.
    pub default: Arc<dyn LLMProvider>,
}

impl ProviderRegistry {
    /// Build the registry from environment-provided keys.
    ///
    /// `ANTHROPIC_API_KEY` is required only to construct the default API
    /// provider. If it is missing the registry returns `None`, while CLI
    /// runner paths can still execute through their local subscriptions.
    /// Additional providers (OpenAI / Gemini / vLLM) are registered when their
    /// respective env vars are present.
    pub fn from_env() -> Option<Self> {
        if !direct_provider_api_allowed_from_env() {
            return None;
        }
        let cfg = ProviderConfig::from_env();
        let mut by_name: HashMap<&'static str, Arc<dyn LLMProvider>> = HashMap::new();
        if nonblank_env("ANTHROPIC_API_KEY").is_some() {
            if let Ok(p) = provider_by_name("claude", &cfg) {
                by_name.insert("claude", p);
            }
        }
        if nonblank_env("OPENAI_API_KEY").is_some() {
            if let Ok(p) = provider_by_name("openai", &cfg) {
                by_name.insert("openai", p);
            }
        }
        if nonblank_env("GOOGLE_GENERATIVE_AI_API_KEY").is_some() {
            if let Ok(p) = provider_by_name("gemini", &cfg) {
                by_name.insert("gemini", p);
            }
        }
        if nonblank_env("VLLM_BASE_URL").is_some() {
            if let Ok(p) = provider_by_name("vllm", &cfg) {
                by_name.insert("vllm", p);
            }
        }
        let default = by_name.get("claude").cloned()?;
        Some(Self {
            by_name: Arc::new(by_name),
            default,
        })
    }
}

fn nonblank_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Application state shared with every handler.
#[derive(Clone)]
pub struct AppState {
    /// Optional Postgres pool (None when the orchestrator is started without
    /// a `DATABASE_URL`).
    pub db: Option<PgPool>,
    /// LLM provider registry. `None` if no API keys are available.
    pub providers: Option<ProviderRegistry>,
    /// Shared HTTP client for outbound calls (revalidate, webhooks, etc.).
    pub http: Client,
    /// Cached configuration.
    pub config: Arc<Config>,
    /// Single-flight gate enforcing arXiv's rate-limit guidance.
    pub arxiv: Arc<ArxivGate>,
    /// Per-role JSON-schema documents loaded at boot. Used both to constrain
    /// the LLM call (via `ResponseFormat::JsonSchema`) and to drive each role's
    /// verifier ladder. Only present when the verifier feature is on.
    #[cfg(feature = "grokrxiv-verifier")]
    pub agent_schemas: Arc<AgentSchemaMap>,
    /// Per-role verifier ladders, each built around its role-specific JSON
    /// schema. Replaces the previous single permissive-object ladder.
    #[cfg(feature = "grokrxiv-verifier")]
    pub verifiers: Arc<VerifierMap>,
    /// Per-role configured agents built from `agents/*.yaml`. This is the
    /// single role registry for review dispatch.
    pub agents: Arc<AgentRegistry>,
    /// Parsed YAML config for each DAG role. Runtime code reads behavior flags
    /// from this map instead of matching role ids.
    pub agent_configs: Arc<AgentConfigMap>,
    /// Per-`AgentRunnerKind` runner backends. RPT2 Track A registers only the
    /// `ApiRunner`; CLI is registered separately.
    pub runners: Arc<RunnerRegistry>,
    /// Supervisor sender used by internal HTTP write endpoints to enqueue
    /// durable work items.
    pub supervisor_tx: Option<mpsc::Sender<crate::supervisor::WorkItem>>,
}

impl AppState {
    /// Build an `AppState` with the supplied configuration. Side-effecting:
    /// also constructs the shared HTTP client and (if available) connects to
    /// Postgres.
    pub async fn from_config(config: Config) -> anyhow::Result<Self> {
        let http = Client::builder()
            .user_agent(config.arxiv_user_agent.clone())
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let db = match &config.database_url {
            Some(url) => match PgPool::connect_lazy(url) {
                Ok(pool) => Some(pool),
                Err(e) => {
                    tracing::warn!(err = %e, "could not connect to database; running in stateless mode");
                    None
                }
            },
            None => None,
        };

        let providers = ProviderRegistry::from_env();
        let arxiv = Arc::new(ArxivGate::new(std::time::Duration::from_secs(3)));

        let role_yaml = load_role_configs()?;
        validate_role_configs(&role_yaml)?;
        validate_required_cli_runners(&role_yaml)?;
        let agent_configs = Arc::new(
            role_yaml
                .iter()
                .filter_map(|(role, cfg)| cfg.as_ref().map(|cfg| (role.clone(), cfg.clone())))
                .collect::<AgentConfigMap>(),
        );

        #[cfg(feature = "grokrxiv-verifier")]
        let (agent_schemas, verifiers) = build_agent_schemas_and_verifiers(&role_yaml)?;

        // Build the runner registry. API can be empty in CLI-only setups.
        let mut runners_map: RunnerRegistry = HashMap::new();
        if direct_provider_api_allowed_from_env() {
            let provider_map_by_string: HashMap<String, Arc<dyn LLMProvider>> = match &providers {
                Some(reg) => reg
                    .by_name
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
                None => HashMap::new(),
            };
            let api_runner: Arc<dyn AgentRunner> = Arc::new(ApiRunner::new(provider_map_by_string));
            runners_map.insert(AgentRunnerKind::Api, api_runner);
        }
        runners_map.insert(
            AgentRunnerKind::Cli,
            Arc::new(crate::agents::runners::cli::CliRunner::new()) as Arc<dyn AgentRunner>,
        );
        let runners = Arc::new(runners_map);

        // Build the single per-role configured-agent registry from YAML and
        // in-memory schemas.
        let agents = {
            #[cfg(feature = "grokrxiv-verifier")]
            {
                Arc::new(build_agent_registry(&role_yaml, &agent_schemas)?)
            }
            #[cfg(not(feature = "grokrxiv-verifier"))]
            {
                Arc::new(AgentRegistry::new())
            }
        };

        Ok(Self {
            db,
            providers,
            http,
            config: Arc::new(config),
            arxiv,
            #[cfg(feature = "grokrxiv-verifier")]
            agent_schemas,
            #[cfg(feature = "grokrxiv-verifier")]
            verifiers,
            agents,
            agent_configs,
            runners,
            supervisor_tx: None,
        })
    }

    /// Attach the live supervisor sender after the supervisor has been spawned.
    pub fn with_supervisor_sender(mut self, tx: mpsc::Sender<crate::supervisor::WorkItem>) -> Self {
        self.supervisor_tx = Some(tx);
        self
    }
}

/// Per-role YAML config map. `None` means the DAG-declared YAML was missing or
/// malformed; startup validation refuses to continue in that state.
type RoleYamlMap = HashMap<String, Option<AgentConfig>>;

const PAPER_REVIEW_DAG_ID: &str = "paper-review";
const REVIEW_LOOP_DAG_ID: &str = "review-loop";

/// Read each configured review role YAML once for the configured-agent
/// registry. `paper-review.yaml` is the source of truth.
fn load_role_configs() -> anyhow::Result<RoleYamlMap> {
    let mut out: RoleYamlMap = HashMap::new();
    for config_ref in loaded_role_config_refs()? {
        let cfg = match config::read_agent_config(&config_ref.path) {
            Ok(config) => Some(config),
            Err(e) => {
                tracing::warn!(
                    role = %config_ref.role_id,
                    path = %config_ref.path.display(),
                    err = %e,
                    "agent yaml missing; startup validation will fail"
                );
                None
            }
        };
        out.insert(config_ref.role_id, cfg);
    }
    Ok(out)
}

fn validate_role_configs(role_yaml: &RoleYamlMap) -> anyhow::Result<()> {
    let config_refs = loaded_role_config_refs()?;
    let missing: Vec<String> = config_refs
        .iter()
        .filter_map(|config_ref| match role_yaml.get(&config_ref.role_id) {
            Some(Some(_)) => None,
            _ => Some(format!("{} ({})", config_ref.role_id, config_ref.label)),
        })
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "agent role YAML is missing or malformed for {}; refusing to start with permissive schemas",
            missing.join(", ")
        )
    }
    for config_ref in config_refs {
        if let Some(Some(cfg)) = role_yaml.get(&config_ref.role_id) {
            config::validate_agent_config_detail(
                &config_ref.role_id,
                &config_ref.expected_kind,
                cfg,
            )?;
        }
    }
    Ok(())
}

fn validate_required_cli_runners(role_yaml: &RoleYamlMap) -> anyhow::Result<()> {
    validate_required_cli_runners_with_path(
        role_yaml,
        &std::env::var_os("PATH").unwrap_or_default(),
    )
}

fn validate_required_cli_runners_with_path(
    role_yaml: &RoleYamlMap,
    path: &OsString,
) -> anyhow::Result<()> {
    let required = required_cli_bins(role_yaml)?;
    let missing: Vec<String> = required
        .into_iter()
        .filter(|bin| crate::doctor::binary_available_in_path(bin, path).is_none())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "configured CLI runner binaries are missing from PATH: {}",
            missing.join(", ")
        )
    }
}

fn required_cli_bins(role_yaml: &RoleYamlMap) -> anyhow::Result<BTreeSet<String>> {
    let mut bins = BTreeSet::new();
    for cfg in role_yaml.values().filter_map(|cfg| cfg.as_ref()) {
        if cfg.runner.unwrap_or_default() == AgentRunnerKind::Cli {
            bins.insert(crate::doctor::cli_binary_for_provider(&cfg.provider)?);
        }
    }
    Ok(bins)
}

/// Build the per-role configured-agent registry from the YAML configs and the
/// in-memory per-role JSON schemas. Startup validation already rejected
/// missing/malformed YAML, so this registry is the complete role source of
/// truth. Runtime flags/env select the actual runner, so
/// CLI paths do not depend on provider APIs.
#[cfg(feature = "grokrxiv-verifier")]
fn build_agent_registry(
    role_yaml: &RoleYamlMap,
    schemas: &Arc<AgentSchemaMap>,
) -> anyhow::Result<AgentRegistry> {
    let mut out: AgentRegistry = HashMap::new();
    for config_ref in loaded_role_config_refs()? {
        let role = config_ref.role_id;
        let Some(cfg) = role_yaml.get(&role).and_then(|c| c.as_ref()) else {
            continue;
        };
        let schema = schemas
            .get(&role)
            .cloned()
            .expect("role schema loaded for configured role");
        let spec = AgentSpec {
            role: role.clone(),
            runner: cfg.runner.unwrap_or_default(),
            sandbox: SandboxPolicy::None,
            provider: cfg.provider.clone(),
            model: model_override_for_role(&role).unwrap_or_else(|| cfg.model.clone()),
            schema,
            max_retries: cfg.max_retries.unwrap_or(2),
            timeout_secs: cfg.timeout_secs.unwrap_or(180),
        };
        out.insert(role, Arc::new(build_agent(spec)));
    }
    Ok(out)
}

fn loaded_role_config_refs() -> anyhow::Result<Vec<config::AgentConfigRef>> {
    let mut refs = Vec::new();
    for dag_id in [PAPER_REVIEW_DAG_ID, REVIEW_LOOP_DAG_ID] {
        refs.extend(role_config_refs(dag_id)?);
    }
    Ok(refs)
}

fn role_config_refs(dag_id: &str) -> anyhow::Result<Vec<config::AgentConfigRef>> {
    config::dag_agent_config_refs(dag_id)
        .map_err(|err| anyhow::anyhow!("could not load `{dag_id}` DAG agent configs: {err:#}"))
}

#[cfg(feature = "grokrxiv-verifier")]
fn build_agent_schemas_and_verifiers(
    role_yaml: &RoleYamlMap,
) -> anyhow::Result<(Arc<AgentSchemaMap>, Arc<VerifierMap>)> {
    let mut schemas: AgentSchemaMap = HashMap::new();
    let mut ladders: VerifierMap = HashMap::new();

    for (role_id, cfg) in role_yaml {
        let Some(cfg) = cfg.as_ref() else {
            continue;
        };
        let Some(output_schema) = cfg.output_schema.as_deref() else {
            anyhow::bail!("agent role YAML for {role_id} is missing output_schema");
        };
        let schema_path = config::resolve_declared_runtime_path(output_schema);
        let schema_text = std::fs::read_to_string(&schema_path).map_err(|e| {
            anyhow::anyhow!(
                "read output_schema {} for {role_id}: {e}",
                schema_path.display()
            )
        })?;
        let schema: serde_json::Value = serde_json::from_str(&schema_text).map_err(|e| {
            anyhow::anyhow!(
                "parse output_schema {} for {role_id}: {e}",
                schema_path.display()
            )
        })?;
        let base_dir = schema_path.parent().unwrap_or_else(|| Path::new("."));
        let schema = inline_local_schema_refs(schema, base_dir)?;
        schemas.insert(role_id.clone(), Arc::new(schema.clone()));
        ladders.insert(
            role_id.clone(),
            grokrxiv_verifier::VerifierLadder::standard_for_config(&cfg.verifiers, Some(schema)),
        );
    }

    Ok((Arc::new(schemas), Arc::new(ladders)))
}

/// Walk `value` and inline local JSON-schema `$ref` files. Keeps on-disk
/// schemas human-readable while making runtime validators self-contained.
#[cfg(feature = "grokrxiv-verifier")]
fn inline_local_schema_refs(
    value: serde_json::Value,
    base_dir: &Path,
) -> anyhow::Result<serde_json::Value> {
    match value {
        serde_json::Value::Object(mut map) => {
            if let Some(serde_json::Value::String(s)) = map.get("$ref") {
                if is_local_schema_ref(s) {
                    let ref_path = base_dir.join(s);
                    let ref_text = std::fs::read_to_string(&ref_path).map_err(|e| {
                        anyhow::anyhow!("read local schema ref {}: {e}", ref_path.display())
                    })?;
                    let ref_value: serde_json::Value =
                        serde_json::from_str(&ref_text).map_err(|e| {
                            anyhow::anyhow!("parse local schema ref {}: {e}", ref_path.display())
                        })?;
                    let ref_base = ref_path.parent().unwrap_or(base_dir);
                    return inline_local_schema_refs(ref_value, ref_base);
                }
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map.iter_mut() {
                out.insert(k.clone(), inline_local_schema_refs(v.take(), base_dir)?);
            }
            Ok(serde_json::Value::Object(out))
        }
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(inline_local_schema_refs(v, base_dir)?);
            }
            Ok(serde_json::Value::Array(out))
        }
        other => Ok(other),
    }
}

#[cfg(feature = "grokrxiv-verifier")]
fn is_local_schema_ref(reference: &str) -> bool {
    !reference.starts_with('#')
        && !reference.starts_with("http://")
        && !reference.starts_with("https://")
}

#[cfg(all(test, feature = "grokrxiv-verifier"))]
mod tests {
    use super::*;
    use crate::runtime_config::ALLOW_PROVIDER_API_ENV;
    use agenthero_dag_runtime::{AgentKind, DagExecutionMode};

    struct EnvVarGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: String, value: &str) -> Self {
            let prev = std::env::var(&key).ok();
            std::env::set_var(&key, value);
            Self { key, prev }
        }

        fn unset(key: String) -> Self {
            let prev = std::env::var(&key).ok();
            std::env::remove_var(&key);
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    fn test_agent_config(runner: AgentRunnerKind) -> AgentConfig {
        AgentConfig {
            id: Some("summary".to_string()),
            kind: Some(AgentKind::Critic),
            role: Some("Test summary agent".to_string()),
            provider: "claude".to_string(),
            model: "claude-haiku-4-5-20251001".to_string(),
            runner: Some(runner),
            execution_mode: DagExecutionMode::OneShot,
            prompt_template: Some("prompts/summary.md".to_string()),
            input_schema: Some("schemas/paper_extract.schema.json".to_string()),
            output_schema: Some("schemas/summary_review.schema.json".to_string()),
            verifiers: vec!["json_schema".to_string()],
            tools: Vec::new(),
            prompt_context: Default::default(),
            system_overlays: Vec::new(),
            postprocessors: Vec::new(),
            max_iters: None,
            max_cost_usd: None,
            max_retries: Some(2),
            timeout_secs: Some(90),
            escalation: Some("skip".to_string()),
        }
    }

    #[test]
    fn build_agent_registry_applies_resolved_model_override() {
        let role = "haskell_semantic_author";
        let _guard = EnvVarGuard::set(
            crate::runtime_config::role_model_override_env_var(role),
            "claude-sonnet-test",
        );
        let mut role_yaml = RoleYamlMap::new();
        role_yaml.insert(role.to_string(), Some(test_agent_config(AgentRunnerKind::Cli)));
        let mut schemas = AgentSchemaMap::new();
        let schema = Arc::new(serde_json::json!({ "type": "object" }));
        schemas.insert(role.to_string(), schema.clone());
        let registry = build_agent_registry(&role_yaml, &Arc::new(schemas)).unwrap();
        let agent = registry.get(role).expect("haskell semantic author agent");

        assert_eq!(agent.spec().model, "claude-sonnet-test");
        assert!(
            Arc::ptr_eq(&agent.spec().schema, &schema),
            "agent registry should share schema Arcs instead of cloning large Values"
        );
    }

    #[test]
    fn provider_registry_is_disabled_when_provider_api_guard_is_zero() {
        let _allow = EnvVarGuard::set(ALLOW_PROVIDER_API_ENV.to_string(), "0");
        let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY".to_string(), "test-key");

        assert!(
            ProviderRegistry::from_env().is_none(),
            "serve/preview must not register direct provider APIs when AGENTHERO_ALLOW_PROVIDER_API=0"
        );
    }

    #[test]
    fn provider_registry_ignores_blank_provider_keys() {
        let _allow = EnvVarGuard::set(ALLOW_PROVIDER_API_ENV.to_string(), "1");
        let _anthropic = EnvVarGuard::set("ANTHROPIC_API_KEY".to_string(), "   ");
        let _openai = EnvVarGuard::unset("OPENAI_API_KEY".to_string());
        let _google = EnvVarGuard::unset("GOOGLE_GENERATIVE_AI_API_KEY".to_string());
        let _vllm = EnvVarGuard::unset("VLLM_BASE_URL".to_string());

        assert!(
            ProviderRegistry::from_env().is_none(),
            "blank provider keys are not usable API credentials"
        );
    }

    #[test]
    fn configured_cli_role_requires_binary_at_startup() {
        let _bin = EnvVarGuard::unset("AGENTHERO_CLAUDE_BIN".to_string());
        let mut role_yaml = RoleYamlMap::new();
        role_yaml.insert(
            "summary".to_string(),
            Some(test_agent_config(AgentRunnerKind::Cli)),
        );

        let err = validate_required_cli_runners_with_path(&role_yaml, &std::ffi::OsString::new())
            .expect_err("missing configured CLI binary should refuse startup");

        assert!(
            err.to_string().contains("claude"),
            "error should name missing binary, got: {err:#}"
        );
    }

    #[test]
    fn api_role_does_not_require_cli_binary_at_startup() {
        let mut role_yaml = RoleYamlMap::new();
        role_yaml.insert(
            "summary".to_string(),
            Some(test_agent_config(AgentRunnerKind::Api)),
        );

        validate_required_cli_runners_with_path(&role_yaml, &std::ffi::OsString::new())
            .expect("API roles should not require local CLI binaries");
    }

    #[test]
    fn missing_review_dag_config_returns_error_instead_of_panicking() {
        let result = std::panic::catch_unwind(|| role_config_refs("missing-review-dag"));

        assert!(
            result.is_ok(),
            "missing review DAG config should be reported as an error, not a panic"
        );
        let err = result
            .expect("catch_unwind result checked")
            .expect_err("missing review DAG config should return an error");
        assert!(
            err.to_string()
                .contains("could not load `missing-review-dag`"),
            "error should identify the missing DAG config, got: {err:#}"
        );
    }

    #[tokio::test]
    async fn configured_ladders_follow_yaml_verifier_names() {
        let mut role_yaml = RoleYamlMap::new();
        let mut summary = test_agent_config(AgentRunnerKind::Api);
        summary.verifiers = vec!["json_schema".to_string()];
        role_yaml.insert("summary".to_string(), Some(summary));
        let mut citation = test_agent_config(AgentRunnerKind::Api);
        citation.output_schema = Some("schemas/citation_review.schema.json".to_string());
        citation.verifiers = vec!["json_schema".to_string(), "citation_existence".to_string()];
        role_yaml.insert("citation".to_string(), Some(citation));

        let (_schemas, ladders) = build_agent_schemas_and_verifiers(&role_yaml).unwrap();
        let http = reqwest::Client::new();
        let paper = grokrxiv_schemas::PaperExtract {
            arxiv_id: "2605.00001".to_string(),
            title: "Verifier Paper".to_string(),
            authors: Vec::new(),
            abstract_: "A paper abstract.".to_string(),
            field: Some("cs.AI".to_string()),
            sections: Vec::new(),
            figures: Vec::new(),
            bibliography: Vec::new(),
            source_format: None,
        };
        let ctx = grokrxiv_verifier::VerifierContext::for_paper(&paper, &http);

        let summary_names: Vec<String> = ladders
            .get("summary")
            .expect("summary ladder")
            .run(&serde_json::json!({}), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let citation_names: Vec<String> = ladders
            .get("citation")
            .expect("citation ladder")
            .run(&serde_json::json!({}), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(!summary_names.contains(&"citation".to_string()));
        assert!(citation_names.contains(&"citation".to_string()));
    }
}
