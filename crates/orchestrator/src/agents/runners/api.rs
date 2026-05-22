//! `ApiRunner` — direct LLM provider API calls.
//!
//! The default backend for every role at RPT2 ship time. Wraps the existing
//! `LLMProvider` trait from `crates/llm-adapter`. Track A lifts the body of
//! the legacy `supervisor.rs::call_with_schema` helper into `ApiRunner::run`
//! so the supervisor can delegate via a configured role binding.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use grokrxiv_llm_adapter::{
    max_output_tokens_for, ChatRequest, ContentPart, LLMProvider, Message as LlmMessage,
    ResponseFormat, Role, ToolChatRequest,
};
use tracing::warn;

use crate::agents::extraction::ToolCtx;
use crate::agents::types::{
    AgentInput, AgentRun, AgentRunnerKind, AgentSchema, AgentSpec, Message, ToolCompletion,
    ToolSpec,
};
use crate::agents::AgentRunner;
use crate::runtime_config::direct_provider_api_allowed_from_env;

/// Holds a registry of `LLMProvider` impls keyed by name (`"claude"`,
/// `"openai"`, `"gemini"`, `"deepseek"`, etc.). The runner dispatches to the
/// right one based on `spec.provider`.
pub struct ApiRunner {
    providers: HashMap<String, Arc<dyn LLMProvider>>,
}

impl ApiRunner {
    /// Construct from an existing provider registry.
    pub fn new(providers: HashMap<String, Arc<dyn LLMProvider>>) -> Self {
        Self { providers }
    }
}

fn provider_api_allowed() -> bool {
    direct_provider_api_allowed_from_env()
}

fn ensure_provider_api_allowed(context: &str, spec: &AgentSpec) -> anyhow::Result<()> {
    if provider_api_allowed() {
        return Ok(());
    }
    anyhow::bail!(
        "{context}: direct provider API disabled for provider={} model={} role={:?}; \
         use --runner api or --extractor api to allow API billing, or use \
         --runner cli --extractor cli for local logged-in CLIs",
        spec.provider,
        spec.model,
        spec.role
    )
}

#[async_trait]
impl AgentRunner for ApiRunner {
    fn name(&self) -> &'static str {
        "api"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
        ensure_provider_api_allowed("ApiRunner.run", spec)?;
        let provider = self.providers.get(&spec.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "ApiRunner: no provider registered for `{}` (role={:?})",
                spec.provider,
                spec.role
            )
        })?;

        let schema = spec.schema.clone();
        let model = spec.model.clone();
        let system = input.system_prompt.clone();
        let prompt = input.user_prompt.clone();

        let make_req = |system_prompt: String, user_prompt: String, schema: AgentSchema| {
            ChatRequest {
                system: Some(system_prompt),
                messages: vec![LlmMessage {
                    role: Role::User,
                    content: vec![ContentPart::Text(user_prompt)],
                }],
                model: model.clone(),
                // Keep provider/model output caps in llm-adapter so this
                // runner does not guess limits from model-name substrings.
                max_tokens: max_output_tokens_for(&spec.provider, &model),
                temperature: 0.2,
                response_format: ResponseFormat::JsonSchema(schema.as_ref().clone()),
                cache_system: true,
            }
        };

        let started = Instant::now();
        let resp = provider
            .complete(make_req(system.clone(), prompt.clone(), schema.clone()))
            .await
            .map_err(|e| {
                // Always include the provider name + model + role so the
                // operator never has to guess which backend produced a 429.
                // Pre-RPT2 G we lost this context and mis-attributed a
                // transient burst-reject to "OpenAI rate limit"; that's
                // why the wrapping exists.
                anyhow::anyhow!(
                    "provider={} model={} role={:?}: {e:#}",
                    spec.provider,
                    spec.model,
                    spec.role
                )
            })?;

        if resp.text.trim().is_empty() {
            let raw_preview = serde_json::to_string(&resp.raw).unwrap_or_default();
            warn!(
                provider = %spec.provider,
                model = %spec.model,
                role = ?spec.role,
                content_len = resp.text.len(),
                tokens_in = resp.usage.tokens_in,
                tokens_out = resp.usage.tokens_out,
                finish_reason = ?resp.finish_reason,
                raw_preview = %&raw_preview.chars().take(800).collect::<String>(),
                "provider returned empty text body"
            );
        }

        let (output, usage) = match parse_and_validate(&resp.text, schema.as_ref()) {
            Ok(v) => (v, resp.usage),
            Err(first_err) => {
                // Single corrective retry: tell the model its prior output failed
                // parse/schema validation, classify the failure mode, and
                // mutate the system prompt. No `{"raw": ...}` fallback — if
                // the retry also fails the caller surfaces a hard error.
                let corrective_system = corrective_system_prompt(&system, &first_err);
                let retry = provider
                    .complete(make_req(corrective_system, prompt.clone(), schema.clone()))
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "provider={} model={} role={:?} (corrective retry): {e:#}",
                            spec.provider,
                            spec.model,
                            spec.role
                        )
                    })?;
                if retry.text.trim().is_empty() {
                    let raw_preview = serde_json::to_string(&retry.raw).unwrap_or_default();
                    warn!(
                        provider = %spec.provider,
                        model = %spec.model,
                        role = ?spec.role,
                        attempt = "corrective_retry",
                        content_len = retry.text.len(),
                        tokens_in = retry.usage.tokens_in,
                        tokens_out = retry.usage.tokens_out,
                        finish_reason = ?retry.finish_reason,
                        raw_preview = %&raw_preview.chars().take(800).collect::<String>(),
                        "provider returned empty text body on corrective retry"
                    );
                }
                match parse_and_validate(&retry.text, schema.as_ref()) {
                    Ok(v) => (v, retry.usage),
                    Err(e) => {
                        warn!(
                            provider = %spec.provider,
                            model = %spec.model,
                            role = ?spec.role,
                            first_finish = ?resp.finish_reason,
                            retry_finish = ?retry.finish_reason,
                            first_tokens_out = resp.usage.tokens_out,
                            retry_tokens_out = retry.usage.tokens_out,
                            first_len = resp.text.len(),
                            retry_len = retry.text.len(),
                            "parse failure after corrective retry"
                        );
                        return Err(anyhow::anyhow!(
                            "parse failure after corrective retry: first={first_err}; retry={e}; \
                             raw_first={raw_first:?}; raw_retry={raw_retry:?}",
                            raw_first = resp.text,
                            raw_retry = retry.text,
                        ));
                    }
                }
            }
        };
        let latency_ms = started.elapsed().as_millis() as i32;

        Ok(AgentRun {
            role: input.role.clone(),
            runner: AgentRunnerKind::Api,
            model: spec.model.clone(),
            output,
            verifier_status: None,
            verifier_notes: None,
            tokens_in: Some(usage.tokens_in as i32),
            tokens_out: Some(usage.tokens_out as i32),
            latency_ms,
            cache_hit: false,
            sandbox_ref: None,
        })
    }

    async fn complete_with_tools(
        &self,
        spec: &AgentSpec,
        messages: &[Message],
        tools: &[ToolSpec],
        _ctx: &ToolCtx<'_>,
    ) -> anyhow::Result<ToolCompletion> {
        ensure_provider_api_allowed("ApiRunner.complete_with_tools", spec)?;
        let provider = self.providers.get(&spec.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "ApiRunner.complete_with_tools: no provider registered for `{}` (role={:?})",
                spec.provider,
                spec.role
            )
        })?;
        let model_name = spec.model.clone();
        let req = ToolChatRequest {
            system: None,
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            model: model_name.clone(),
            // Tool-loop turns are usually short (one or a few tool calls), but
            // gemini's hidden-thinking step used to starve the response. With
            // thinkingBudget=0 the cap above is honoured; still per-model so
            // we don't overshoot a haiku request.
            max_tokens: max_output_tokens_for(&spec.provider, &model_name),
            temperature: 0.2,
        };
        provider.complete_with_tools(req).await.map_err(|e| {
            anyhow::anyhow!(
                "provider={} model={} role={:?} (tools): {e}",
                spec.provider,
                spec.model,
                spec.role,
            )
        })
    }
}

/// Try strict JSON parse; on failure, strip ```json fences and retry; on
/// failure again, return a classified error. Never returns `{"raw": ...}`.
fn parse_and_validate(
    s: &str,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, OutputValidationError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(OutputValidationError::new(
            OutputFailureKind::Empty,
            "provider returned an empty response body",
        ));
    }
    let stripped = strip_fences(trimmed);
    let parsed = serde_json::from_str::<serde_json::Value>(stripped).map_err(|e| {
        let kind = if looks_like_prose_wrapped_json(trimmed) {
            OutputFailureKind::ProseWrappedJson
        } else {
            OutputFailureKind::InvalidJson
        };
        OutputValidationError::new(kind, format!("not valid JSON: {e}"))
    })?;
    validate_parsed(parsed, schema)
}

#[derive(Debug)]
struct OutputValidationError {
    kind: OutputFailureKind,
    detail: String,
}

impl OutputValidationError {
    fn new(kind: OutputFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for OutputValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.detail)
    }
}

impl std::error::Error for OutputValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFailureKind {
    Empty,
    ProseWrappedJson,
    InvalidJson,
    SchemaValidation,
}

impl OutputFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty response",
            Self::ProseWrappedJson => "prose-wrapped JSON",
            Self::InvalidJson => "invalid JSON",
            Self::SchemaValidation => "schema validation failed",
        }
    }
}

fn corrective_system_prompt(original: &str, failure: &OutputValidationError) -> String {
    format!(
        "{original}\n\nCORRECTIVE RETRY: The previous response failed because {failure}. \
         Return exactly one JSON object that validates against the supplied JSON schema. \
         Do not include prose, markdown fences, code blocks, comments, or commentary."
    )
}

fn validate_parsed(
    parsed: serde_json::Value,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, OutputValidationError> {
    if schema.is_null()
        || (schema.is_object() && schema.as_object().map(|m| m.is_empty()).unwrap_or(false))
    {
        return Ok(parsed);
    }

    let validator = jsonschema::validator_for(schema).map_err(|e| {
        OutputValidationError::new(
            OutputFailureKind::SchemaValidation,
            format!("invalid role schema: {e}"),
        )
    })?;
    let errors: Vec<String> = validator
        .iter_errors(&parsed)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(OutputValidationError::new(
            OutputFailureKind::SchemaValidation,
            errors.join("; "),
        ));
    }
    Ok(parsed)
}

fn looks_like_prose_wrapped_json(s: &str) -> bool {
    let trimmed = s.trim_start();
    !trimmed.starts_with('{') && trimmed.contains('{') && trimmed.contains('}')
}

fn strip_fences(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_config::ALLOW_PROVIDER_API_ENV;
    use grokrxiv_llm_adapter::{
        ChatResponse, ClaudeProvider, FinishReason, GeminiProvider, OpenAIProvider, ProviderConfig,
        Role as LlmRole, ToolContent, ToolMessage, ToolSpec, Usage,
    };
    use serde_json::json;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use wiremock::matchers::{body_json_schema, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static API_ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct ProviderApiEnvGuard {
        prev: Option<String>,
    }

    impl ProviderApiEnvGuard {
        fn set(value: Option<&str>) -> Self {
            let prev = std::env::var(ALLOW_PROVIDER_API_ENV).ok();
            match value {
                Some(value) => std::env::set_var(ALLOW_PROVIDER_API_ENV, value),
                None => std::env::remove_var(ALLOW_PROVIDER_API_ENV),
            }
            Self { prev }
        }
    }

    impl Drop for ProviderApiEnvGuard {
        fn drop(&mut self) {
            match self.prev.as_deref() {
                Some(value) => std::env::set_var(ALLOW_PROVIDER_API_ENV, value),
                None => std::env::remove_var(ALLOW_PROVIDER_API_ENV),
            }
        }
    }

    fn tool_spec() -> ToolSpec {
        ToolSpec {
            name: "list_files".into(),
            description: "List files in workdir".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "glob": { "type": "string" } }
            }),
        }
    }

    fn kickoff_messages() -> Vec<Message> {
        vec![ToolMessage {
            role: LlmRole::User,
            content: vec![ToolContent::Text {
                text: "Inspect the bundle".into(),
            }],
        }]
    }

    fn tmp_workdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grokrxiv-api-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn spec(provider: &str, model: &str) -> AgentSpec {
        AgentSpec::api_default("summary", provider.to_string(), model.to_string())
    }

    fn make_ctx(workdir: &std::path::Path) -> ToolCtx<'_> {
        ToolCtx {
            workdir,
            semantic_ast: None,
            arxiv_id: "2401.00001v1",
            http: std::sync::Arc::new(reqwest::Client::new()),
        }
    }

    fn agent_input() -> AgentInput {
        AgentInput {
            paper_id: uuid::Uuid::new_v4(),
            review_id: uuid::Uuid::new_v4(),
            role: "summary".to_string(),
            content_hash_material: json!({ "paper": "x" }),
            artifact: json!({ "paper": "x" }),
            system_prompt: "Base system prompt.".to_string(),
            user_prompt: "Review this paper.".to_string(),
            source_bundle_path: None,
        }
    }

    fn required_tldr_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["tldr"],
            "additionalProperties": false,
            "properties": {
                "tldr": { "type": "string" }
            }
        })
    }

    fn response(text: &str) -> ChatResponse {
        ChatResponse {
            text: text.to_string(),
            finish_reason: FinishReason::Stop,
            usage: Usage {
                tokens_in: 3,
                tokens_out: 4,
                cache_hits: 0,
            },
            raw: json!({ "text": text }),
        }
    }

    struct RecordingProvider {
        responses: Mutex<VecDeque<ChatResponse>>,
        requests: Mutex<Vec<ChatRequest>>,
    }

    impl RecordingProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for RecordingProvider {
        async fn complete(
            &self,
            req: ChatRequest,
        ) -> Result<ChatResponse, grokrxiv_llm_adapter::LLMError> {
            self.requests.lock().unwrap().push(req);
            self.responses.lock().unwrap().pop_front().ok_or_else(|| {
                grokrxiv_llm_adapter::LLMError::Provider("no response queued".into())
            })
        }

        fn name(&self) -> &'static str {
            "recording"
        }

        fn context_window(&self) -> usize {
            128_000
        }
    }

    /// Verify that the Anthropic tools body has the right shape: a `tools`
    /// array with `name`/`description`/`input_schema`, and `messages` with
    /// tool_use / tool_result blocks intact.
    #[tokio::test]
    async fn translates_tools_to_anthropic_format() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(Some("1"));
        let server = MockServer::start().await;
        // wiremock captures the request body when we mount with `body_json_schema`,
        // but we want to inspect the exact body we sent. Use a regular Mock that
        // returns a canned tool-use response, then inspect server.received_requests.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "tu_1", "name": "list_files", "input": {} }
                ],
                "stop_reason": "tool_use",
                "usage": { "input_tokens": 5, "output_tokens": 6 }
            })))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            anthropic_api_key: Some("test".into()),
            ..ProviderConfig::default()
        };
        let provider = ClaudeProvider::from_config(&cfg)
            .unwrap()
            .with_base_url(server.uri());
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert("claude".into(), Arc::new(provider));
        let runner = ApiRunner::new(providers);

        let workdir = tmp_workdir();
        let ctx = make_ctx(&workdir);
        let resp = runner
            .complete_with_tools(
                &spec("claude", "claude-test"),
                &kickoff_messages(),
                &[tool_spec()],
                &ctx,
            )
            .await
            .expect("call should succeed against mock");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "list_files");

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        // tools[] with native shape
        assert!(body["tools"].is_array(), "tools must be an array: {body}");
        assert_eq!(body["tools"][0]["name"], "list_files");
        assert!(body["tools"][0]["input_schema"].is_object());
        // messages[] with first user text intact
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[tokio::test]
    async fn translates_tools_to_openai_format() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(Some("1"));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl_1",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "list_files", "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 6, "total_tokens": 11 }
            })))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            openai_api_key: Some("test".into()),
            ..ProviderConfig::default()
        };
        let provider = OpenAIProvider::from_config(&cfg)
            .unwrap()
            .with_base_url(server.uri());
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert("openai".into(), Arc::new(provider));
        let runner = ApiRunner::new(providers);

        let workdir = tmp_workdir();
        let ctx = make_ctx(&workdir);
        let resp = runner
            .complete_with_tools(
                &spec("openai", "gpt-5"),
                &kickoff_messages(),
                &[tool_spec()],
                &ctx,
            )
            .await
            .expect("openai call");
        assert_eq!(resp.tool_calls.len(), 1);

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "list_files");
        assert!(body["tools"][0]["function"]["parameters"].is_object());
        // messages preserves the user content
        let msg0 = &body["messages"][0];
        assert_eq!(msg0["role"], "user");
    }

    #[tokio::test]
    async fn translates_tools_to_gemini_format() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(Some("1"));
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [
                            { "functionCall": { "name": "list_files", "args": {} } }
                        ]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 6 }
            })))
            .mount(&server)
            .await;

        let cfg = ProviderConfig {
            google_api_key: Some("test".into()),
            ..ProviderConfig::default()
        };
        let provider = GeminiProvider::from_config(&cfg)
            .unwrap()
            .with_base_url(server.uri());
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert("gemini".into(), Arc::new(provider));
        let runner = ApiRunner::new(providers);

        let workdir = tmp_workdir();
        let ctx = make_ctx(&workdir);
        let resp = runner
            .complete_with_tools(
                &spec("gemini", "gemini-2.5-pro"),
                &kickoff_messages(),
                &[tool_spec()],
                &ctx,
            )
            .await
            .expect("gemini call");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "list_files");

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(
            body["tools"][0]["functionDeclarations"].is_array(),
            "expected tools[0].functionDeclarations[], got {body}"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "list_files"
        );
    }

    #[tokio::test]
    async fn refuses_direct_provider_api_when_not_enabled() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(None);
        let runner = ApiRunner::new(HashMap::new());
        let workdir = tmp_workdir();
        let ctx = make_ctx(&workdir);

        let err = runner
            .complete_with_tools(
                &spec("claude", "claude-test"),
                &kickoff_messages(),
                &[tool_spec()],
                &ctx,
            )
            .await
            .expect_err("direct API should be fail-closed by default");

        let msg = err.to_string();
        assert!(
            msg.contains("direct provider API disabled"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("--extractor api"),
            "error should name the explicit opt-in: {msg}"
        );
    }

    #[tokio::test]
    async fn corrective_retry_changes_system_prompt_for_prose_wrapped_json() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(Some("1"));
        let provider = Arc::new(RecordingProvider::new(vec![
            response("Here is the JSON:\n{\"tldr\":\"bad shape\"}"),
            response("{\"tldr\":\"fixed\"}"),
        ]));
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert("recording".into(), provider.clone());
        let runner = ApiRunner::new(providers);
        let mut spec = spec("recording", "gpt-5.5");
        spec.schema = required_tldr_schema().into();

        let run = runner
            .run(&spec, &agent_input())
            .await
            .expect("corrective retry should recover");

        assert_eq!(run.output["tldr"], "fixed");
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].system.as_deref(), Some("Base system prompt."));
        assert_ne!(
            requests[0].system, requests[1].system,
            "corrective retry must mutate the system prompt, not repeat the same prompt shape"
        );
        assert!(
            requests[1]
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("prose"),
            "retry prompt should classify prose-wrapped JSON: {:?}",
            requests[1].system
        );
    }

    #[tokio::test]
    async fn corrective_retry_changes_system_prompt_for_schema_failure() {
        let _lock = API_ENV_TEST_LOCK.lock().await;
        let _env = ProviderApiEnvGuard::set(Some("1"));
        let provider = Arc::new(RecordingProvider::new(vec![
            response("{\"wrong\":7}"),
            response("{\"tldr\":\"fixed\"}"),
        ]));
        let mut providers: HashMap<String, Arc<dyn LLMProvider>> = HashMap::new();
        providers.insert("recording".into(), provider.clone());
        let runner = ApiRunner::new(providers);
        let mut spec = spec("recording", "gpt-5.5");
        spec.schema = required_tldr_schema().into();

        runner
            .run(&spec, &agent_input())
            .await
            .expect("corrective retry should recover");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert_ne!(requests[0].system, requests[1].system);
        assert!(
            requests[1]
                .system
                .as_deref()
                .unwrap_or_default()
                .contains("schema validation failed"),
            "retry prompt should classify schema validation failure: {:?}",
            requests[1].system
        );
    }

    // Silence the unused-import warning for `body_json_schema` (kept around in
    // case future tests want JSON-Schema-shaped matchers).
    #[allow(dead_code)]
    fn _keep_unused() {
        let _ = body_json_schema::<serde_json::Value>;
    }
}
