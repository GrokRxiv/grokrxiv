//! Shared `AppState` injected into every axum handler.

use std::collections::HashMap;
use std::sync::Arc;

use grokrxiv_llm_adapter::{provider_by_name, LLMProvider, ProviderConfig};
use reqwest::Client;
use sqlx::PgPool;

use crate::arxiv_rate_limit::ArxivGate;
use crate::config::Config;

/// Providers keyed by short name (`claude`, `openai`, `gemini`, `vllm`).
pub type ProviderMap = HashMap<&'static str, Arc<dyn LLMProvider>>;
/// Per-agent provider and model routing table.
pub type RoleRouting = HashMap<grokrxiv_schemas::AgentRole, (Arc<dyn LLMProvider>, String)>;
/// Role-specific JSON schema documents.
#[cfg(feature = "grokrxiv-verifier")]
pub type AgentSchemaMap = HashMap<grokrxiv_schemas::AgentRole, serde_json::Value>;
/// Role-specific verifier ladders.
#[cfg(feature = "grokrxiv-verifier")]
pub type VerifierMap = HashMap<grokrxiv_schemas::AgentRole, grokrxiv_verifier::VerifierLadder>;

/// Registry of all configured LLM providers, keyed by short name.
///
/// The registry is built from whichever provider API keys are available in
/// the environment at boot. The Claude provider is the canonical default
/// (used by `/preview` and as the fallback for role routing when a YAML
/// declares a provider whose key is missing).
#[derive(Clone)]
pub struct ProviderRegistry {
    /// All providers reachable from this process, keyed by short name
    /// (`"claude" | "openai" | "gemini" | "vllm"`).
    pub by_name: Arc<ProviderMap>,
    /// Default provider used by the `/preview` route and as a fallback when a
    /// per-role provider isn't configured. Always Claude.
    pub default: Arc<dyn LLMProvider>,
}

impl ProviderRegistry {
    /// Build the registry from environment-provided keys.
    ///
    /// `ANTHROPIC_API_KEY` is required because the default provider is Claude;
    /// if it's missing the registry returns `None` and routes that need an LLM
    /// will respond with a clear error. Additional providers (OpenAI / Gemini
    /// / vLLM) are registered when their respective env vars are present.
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
    /// Per-role `(provider, model)` routing loaded from `agents/*.yaml`. When a
    /// role's YAML declares a provider whose API key isn't set, the routing
    /// entry falls back to the default Claude provider and the configured
    /// preview model so an unavailable provider cannot route a non-Claude model
    /// string into Claude.
    pub role_routing: Arc<RoleRouting>,
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
        let role_routing = Arc::new(build_role_routing(&providers, &fallback_model));

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
        })
    }
}

/// Minimal YAML shape we read from `agents/*.yaml`. The full agent config has
/// more fields (verifiers, retries, etc.) but only `provider` and `model`
/// drive routing.
#[derive(serde::Deserialize)]
struct AgentRouting {
    provider: String,
    model: String,
}

/// Walk `agents/*.yaml` and build the per-role `(provider, model)` map. If a
/// YAML declares a provider whose API key isn't set in this process, the
/// routing entry falls back to Claude with the orchestrator's
/// `PREVIEW_MODEL` (a Claude-compatible id) so the single-provider M1 path
/// still produces real calls. A warning is logged recording which provider
/// was declared in YAML so operators see the routing drift.
fn build_role_routing(providers: &Option<ProviderRegistry>, fallback_model: &str) -> RoleRouting {
    use grokrxiv_schemas::AgentRole;
    let mut out: RoleRouting = std::collections::HashMap::new();
    // No providers at all means /preview will error anyway; routing is empty.
    let Some(registry) = providers else {
        return out;
    };

    // The agents/ directory lives at the repo root next to crates/. The
    // include_str! pattern used for schemas would make these YAMLs part of the
    // binary, but we deliberately read them at runtime so operators can tweak
    // a deployed orchestrator's routing without rebuilding.
    let candidates: &[(AgentRole, &str)] = &[
        (AgentRole::Summary, "summary.yaml"),
        (
            AgentRole::TechnicalCorrectness,
            "technical_correctness.yaml",
        ),
        (AgentRole::Novelty, "novelty.yaml"),
        (AgentRole::Reproducibility, "reproducibility.yaml"),
        (AgentRole::Citation, "citation.yaml"),
        (AgentRole::MetaReviewer, "meta_reviewer.yaml"),
    ];

    // Resolve `agents/` relative to the workspace root. In a container the
    // binary's cwd is the repo root; in dev `cargo run` also runs from there.
    let agents_dir = std::env::var("GROKRXIV_AGENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("agents"));
    for (role, filename) in candidates {
        let path = agents_dir.join(filename);
        let routing = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_yaml::from_str::<AgentRouting>(&s) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        role = ?role,
                        path = %path.display(),
                        err = %e,
                        "could not parse agent yaml; falling back to default provider/model"
                    );
                    out.insert(
                        *role,
                        (registry.default.clone(), fallback_model.to_string()),
                    );
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!(
                    role = ?role,
                    path = %path.display(),
                    err = %e,
                    "agent yaml missing; falling back to default provider/model"
                );
                out.insert(
                    *role,
                    (registry.default.clone(), fallback_model.to_string()),
                );
                continue;
            }
        };

        let declared_provider = routing.provider.as_str();
        match registry.by_name.get(declared_provider) {
            Some(p) => {
                out.insert(*role, (p.clone(), routing.model));
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
