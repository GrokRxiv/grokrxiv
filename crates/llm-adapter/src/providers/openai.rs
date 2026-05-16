//! OpenAI (chat-completions) provider.
//!
//! Posts to `https://api.openai.com/v1/chat/completions`.
//!
//! ### Prompt caching
//!
//! OpenAI applies automatic prompt caching to the `gpt-4o`, `gpt-4o-mini`,
//! `o1`/`o1-mini`, and `gpt-5.x` families for any prefix >= 1024 tokens — no
//! header opt-in required. To improve cache routing (so similar prompts hash
//! to the same shard) we set the optional `prompt_cache_key` field to a
//! sha256 of `system_message + first 512 chars of user prompt`. Older models
//! that do not benefit from auto-caching skip the field entirely.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::retry::with_backoff;
use crate::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, LLMError, LLMProvider, Message,
    ProviderConfig, ProviderToolCall, ResponseFormat, Role, ToolChatRequest, ToolCompletion,
    ToolContent, ToolMessage, Usage,
};

/// Default OpenAI base URL.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI provider.
#[derive(Clone)]
pub struct OpenAIProvider {
    http: Arc<reqwest::Client>,
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    /// Build from a populated [`ProviderConfig`].
    pub fn from_config(cfg: &ProviderConfig) -> Result<Self, LLMError> {
        let api_key = cfg
            .openai_api_key
            .clone()
            .ok_or_else(|| LLMError::Provider("OPENAI_API_KEY not set".into()))?;
        Ok(Self {
            http: cfg.http(),
            api_key,
            base_url: OPENAI_BASE_URL.to_string(),
        })
    }

    /// Override the base URL (used by tests).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Construct the chat-completions request body.
    pub fn build_body(req: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if let Some(s) = &req.system {
            messages.push(json!({ "role": "system", "content": s }));
        }
        for m in &req.messages {
            messages.push(openai_message(m));
        }

        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });
        if let ResponseFormat::JsonSchema(schema) = &req.response_format {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "structured_output",
                    "strict": true,
                    "schema": schema
                }
            });
        }
        body
    }

    /// Construct the request body sent to OpenAI's Chat Completions API.
    ///
    /// `build_body` intentionally keeps the older OpenAI-compatible shape
    /// because the vLLM provider reuses it. Native OpenAI GPT-5-family models
    /// use `max_completion_tokens` and reasoning controls instead.
    pub fn build_openai_body(req: &ChatRequest) -> Value {
        let mut body = Self::build_body(req);
        body["max_completion_tokens"] = json!(req.max_tokens);
        body.as_object_mut()
            .expect("openai body is an object")
            .remove("max_tokens");

        if uses_openai_reasoning_controls(&req.model) {
            body["reasoning_effort"] = json!("medium");
            body.as_object_mut()
                .expect("openai body is an object")
                .remove("temperature");
        }

        if supports_prompt_cache(&req.model) {
            body["prompt_cache_key"] = json!(prompt_cache_key(req));
        }

        body
    }

    pub(crate) fn parse_response(value: Value) -> Result<ChatResponse, LLMError> {
        let choice = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .cloned()
            .unwrap_or(Value::Null);
        let text = choice
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let finish_reason = match choice.get("finish_reason").and_then(Value::as_str) {
            Some("stop") => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        };
        let usage_obj = value.get("usage").cloned().unwrap_or(json!({}));
        let usage = Usage {
            tokens_in: usage_obj
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            tokens_out: usage_obj
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_hits: usage_obj
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        };
        Ok(ChatResponse {
            text,
            finish_reason,
            usage,
            raw: value,
        })
    }
}

fn openai_message(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    // Use the array-form content so we can mix text and images uniformly.
    let parts: Vec<Value> = m
        .content
        .iter()
        .map(|p| match p {
            ContentPart::Text(s) => json!({ "type": "text", "text": s }),
            ContentPart::ImageUrl(u) => json!({
                "type": "image_url",
                "image_url": { "url": u }
            }),
            ContentPart::ImageBytes(img) => json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{};base64,{}", img.mime, base64_encode(&img.bytes))
                }
            }),
        })
        .collect();
    json!({ "role": role, "content": parts })
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn uses_openai_reasoning_controls(model: &str) -> bool {
    model.starts_with("gpt-5") || model.starts_with('o')
}

/// Whether OpenAI applies automatic prompt caching for this model family. Only
/// the families documented in OpenAI's prompt-caching guide qualify; passing a
/// `prompt_cache_key` to other models is harmless but pointless, so we skip
/// it for clarity.
fn supports_prompt_cache(model: &str) -> bool {
    model.starts_with("gpt-5")
        || model.starts_with("gpt-4o")
        || model.starts_with("o1")
}

/// Build the `prompt_cache_key` routing hint. Hashes the system prompt plus
/// the first 512 chars of the first user-text segment, hex-encoded. Stable
/// across runs for the same template even when the long body differs.
fn prompt_cache_key(req: &ChatRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if let Some(sys) = &req.system {
        hasher.update(sys.as_bytes());
    }
    hasher.update(b":");
    // First text segment of the first user message — enough to discriminate
    // task templates, while staying cheap.
    let user_head = req
        .messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .and_then(|m| {
            m.content.iter().find_map(|p| match p {
                ContentPart::Text(s) => Some(s.as_str()),
                _ => None,
            })
        })
        .unwrap_or("");
    let take = user_head.len().min(512);
    // Slice on a UTF-8 boundary to avoid panics on multi-byte chars near 512.
    let mut end = take;
    while end > 0 && !user_head.is_char_boundary(end) {
        end -= 1;
    }
    hasher.update(user_head[..end].as_bytes());
    hex::encode(hasher.finalize())
}

fn openai_error_code(body_text: &str) -> Option<String> {
    serde_json::from_str::<Value>(body_text)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.get("code")).cloned())
        .and_then(|v| v.as_str().map(ToOwned::to_owned))
}

impl OpenAIProvider {
    /// Build the OpenAI chat-completions request body for a tool-using turn.
    pub fn build_tools_body(req: &ToolChatRequest) -> Value {
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if let Some(s) = &req.system {
            messages.push(json!({ "role": "system", "content": s }));
        }
        for m in &req.messages {
            push_openai_tool_messages(m, &mut messages);
        }

        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "tools": tools,
            "max_completion_tokens": req.max_tokens,
        });
        if !uses_openai_reasoning_controls(&req.model) {
            body["temperature"] = json!(req.temperature);
        } else {
            body["reasoning_effort"] = json!("medium");
        }
        body
    }

    /// Parse an OpenAI tools-API response into a [`ToolCompletion`].
    pub fn parse_tools_response(value: Value) -> Result<ToolCompletion, LLMError> {
        let choice = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .cloned()
            .unwrap_or(Value::Null);
        let message = choice.get("message").cloned().unwrap_or(Value::Null);
        let text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut tool_calls: Vec<ProviderToolCall> = Vec::new();
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let id = c
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let function = c.get("function").cloned().unwrap_or(Value::Null);
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let args_str = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let arguments: Value =
                    serde_json::from_str(args_str).unwrap_or(Value::Null);
                tool_calls.push(ProviderToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }
        let finish_reason = match choice.get("finish_reason").and_then(Value::as_str) {
            Some("stop") => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        };
        let usage_obj = value.get("usage").cloned().unwrap_or(json!({}));
        let usage = Usage {
            tokens_in: usage_obj
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            tokens_out: usage_obj
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_hits: usage_obj
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        };
        Ok(ToolCompletion {
            tool_calls,
            text,
            finish_reason,
            usage,
            raw: value,
        })
    }
}

fn push_openai_tool_messages(m: &ToolMessage, out: &mut Vec<Value>) {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut text_buf = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut emitted_tool_results = false;
    for part in &m.content {
        match part {
            ToolContent::Text { text } => {
                text_buf.push_str(text);
            }
            ToolContent::ToolUse { id, name, input } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            }
            ToolContent::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": match content {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    },
                }));
                emitted_tool_results = true;
            }
        }
    }
    if emitted_tool_results && text_buf.is_empty() && tool_calls.is_empty() {
        return;
    }
    let mut msg = json!({ "role": role, "content": text_buf });
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }
    out.push(msg);
}

#[async_trait::async_trait]
impl LLMProvider for OpenAIProvider {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LLMError> {
        let body = Self::build_openai_body(&req);
        let url = format!("{}/chat/completions", self.base_url);
        let http = self.http.clone();
        let key = self.api_key.clone();
        with_backoff(|| {
            let http = http.clone();
            let url = url.clone();
            let key = key.clone();
            let body = body.clone();
            async move {
                let resp = http
                    .post(&url)
                    .bearer_auth(&key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(LLMError::from)?;
                let status = resp.status();
                if status.as_u16() == 429 {
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let body_text = resp.text().await.unwrap_or_default();
                    if openai_error_code(&body_text).as_deref() == Some("insufficient_quota") {
                        return Err(LLMError::QuotaExceeded(format!("{status}: {body_text}")));
                    }
                    return Err(LLMError::RateLimited(retry_after));
                }
                if status.is_server_error() || !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Provider(format!("{status}: {body_text}")));
                }
                let value: Value = resp.json().await.map_err(LLMError::from)?;
                OpenAIProvider::parse_response(value)
            }
        })
        .await
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        128_000
    }

    async fn complete_with_tools(
        &self,
        req: ToolChatRequest,
    ) -> Result<ToolCompletion, LLMError> {
        let body = Self::build_tools_body(&req);
        let url = format!("{}/chat/completions", self.base_url);
        let http = self.http.clone();
        let key = self.api_key.clone();
        with_backoff(|| {
            let http = http.clone();
            let url = url.clone();
            let key = key.clone();
            let body = body.clone();
            async move {
                let resp = http
                    .post(&url)
                    .bearer_auth(&key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(LLMError::from)?;
                let status = resp.status();
                if status.as_u16() == 429 {
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let body_text = resp.text().await.unwrap_or_default();
                    if openai_error_code(&body_text).as_deref() == Some("insufficient_quota") {
                        return Err(LLMError::QuotaExceeded(format!("{status}: {body_text}")));
                    }
                    return Err(LLMError::RateLimited(retry_after));
                }
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Provider(format!("{status}: {body_text}")));
                }
                let value: Value = resp.json().await.map_err(LLMError::from)?;
                OpenAIProvider::parse_tools_response(value)
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentPart, Message, ResponseFormat, Role};

    fn req() -> ChatRequest {
        ChatRequest {
            system: Some("You review papers.".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text("Body".into())],
            }],
            model: "gpt-5-codex".into(),
            max_tokens: 256,
            temperature: 0.1,
            response_format: ResponseFormat::Text,
            cache_system: false,
        }
    }

    #[test]
    fn system_message_prepended() {
        let body = OpenAIProvider::build_body(&req());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You review papers.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["model"], "gpt-5-codex");
    }

    #[test]
    fn json_schema_sets_response_format() {
        let mut r = req();
        r.response_format = ResponseFormat::JsonSchema(serde_json::json!({ "type": "object" }));
        let body = OpenAIProvider::build_body(&r);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["type"],
            "object"
        );
    }

    #[test]
    fn openai_gpt5_body_uses_current_token_and_reasoning_fields() {
        let mut r = req();
        r.model = "gpt-5.5".into();
        let body = OpenAIProvider::build_openai_body(&r);
        assert_eq!(body["max_completion_tokens"], 256);
        assert!(body.get("max_tokens").is_none());
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn extracts_openai_error_code() {
        let body = serde_json::json!({
            "error": {
                "message": "You exceeded your current quota",
                "code": "insufficient_quota"
            }
        })
        .to_string();
        assert_eq!(
            openai_error_code(&body),
            Some("insufficient_quota".to_string())
        );
    }

    #[test]
    fn openai_request_sets_prompt_cache_key_for_gpt5() {
        let mut r = req();
        r.model = "gpt-5.5".into();
        let body = OpenAIProvider::build_openai_body(&r);
        let key = body["prompt_cache_key"]
            .as_str()
            .expect("prompt_cache_key should be set for gpt-5 models");
        // sha256 hex = 64 lowercase hex chars.
        assert_eq!(key.len(), 64, "expected 64-char hex sha256, got {key}");
        assert!(
            key.chars().all(|c| c.is_ascii_hexdigit()),
            "key not hex: {key}"
        );
    }

    #[test]
    fn openai_request_sets_prompt_cache_key_for_gpt4o_and_o1() {
        for model in ["gpt-4o", "gpt-4o-mini", "o1-mini"] {
            let mut r = req();
            r.model = model.into();
            let body = OpenAIProvider::build_openai_body(&r);
            assert!(
                body.get("prompt_cache_key").and_then(Value::as_str).is_some(),
                "{model} should set prompt_cache_key"
            );
        }
    }

    #[test]
    fn openai_request_skips_prompt_cache_key_for_old_model() {
        let mut r = req();
        r.model = "gpt-3.5-turbo".into();
        let body = OpenAIProvider::build_openai_body(&r);
        assert!(
            body.get("prompt_cache_key").is_none(),
            "gpt-3.5-turbo should NOT set prompt_cache_key, got {body}"
        );
    }

    #[test]
    fn openai_prompt_cache_key_is_stable_for_identical_prompts() {
        let mut r1 = req();
        r1.model = "gpt-5.5".into();
        let mut r2 = req();
        r2.model = "gpt-5.5".into();
        let b1 = OpenAIProvider::build_openai_body(&r1);
        let b2 = OpenAIProvider::build_openai_body(&r2);
        assert_eq!(b1["prompt_cache_key"], b2["prompt_cache_key"]);
    }

    #[test]
    fn parse_response_reads_first_choice() {
        let raw = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "hi" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 2 }
        });
        let r = OpenAIProvider::parse_response(raw).unwrap();
        assert_eq!(r.text, "hi");
        assert!(matches!(r.finish_reason, FinishReason::Stop));
        assert_eq!(r.usage.tokens_in, 11);
        assert_eq!(r.usage.tokens_out, 2);
    }
}
