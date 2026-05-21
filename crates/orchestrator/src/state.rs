//! Shared `AppState` injected into every axum handler.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::sync::Arc;

use grokrxiv_llm_adapter::{provider_by_name, LLMProvider, ProviderConfig};
use reqwest::Client;
use sqlx::PgPool;
use tokio::sync::mpsc;

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
/// Per-role configured-agent registry, keyed by review role.
pub type AgentRegistry = HashMap<grokrxiv_schemas::AgentRole, Arc<ConfiguredAgent>>;
/// Per-kind `AgentRunner` registry, keyed by runner backend.
pub type RunnerRegistry = HashMap<AgentRunnerKind, Arc<dyn AgentRunner>>;
/// Role-specific JSON schema documents.
#[cfg(feature = "grokrxiv-verifier")]
pub type AgentSchemaMap = HashMap<grokrxiv_schemas::AgentRole, AgentSchema>;
/// Role-specific verifier ladders.
#[cfg(feature = "grokrxiv-verifier")]
pub type VerifierMap = HashMap<grokrxiv_schemas::AgentRole, grokrxiv_verifier::VerifierLadder>;

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
    /// Per-`AgentRunnerKind` runner backends. RPT2 Track A registers only the
    /// `ApiRunner`; CLI/cloud/local-inference are filled by other tracks.
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

        #[cfg(feature = "grokrxiv-verifier")]
        let (agent_schemas, verifiers) = build_agent_schemas_and_verifiers();

        let role_yaml = load_role_configs();
        validate_role_configs(&role_yaml)?;
        validate_required_cli_runners(&role_yaml)?;

        // Build the runner registry. API can be empty in CLI-only setups;
        // CLI/cloud/local-inference are still registered and fail only when
        // invoked without their own prerequisites.
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
        // RPT2 G follow-up: register the other 3 runner kinds so the supervisor's
        // `--runner cli` / `--runner cloud` / `--runner local_inference` flag
        // can route through them at runtime. Each constructs cheaply; they only
        // hit network / spawn subprocesses when actually invoked.
        runners_map.insert(
            AgentRunnerKind::Cli,
            Arc::new(crate::agents::runners::cli::CliRunner::new()) as Arc<dyn AgentRunner>,
        );
        runners_map.insert(
            AgentRunnerKind::Cloud,
            Arc::new(crate::agents::runners::cloud::CloudRunner::new()) as Arc<dyn AgentRunner>,
        );
        runners_map.insert(
            AgentRunnerKind::LocalInference,
            Arc::new(crate::agents::runners::local_inference::LocalInferenceRunner::new())
                as Arc<dyn AgentRunner>,
        );
        let runners = Arc::new(runners_map);

        // Build the single per-role configured-agent registry from YAML and
        // in-memory schemas.
        let agents = {
            #[cfg(feature = "grokrxiv-verifier")]
            {
                Arc::new(build_agent_registry(&role_yaml, &agent_schemas))
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

/// Minimal YAML shape we read from `agents/*.yaml`. Captures the per-role
/// fields the orchestrator cares about: the routing target (`provider`,
/// `model`), the optional runner backend, and the agent-level timeout /
/// retry caps used to build [`AgentSpec`].
#[derive(serde::Deserialize, Clone)]
struct AgentRouting {
    provider: String,
    model: String,
    #[serde(default)]
    runner: Option<AgentRunnerKind>,
    #[serde(default)]
    max_retries: Option<u8>,
    #[serde(default)]
    timeout_secs: Option<u32>,
}

/// Per-role YAML config map. `None` for a role means the YAML was missing or
/// malformed; the consumer falls back to the default provider/model.
type RoleYamlMap = HashMap<grokrxiv_schemas::AgentRole, Option<AgentRouting>>;

/// Roles the orchestrator wires up at boot.
const ROLE_FILES: &[(grokrxiv_schemas::AgentRole, &str)] = &[
    (grokrxiv_schemas::AgentRole::Summary, "summary.yaml"),
    (
        grokrxiv_schemas::AgentRole::TechnicalCorrectness,
        "technical_correctness.yaml",
    ),
    (grokrxiv_schemas::AgentRole::Novelty, "novelty.yaml"),
    (
        grokrxiv_schemas::AgentRole::Reproducibility,
        "reproducibility.yaml",
    ),
    (grokrxiv_schemas::AgentRole::Citation, "citation.yaml"),
    (
        grokrxiv_schemas::AgentRole::MetaReviewer,
        "meta_reviewer.yaml",
    ),
];

/// Read each `agents/<role>.yaml` once for the configured-agent registry.
fn load_role_configs() -> RoleYamlMap {
    // Resolve the default relative to this crate, not process cwd. Some tests
    // deliberately chdir into temp directories before constructing AppState.
    let agents_dir = std::env::var("GROKRXIV_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("agents")
        });

    let mut out: RoleYamlMap = HashMap::new();
    for (role, filename) in ROLE_FILES {
        let path = agents_dir.join(filename);
        let cfg = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_yaml::from_str::<AgentRouting>(&s) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(
                        role = ?role,
                        path = %path.display(),
                        err = %e,
                        "could not parse agent yaml; startup validation will fail"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    role = ?role,
                    path = %path.display(),
                    err = %e,
                    "agent yaml missing; startup validation will fail"
                );
                None
            }
        };
        out.insert(*role, cfg);
    }
    out
}

fn validate_role_configs(role_yaml: &RoleYamlMap) -> anyhow::Result<()> {
    let missing: Vec<String> = ROLE_FILES
        .iter()
        .filter_map(|(role, filename)| match role_yaml.get(role) {
            Some(Some(_)) => None,
            _ => Some(format!("{role:?} ({filename})")),
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "agent role YAML is missing or malformed for {}; refusing to start with permissive schemas",
            missing.join(", ")
        )
    }
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
/// CLI/cloud/local-inference paths do not depend on provider APIs.
#[cfg(feature = "grokrxiv-verifier")]
fn build_agent_registry(role_yaml: &RoleYamlMap, schemas: &Arc<AgentSchemaMap>) -> AgentRegistry {
    let mut out: AgentRegistry = HashMap::new();
    for (role, _filename) in ROLE_FILES {
        let Some(cfg) = role_yaml.get(role).and_then(|c| c.as_ref()) else {
            continue;
        };
        let schema = schemas
            .get(role)
            .cloned()
            .expect("role schema loaded for configured role");
        let spec = AgentSpec {
            role: *role,
            runner: cfg.runner.unwrap_or_default(),
            sandbox: SandboxPolicy::None,
            provider: cfg.provider.clone(),
            model: model_override_for_role(*role).unwrap_or_else(|| cfg.model.clone()),
            schema,
            max_retries: cfg.max_retries.unwrap_or(2),
            timeout_secs: cfg.timeout_secs.unwrap_or(180),
        };
        out.insert(*role, Arc::new(build_agent(spec)));
    }
    out
}

#[cfg(feature = "grokrxiv-verifier")]
fn build_agent_schemas_and_verifiers() -> (Arc<AgentSchemaMap>, Arc<VerifierMap>) {
    use grokrxiv_schemas::AgentRole;
    use std::collections::HashMap;

    // The six per-role JSON Schema documents live alongside the workspace
    // (under `/schemas`). Embedding them with `include_str!` keeps the
    // orchestrator binary self-contained and avoids a runtime filesystem
    // dependency in container images.
    let summary: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/summary_review.schema.json"))
            .expect("summary_review schema");
    let technical: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/technical_review.schema.json"
    ))
    .expect("technical_review schema");
    let novelty: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/novelty_review.schema.json"))
            .expect("novelty_review schema");
    let reproducibility: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/reproducibility_review.schema.json"
    ))
    .expect("reproducibility_review schema");
    // The citation review schema $refs citation.schema.json. The jsonschema
    // validator we use does not resolve external $refs at runtime, so we
    // inline the referenced subschema once at boot.
    let citation_review_raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/citation_review.schema.json"))
            .expect("citation_review schema");
    let citation_subschema: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/citation.schema.json"))
            .expect("citation schema");
    let citation = inline_citation_ref(citation_review_raw, &citation_subschema);
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("../../../schemas/meta_review.schema.json"))
            .expect("meta_review schema");

    let mut schemas: HashMap<AgentRole, AgentSchema> = HashMap::new();
    schemas.insert(AgentRole::Summary, Arc::new(summary));
    schemas.insert(AgentRole::TechnicalCorrectness, Arc::new(technical));
    schemas.insert(AgentRole::Novelty, Arc::new(novelty));
    schemas.insert(AgentRole::Reproducibility, Arc::new(reproducibility));
    schemas.insert(AgentRole::Citation, Arc::new(citation));
    schemas.insert(AgentRole::MetaReviewer, Arc::new(meta));

    let mut ladders: HashMap<AgentRole, grokrxiv_verifier::VerifierLadder> = HashMap::new();
    for (role, schema) in &schemas {
        ladders.insert(
            *role,
            grokrxiv_verifier::VerifierLadder::standard_for_role(
                *role,
                Some(schema.as_ref().clone()),
            ),
        );
    }

    (Arc::new(schemas), Arc::new(ladders))
}

/// Walk `value` and rewrite any `{ "$ref": "citation.schema.json" }` node so
/// it points at the inlined `citation` subschema. Keeps the on-disk schema
/// human-friendly while making the runtime validator self-contained.
#[cfg(feature = "grokrxiv-verifier")]
fn inline_citation_ref(
    value: serde_json::Value,
    citation: &serde_json::Value,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(mut map) => {
            if let Some(serde_json::Value::String(s)) = map.get("$ref") {
                if s == "citation.schema.json" {
                    return citation.clone();
                }
            }
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map.iter_mut() {
                out.insert(k.clone(), inline_citation_ref(v.take(), citation));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(|v| inline_citation_ref(v, citation))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(all(test, feature = "grokrxiv-verifier"))]
mod tests {
    use super::*;
    use crate::runtime_config::ALLOW_PROVIDER_API_ENV;
    use grokrxiv_schemas::AgentRole;

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

    #[test]
    fn build_agent_registry_applies_resolved_model_override() {
        let _guard = EnvVarGuard::set(
            crate::runtime_config::role_model_override_env_var(AgentRole::Summary),
            "claude-sonnet-test",
        );
        let mut role_yaml = RoleYamlMap::new();
        role_yaml.insert(
            AgentRole::Summary,
            Some(AgentRouting {
                provider: "claude".to_string(),
                model: "claude-haiku-4-5-20251001".to_string(),
                runner: Some(AgentRunnerKind::Cli),
                max_retries: Some(2),
                timeout_secs: Some(90),
            }),
        );
        let mut schemas = AgentSchemaMap::new();
        let schema = Arc::new(serde_json::json!({ "type": "object" }));
        schemas.insert(AgentRole::Summary, schema.clone());
        let registry = build_agent_registry(&role_yaml, &Arc::new(schemas));
        let agent = registry.get(&AgentRole::Summary).expect("summary agent");

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
            "serve/preview must not register direct provider APIs when GROKRXIV_ALLOW_PROVIDER_API=0"
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
        let _bin = EnvVarGuard::unset("GROKRXIV_CLAUDE_BIN".to_string());
        let mut role_yaml = RoleYamlMap::new();
        role_yaml.insert(
            AgentRole::Summary,
            Some(AgentRouting {
                provider: "claude".to_string(),
                model: "claude-haiku-4-5-20251001".to_string(),
                runner: Some(AgentRunnerKind::Cli),
                max_retries: Some(2),
                timeout_secs: Some(90),
            }),
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
            AgentRole::Summary,
            Some(AgentRouting {
                provider: "claude".to_string(),
                model: "claude-haiku-4-5-20251001".to_string(),
                runner: Some(AgentRunnerKind::Api),
                max_retries: Some(2),
                timeout_secs: Some(90),
            }),
        );

        validate_required_cli_runners_with_path(&role_yaml, &std::ffi::OsString::new())
            .expect("API roles should not require local CLI binaries");
    }

    #[tokio::test]
    async fn role_ladders_include_citation_only_for_citation_role() {
        let (_schemas, ladders) = build_agent_schemas_and_verifiers();
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
        let ctx = grokrxiv_verifier::VerifierContext {
            paper: &paper,
            http: &http,
        };

        let summary_names: Vec<String> = ladders
            .get(&AgentRole::Summary)
            .expect("summary ladder")
            .run(&serde_json::json!({}), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let citation_names: Vec<String> = ladders
            .get(&AgentRole::Citation)
            .expect("citation ladder")
            .run(&serde_json::json!({}), &ctx)
            .await
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert!(!summary_names.contains(&"citation".to_string()));
        assert!(citation_names.contains(&"citation".to_string()));
    }

    fn write_api_role_configs(dir: &std::path::Path) {
        for (_role, filename) in ROLE_FILES {
            std::fs::write(
                dir.join(filename),
                "provider: claude\nmodel: claude-haiku-4-5-20251001\nrunner: api\n",
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn state_does_not_register_api_runner_when_direct_api_is_disabled() {
        let _allow = EnvVarGuard::set(ALLOW_PROVIDER_API_ENV.to_string(), "0");
        let tmp = tempfile::tempdir().unwrap();
        write_api_role_configs(tmp.path());
        let _agents_dir = EnvVarGuard::set(
            "GROKRXIV_AGENTS_DIR".to_string(),
            tmp.path().to_str().unwrap(),
        );
        let mut config = Config::from_env();
        config.database_url = None;

        let state = AppState::from_config(config)
            .await
            .expect("state should still boot for CLI-only runtime");

        assert!(
            state.providers.is_none(),
            "provider registry should not be built when direct provider API is disabled"
        );
        assert!(
            !state.runners.contains_key(&AgentRunnerKind::Api),
            "state should not register an API runner that is guaranteed to fail under the API guard"
        );
    }
}
