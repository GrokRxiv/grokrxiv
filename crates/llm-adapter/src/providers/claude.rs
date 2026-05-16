//! Anthropic Claude provider.
//!
//! Posts directly to `https://api.anthropic.com/v1/messages` with
//! `anthropic-version: 2023-06-01`. When `ChatRequest::cache_system` is true a
//! `cache_control: { type: "ephemeral" }` block is attached to the system
//! message to enable prompt caching.
//!
//! In addition, large user prompts (>= ~4096 characters, the conservative char
//! equivalent of Anthropic's 1024-token cache minimum) automatically receive a
//! `cache_control: { type: "ephemeral" }` hint on their final text block, so
//! repeated calls with the same long prompt prefix can read from cache at 0.1x
//! input price. Cache writes cost +25% and reads cost -90% versus normal input
//! tokens; cache entries live for ~5 minutes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::retry::with_backoff;
use crate::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, LLMError, LLMProvider, Message,
    ProviderConfig, ResponseFormat, Role, Usage,
};

/// Default Anthropic endpoint.
pub const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
/// Anthropic API version pinned for stability.
pub const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// Anthropic Claude provider.
#[derive(Clone)]
pub struct ClaudeProvider {
    http: Arc<reqwest::Client>,
    api_key: String,
    base_url: String,
}

impl ClaudeProvider {
    /// Build from a populated [`ProviderConfig`].
    pub fn from_config(cfg: &ProviderConfig) -> Result<Self, LLMError> {
        let api_key = cfg
            .anthropic_api_key
            .clone()
            .ok_or_else(|| LLMError::Provider("ANTHROPIC_API_KEY not set".into()))?;
        Ok(Self {
            http: cfg.http(),
            api_key,
            base_url: ANTHROPIC_MESSAGES_URL.to_string(),
        })
    }

    /// Override the base URL (used by tests against `wiremock`).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Build the request JSON body Anthropic expects.
    pub fn build_body(req: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = req.messages.iter().map(message_to_json).collect();

        // Cache long user prompts. Anthropic requires >= 1024 tokens for the
        // smallest cache slot; ~4 chars/token gives us a conservative
        // >= 4096-char threshold. We attach `cache_control` to the LAST text
        // block of the LAST user message, which is the standard idiom from
        // Anthropic's prompt-caching docs (everything up to and including the
        // marked block becomes the cacheable prefix).
        if let Some(last_user_idx) = messages
            .iter()
            .rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        {
            let text_total: usize = messages[last_user_idx]
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| {
                            if p.get("type").and_then(Value::as_str) == Some("text") {
                                p.get("text").and_then(Value::as_str).map(str::len)
                            } else {
                                None
                            }
                        })
                        .sum()
                })
                .unwrap_or(0);
            if text_total / 4 > 1024 {
                if let Some(arr) = messages[last_user_idx]
                    .get_mut("content")
                    .and_then(|c| c.as_array_mut())
                {
                    if let Some(last_text) = arr
                        .iter_mut()
                        .rev()
                        .find(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                    {
                        if let Some(obj) = last_text.as_object_mut() {
                            obj.insert(
                                "cache_control".into(),
                                json!({ "type": "ephemeral" }),
                            );
                        }
                    }
                }
            }
        }

        // `temperature` is rejected by some newer Claude models (e.g.
        // `claude-opus-4-7` returns 400 "temperature is deprecated for this
        // model"). Only forward the parameter for models that still accept it,
        // and otherwise let Anthropic apply its default.
        let supports_temperature = !req.model.contains("opus-4") && !req.model.contains("sonnet-4");
        let mut body = if supports_temperature {
            json!({
                "model": req.model,
                "max_tokens": req.max_tokens,
                "temperature": req.temperature,
                "messages": messages,
            })
        } else {
            json!({
                "model": req.model,
                "max_tokens": req.max_tokens,
                "messages": messages,
            })
        };

        // System prompt; cache hint when requested.
        if let Some(system) = &req.system {
            let extra = match &req.response_format {
                ResponseFormat::JsonSchema(schema) => Some(format!(
                    "\n\nRespond with a single JSON object that conforms to this JSON Schema:\n{}",
                    serde_json::to_string(schema).unwrap_or_else(|_| "{}".into())
                )),
                ResponseFormat::Text => None,
            };
            let combined = match extra {
                Some(suffix) => format!("{system}{suffix}"),
                None => system.clone(),
            };
            if req.cache_system {
                body["system"] = json!([
                    { "type": "text", "text": combined, "cache_control": { "type": "ephemeral" } }
                ]);
            } else {
                body["system"] = Value::String(combined);
            }
        } else if let ResponseFormat::JsonSchema(schema) = &req.response_format {
            body["system"] = Value::String(format!(
                "Respond with a single JSON object that conforms to this JSON Schema:\n{}",
                serde_json::to_string(schema).unwrap_or_else(|_| "{}".into())
            ));
        }

        body
    }

    fn parse_response(value: Value) -> Result<ChatResponse, LLMError> {
        let text = value
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let finish_reason = match value.get("stop_reason").and_then(|v| v.as_str()) {
            Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
            Some("max_tokens") => FinishReason::Length,
            Some("tool_use") => FinishReason::ToolUse,
            Some(_) => FinishReason::Other,
            None => FinishReason::Other,
        };
        let usage_obj = value.get("usage").cloned().unwrap_or(json!({}));
        let usage = Usage {
            tokens_in: usage_obj
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            tokens_out: usage_obj
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_hits: usage_obj
                .get("cache_read_input_tokens")
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

fn message_to_json(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<Value> = m
        .content
        .iter()
        .map(|p| match p {
            ContentPart::Text(s) => json!({ "type": "text", "text": s }),
            ContentPart::ImageUrl(u) => json!({
                "type": "image",
                "source": { "type": "url", "url": u }
            }),
            ContentPart::ImageBytes(img) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": img.mime,
                    "data": base64_encode(&img.bytes)
                }
            }),
        })
        .collect();
    json!({ "role": role, "content": content })
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[async_trait::async_trait]
impl LLMProvider for ClaudeProvider {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LLMError> {
        let body = Self::build_body(&req);
        let http = self.http.clone();
        let url = self.base_url.clone();
        let key = self.api_key.clone();
        with_backoff(|| {
            let http = http.clone();
            let url = url.clone();
            let key = key.clone();
            let body = body.clone();
            async move {
                let resp = http
                    .post(&url)
                    .header("x-api-key", &key)
                    .header("anthropic-version", ANTHROPIC_API_VERSION)
                    .header("content-type", "application/json")
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
                    return Err(LLMError::RateLimited(retry_after));
                }
                if status.is_server_error() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Provider(format!("{status}: {body_text}")));
                }
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Provider(format!("{status}: {body_text}")));
                }
                let value: Value = resp.json().await.map_err(LLMError::from)?;
                ClaudeProvider::parse_response(value)
            }
        })
        .await
    }

    fn name(&self) -> &'static str {
        "claude"
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        200_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRequest, ContentPart, Message, ResponseFormat, Role};

    fn req() -> ChatRequest {
        ChatRequest {
            system: Some("You are a careful reviewer.".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text("Hi".into())],
            }],
            model: "claude-opus-4-7".into(),
            max_tokens: 1024,
            temperature: 0.2,
            response_format: ResponseFormat::Text,
            cache_system: false,
        }
    }

    #[test]
    fn body_includes_required_anthropic_fields() {
        let body = ClaudeProvider::build_body(&req());
        assert_eq!(body["model"], "claude-opus-4-7");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["system"], "You are a careful reviewer.");
    }

    #[test]
    fn cache_system_emits_cache_control() {
        let mut r = req();
        r.cache_system = true;
        let body = ClaudeProvider::build_body(&r);
        let sys = &body["system"];
        assert!(sys.is_array(), "system must be an array when cached");
        assert_eq!(sys[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn json_schema_appends_instruction() {
        let mut r = req();
        r.response_format = ResponseFormat::JsonSchema(serde_json::json!({"type":"object"}));
        let body = ClaudeProvider::build_body(&r);
        let sys = body["system"].as_str().unwrap_or_default();
        assert!(sys.contains("JSON Schema"));
    }

    #[test]
    fn claude_request_caches_long_user_prompt() {
        // >4096-char user prompt — should attach cache_control to its last
        // text block.
        let big = "x".repeat(5000);
        let r = ChatRequest {
            system: Some("sys".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text(big)],
            }],
            model: "claude-opus-4-7".into(),
            max_tokens: 1024,
            temperature: 0.2,
            response_format: ResponseFormat::Text,
            cache_system: false,
        };
        let body = ClaudeProvider::build_body(&r);
        let parts = body["messages"][0]["content"]
            .as_array()
            .expect("content array");
        let last = parts.last().expect("at least one text part");
        assert_eq!(
            last["cache_control"]["type"], "ephemeral",
            "expected ephemeral cache_control on long user prompt: {body}"
        );
    }

    #[test]
    fn claude_request_does_not_cache_short_user_prompt() {
        // Short prompt (~2 chars) — no cache_control on user message.
        let body = ClaudeProvider::build_body(&req());
        let parts = body["messages"][0]["content"]
            .as_array()
            .expect("content array");
        for p in parts {
            assert!(
                p.get("cache_control").is_none(),
                "short prompt unexpectedly got cache_control: {p}"
            );
        }
    }

    #[test]
    fn parse_response_extracts_text_and_usage() {
        let raw = serde_json::json!({
            "content": [{ "type": "text", "text": "hello" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 7, "output_tokens": 3, "cache_read_input_tokens": 2 }
        });
        let r = ClaudeProvider::parse_response(raw).unwrap();
        assert_eq!(r.text, "hello");
        assert!(matches!(r.finish_reason, FinishReason::Stop));
        assert_eq!(r.usage.tokens_in, 7);
        assert_eq!(r.usage.tokens_out, 3);
        assert_eq!(r.usage.cache_hits, 2);
    }
}
