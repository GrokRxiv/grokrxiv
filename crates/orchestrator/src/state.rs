//! Shared `AppState` injected into every axum handler.

use std::collections::HashMap;
use std::sync::Arc;

use grokrxiv_llm_adapter::{provider_by_name, LLMProvider, ProviderConfig};
use reqwest::Client;
use sqlx::PgPool;

use crate::agents::runners::api::ApiRunner;
use crate::agents::{
    build_agent, AgentMode, AgentRunner, AgentRunnerKind, AgentSpec, ReviewAgent, SandboxPolicy,
    ToolPolicy,
};
use crate::arxiv_rate_limit::ArxivGate;
use crate::config::Config;
use crate::runtime_config::ALLOW_PROVIDER_API_ENV;

/// Providers keyed by short name (`claude`, `openai`, `gemini`, `vllm`).
pub type ProviderMap = HashMap<&'static str, Arc<dyn LLMProvider>>;
/// Per-agent provider and model routing table.
pub type RoleRouting = HashMap<grokrxiv_schemas::AgentRole, (Arc<dyn LLMProvider>, String)>;
/// Per-role `ReviewAgent` registry, keyed by review role.
pub type AgentRegistry = HashMap<grokrxiv_schemas::AgentRole, Arc<dyn ReviewAgent>>;
/// Per-kind `AgentRunner` registry, keyed by runner backend.
pub type RunnerRegistry = HashMap<AgentRunnerKind, Arc<dyn AgentRunner>>;
/// Role-specific JSON schema documents.
#[cfg(feature = "grokrxiv-verifier")]
pub type AgentSchemaMap = HashMap<grokrxiv_schemas::AgentRole, serde_json::Value>;
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
        let cfg = ProviderConfig::from_env();
        let mut by_name: HashMap<&'static str, Arc<dyn LLMProvider>> = HashMap::new();
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            if let Ok(p) = provider_by_name("claude", &cfg) {
                by_name.insert("claude", p);
            }
        }
        if std::env::var("OPENAI_API_KEY").is_ok() {
            if let Ok(p) = provider_by_name("openai", &cfg) {
                by_name.insert("openai", p);
            }
        }
        if std::env::var("GOOGLE_GENERATIVE_AI_API_KEY").is_ok() {
            if let Ok(p) = provider_by_name("gemini", &cfg) {
                by_name.insert("gemini", p);
            }
        }
        if std::env::var("VLLM_BASE_URL").is_ok() {
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
    /// Legacy per-role `(provider, model)` routing for direct API paths.
    /// CLI review/extraction runs use `agents` + `runners` instead and do not
    /// need provider API keys.
    pub role_routing: Arc<RoleRouting>,
    /// Per-role `ReviewAgent` instances built from `agents/*.yaml`. Populated
    /// alongside `role_routing` so the supervisor can delegate to
    /// `agent.run(&runner, input)` instead of calling the provider directly.
    /// Empty when no provider registry is available (e.g. `from_env` returned
    /// `None`); in that case the supervisor falls back to constructing an agent
    /// on the fly from the passed-in provider.
    pub agents: Arc<AgentRegistry>,
    /// Per-`AgentRunnerKind` runner backends. RPT2 Track A registers only the
    /// `ApiRunner`; CLI/cloud/local-inference are filled by other tracks.
    pub runners: Arc<RunnerRegistry>,
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

        let fallback_model = config.preview_model.clone();
        let role_yaml = load_role_configs();
        let role_routing = Arc::new(build_role_routing(&providers, &fallback_model, &role_yaml));

        // Build the runner registry. API can be empty in CLI-only setups;
        // CLI/cloud/local-inference are still registered and fail only when
        // invoked without their own prerequisites.
        let provider_map_by_string: HashMap<String, Arc<dyn LLMProvider>> = match &providers {
            Some(reg) => reg
                .by_name
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
            None => HashMap::new(),
        };
        let api_runner: Arc<dyn AgentRunner> = Arc::new(ApiRunner::new(provider_map_by_string));
        let mut runners_map: RunnerRegistry = HashMap::new();
        runners_map.insert(AgentRunnerKind::Api, api_runner);
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

        // Build the per-role `ReviewAgent` registry from the YAML configs +
        // the in-memory schemas. Only populated when the verifier feature is
        // on (so schemas exist). The registry is independent of direct API
        // providers so `--runner cli` can execute without provider API keys.
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
            role_routing,
            agents,
            runners,
        })
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

/// Read each `agents/<role>.yaml` once. The result is consumed by both
/// `build_role_routing` (legacy provider+model map) and `build_agent_registry`
/// (new `ReviewAgent` map).
fn load_role_configs() -> RoleYamlMap {
    // Resolve `agents/` relative to the workspace root. In a container the
    // binary's cwd is the repo root; in dev `cargo run` also runs from there.
    let agents_dir = std::env::var("GROKRXIV_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("agents"));

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
                        "could not parse agent yaml; falling back to default provider/model"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    role = ?role,
                    path = %path.display(),
                    err = %e,
                    "agent yaml missing; falling back to default provider/model"
                );
                None
            }
        };
        out.insert(*role, cfg);
    }
    out
}

/// Build the per-role `(provider, model)` map for legacy direct API routing.
/// CLI-only runs intentionally leave this empty so missing provider API keys
/// do not look like fallback-to-Claude API traffic.
fn build_role_routing(
    providers: &Option<ProviderRegistry>,
    fallback_model: &str,
    role_yaml: &RoleYamlMap,
) -> RoleRouting {
    let mut out: RoleRouting = HashMap::new();
    if std::env::var(ALLOW_PROVIDER_API_ENV).ok().as_deref() == Some("0") {
        return out;
    }

    // No providers at all means /preview will error anyway; routing is empty.
    let Some(registry) = providers else {
        return out;
    };

    for (role, _filename) in ROLE_FILES {
        let Some(routing) = role_yaml.get(role).and_then(|c| c.as_ref()) else {
            out.insert(
                *role,
                (registry.default.clone(), fallback_model.to_string()),
            );
            continue;
        };
        let declared_provider = routing.provider.as_str();
        match registry.by_name.get(declared_provider) {
            Some(p) => {
                out.insert(*role, (p.clone(), routing.model.clone()));
            }
            None => {
                tracing::warn!(
                    role = ?role,
                    declared = %declared_provider,
                    declared_model = %routing.model,
                    "missing API key; falling back to claude"
                );
                out.insert(
                    *role,
                    (registry.default.clone(), fallback_model.to_string()),
                );
            }
        }
    }
    out
}

/// Build the per-role `ReviewAgent` registry from the YAML configs and the
/// in-memory per-role JSON schemas. Roles whose YAML is missing or malformed
/// are skipped; the supervisor will detect the gap and fall back to an
/// on-the-fly agent for those roles. Runtime flags/env select the actual
/// runner, so CLI/cloud/local-inference paths do not depend on provider APIs.
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
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        let spec = AgentSpec {
            role: *role,
            runner: cfg.runner.unwrap_or_default(),
            sandbox: SandboxPolicy::None,
            mode: AgentMode::ReviewOnly,
            provider: cfg.provider.clone(),
            model: cfg.model.clone(),
            schema,
            tool_policy: ToolPolicy::default(),
            max_retries: cfg.max_retries.unwrap_or(2),
            timeout_secs: cfg.timeout_secs.unwrap_or(180),
        };
        out.insert(*role, Arc::from(build_agent(spec)));
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

    let mut schemas: HashMap<AgentRole, serde_json::Value> = HashMap::new();
    schemas.insert(AgentRole::Summary, summary);
    schemas.insert(AgentRole::TechnicalCorrectness, technical);
    schemas.insert(AgentRole::Novelty, novelty);
    schemas.insert(AgentRole::Reproducibility, reproducibility);
    schemas.insert(AgentRole::Citation, citation);
    schemas.insert(AgentRole::MetaReviewer, meta);

    let mut ladders: HashMap<AgentRole, grokrxiv_verifier::VerifierLadder> = HashMap::new();
    for (role, schema) in &schemas {
        ladders.insert(
            *role,
            grokrxiv_verifier::VerifierLadder::standard(Some(schema.clone())),
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
