//! `ApiRunner` — direct LLM provider API calls.
//!
//! The default backend for every role at RPT2 ship time. Wraps the existing
//! `LLMProvider` trait from `crates/llm-adapter`. Track A lifts the body of
//! the legacy `supervisor.rs::call_with_schema` helper into `ApiRunner::run`
//! so the supervisor can delegate via `ReviewAgent::run`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use grokrxiv_llm_adapter::{
    ChatRequest, ContentPart, LLMProvider, Message as LlmMessage, ResponseFormat, Role,
    ToolChatRequest,
};
use tracing::warn;

use crate::agents::extraction::ToolCtx;
use crate::agents::traits::AgentRunner;
use crate::agents::types::{
    AgentInput, AgentRun, AgentRunnerKind, AgentSpec, Message, ToolCompletion, ToolSpec,
};

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

#[async_trait]
impl AgentRunner for ApiRunner {
    fn name(&self) -> &'static str {
        "api"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
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

        let make_req = |user_prompt: String, schema: serde_json::Value| ChatRequest {
            system: Some(system.clone()),
            messages: vec![LlmMessage {
                role: Role::User,
                content: vec![ContentPart::Text(user_prompt)],
            }],
            model: model.clone(),
            max_tokens: 6_000,
            temperature: 0.2,
            response_format: ResponseFormat::JsonSchema(schema),
            cache_system: true,
        };

        let started = Instant::now();
        let resp = provider
            .complete(make_req(prompt.clone(), schema.clone()))
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

        let (output, usage) = match parse_strict_json(&resp.text) {
            Ok(v) => (v, resp.usage),
            Err(first_err) => {
                // Single corrective retry: tell the model its prior output failed
                // schema validation and ask for strict JSON. No `{"raw": ...}`
                // fallback — if the retry also fails the caller surfaces a hard
                // error and the verifier records the parse error in
                // `verifier_notes.parse_error`.
                let corrective = format!(
                    "{prompt}\n\nYour previous output did not validate against the schema; \
                     return strict JSON only, with no surrounding prose, code fences, or commentary."
                );
                let retry = provider
                    .complete(make_req(corrective, schema.clone()))
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
                match parse_strict_json(&retry.text) {
                    Ok(v) => (v, retry.usage),
                    Err(e) => {
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
            role: input.role,
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
        let provider = self.providers.get(&spec.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "ApiRunner.complete_with_tools: no provider registered for `{}` (role={:?})",
                spec.provider,
                spec.role
            )
        })?;
        let req = ToolChatRequest {
            system: None,
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            model: spec.model.clone(),
            max_tokens: 4_000,
            temperature: 0.2,
        };
        provider
            .complete_with_tools(req)
            .await
            .map_err(|e| anyhow::anyhow!(
                "provider={} model={} role={:?} (tools): {e}",
                spec.provider,
                spec.model,
                spec.role,
            ))
    }
}

/// Try strict JSON parse; on failure, strip ```json fences and retry; on
/// failure again, return `Err`. Never returns `{"raw": ...}`.
fn parse_strict_json(s: &str) -> anyhow::Result<serde_json::Value> {
    let trimmed = s.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => Ok(v),
        Err(_) => {
            let stripped = strip_fences(trimmed);
            serde_json::from_str::<serde_json::Value>(stripped)
                .map_err(|e| anyhow::anyhow!("not valid JSON: {e}"))
        }
    }
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
    use grokrxiv_llm_adapter::{
        ClaudeProvider, GeminiProvider, OpenAIProvider, ProviderConfig, Role as LlmRole,
        ToolContent, ToolMessage, ToolSpec,
    };
    use grokrxiv_schemas::AgentRole;
    use serde_json::json;
    use std::path::PathBuf;
    use wiremock::matchers::{body_json_schema, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        p.push(format!("grokrxiv-api-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn spec(provider: &str, model: &str) -> AgentSpec {
        AgentSpec::api_default(AgentRole::Summary, provider.to_string(), model.to_string())
    }

    fn make_ctx(workdir: &std::path::Path) -> ToolCtx<'_> {
        ToolCtx {
            workdir,
            semantic_ast: None,
            arxiv_id: "2401.00001v1",
            http: std::sync::Arc::new(reqwest::Client::new()),
        }
    }

    /// Verify that the Anthropic tools body has the right shape: a `tools`
    /// array with `name`/`description`/`input_schema`, and `messages` with
    /// tool_use / tool_result blocks intact.
    #[tokio::test]
    async fn translates_tools_to_anthropic_format() {
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
        let provider = ClaudeProvider::from_config(&cfg).unwrap().with_base_url(server.uri());
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
        let provider = OpenAIProvider::from_config(&cfg).unwrap().with_base_url(server.uri());
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
        let provider = GeminiProvider::from_config(&cfg).unwrap().with_base_url(server.uri());
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
        assert!(body["tools"][0]["functionDeclarations"].is_array(),
            "expected tools[0].functionDeclarations[], got {body}");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "list_files"
        );
    }

    // Silence the unused-import warning for `body_json_schema` (kept around in
    // case future tests want JSON-Schema-shaped matchers).
    #[allow(dead_code)]
    fn _keep_unused() {
        let _ = body_json_schema::<serde_json::Value>;
    }
}
