//! OpenAI (chat-completions) provider.
//!
//! Posts to `https://api.openai.com/v1/chat/completions`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::retry::with_backoff;
use crate::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, LLMError, LLMProvider, Message,
    ProviderConfig, ResponseFormat, Role, Usage,
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

fn openai_error_code(body_text: &str) -> Option<String> {
    serde_json::from_str::<Value>(body_text)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.get("code")).cloned())
        .and_then(|v| v.as_str().map(ToOwned::to_owned))
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
