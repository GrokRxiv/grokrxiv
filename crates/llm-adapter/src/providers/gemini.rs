//! Google Gemini provider.
//!
//! Talks to the v1beta REST endpoint:
//! `https://generativelanguage.googleapis.com/v1beta/models/<model>:generateContent?key=<API_KEY>`.
//! Supports vision via `inline_data` parts.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::retry::with_backoff;
use crate::{
    ChatRequest, ChatResponse, ContentPart, FinishReason, LLMError, LLMProvider, ProviderConfig,
    ResponseFormat, Role, Usage,
};

/// Default base URL for Gemini.
pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Google Gemini provider.
#[derive(Clone)]
pub struct GeminiProvider {
    http: Arc<reqwest::Client>,
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    /// Build from a populated [`ProviderConfig`].
    pub fn from_config(cfg: &ProviderConfig) -> Result<Self, LLMError> {
        let api_key = cfg
            .google_api_key
            .clone()
            .ok_or_else(|| LLMError::Provider("GOOGLE_GENERATIVE_AI_API_KEY not set".into()))?;
        Ok(Self {
            http: cfg.http(),
            api_key,
            base_url: GEMINI_BASE_URL.to_string(),
        })
    }

    /// Override the base URL (used by tests).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Build the Gemini `generateContent` body.
    pub fn build_body(req: &ChatRequest) -> Value {
        let contents: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "model",
                };
                let parts: Vec<Value> = m
                    .content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text(s) => json!({ "text": s }),
                        ContentPart::ImageUrl(u) => json!({ "file_data": { "file_uri": u } }),
                        ContentPart::ImageBytes(img) => json!({
                            "inline_data": {
                                "mime_type": img.mime,
                                "data": base64_encode(&img.bytes)
                            }
                        }),
                    })
                    .collect();
                json!({ "role": role, "parts": parts })
            })
            .collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "temperature": req.temperature,
                "maxOutputTokens": req.max_tokens
            }
        });

        if let Some(sys) = &req.system {
            body["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
        }

        if let ResponseFormat::JsonSchema(schema) = &req.response_format {
            body["generationConfig"]["responseMimeType"] = json!("application/json");
            body["generationConfig"]["responseSchema"] = sanitize_schema_for_gemini(schema.clone());
        }

        body
    }

    fn parse_response(value: Value) -> Result<ChatResponse, LLMError> {
        let candidate = value
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .cloned()
            .unwrap_or(Value::Null);
        let text = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let finish_reason = match candidate.get("finishReason").and_then(Value::as_str) {
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::Length,
            Some("SAFETY") | Some("RECITATION") => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        };
        let usage_obj = value.get("usageMetadata").cloned().unwrap_or(json!({}));
        let usage = Usage {
            tokens_in: usage_obj
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            tokens_out: usage_obj
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            cache_hits: usage_obj
                .get("cachedContentTokenCount")
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

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Translate an OpenAI-compatible JSON Schema into the dialect Gemini's
/// `responseSchema` field accepts. Gemini's proto-backed schema validator
/// rejects JSON-Schema metadata (`$id`, `$schema`, `additionalProperties`) and
/// does not understand the nullable type-union form `"type": ["X", "null"]` —
/// it wants `"type": "X"` paired with a sibling `"nullable": true`. This walker
/// performs that rewrite recursively over nested `properties` / `items`.
fn sanitize_schema_for_gemini(schema: Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            let mut nullable = false;
            for (k, v) in map {
                match k.as_str() {
                    "$id" | "$schema" | "additionalProperties" => {
                        // Drop: Gemini's protobuf validator rejects these keys.
                    }
                    "type" => {
                        if let Value::Array(arr) = v {
                            let mut non_null: Vec<Value> = Vec::with_capacity(arr.len());
                            for t in arr {
                                if t.as_str() == Some("null") {
                                    nullable = true;
                                } else {
                                    non_null.push(t);
                                }
                            }
                            match non_null.len() {
                                1 => {
                                    out.insert(
                                        "type".into(),
                                        non_null.into_iter().next().expect("len==1"),
                                    );
                                }
                                n if n > 1 => {
                                    out.insert("type".into(), Value::Array(non_null));
                                }
                                _ => {
                                    // All-null union: nothing useful to preserve.
                                }
                            }
                        } else {
                            out.insert("type".into(), v);
                        }
                    }
                    "properties" => {
                        if let Value::Object(props) = v {
                            let mut new_props = serde_json::Map::with_capacity(props.len());
                            for (pk, pv) in props {
                                new_props.insert(pk, sanitize_schema_for_gemini(pv));
                            }
                            out.insert("properties".into(), Value::Object(new_props));
                        } else {
                            out.insert("properties".into(), v);
                        }
                    }
                    "items" => {
                        out.insert("items".into(), sanitize_schema_for_gemini(v));
                    }
                    _ => {
                        out.insert(k, v);
                    }
                }
            }
            if nullable {
                out.insert("nullable".into(), Value::Bool(true));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(sanitize_schema_for_gemini)
                .collect(),
        ),
        other => other,
    }
}

#[async_trait::async_trait]
impl LLMProvider for GeminiProvider {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LLMError> {
        let body = Self::build_body(&req);
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url, req.model, self.api_key
        );
        let http = self.http.clone();
        with_backoff(|| {
            let http = http.clone();
            let url = url.clone();
            let body = body.clone();
            async move {
                let resp = http
                    .post(&url)
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
                if status.is_server_error() || !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return Err(LLMError::Provider(format!("{status}: {body_text}")));
                }
                let value: Value = resp.json().await.map_err(LLMError::from)?;
                GeminiProvider::parse_response(value)
            }
        })
        .await
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn supports_vision(&self) -> bool {
        true
    }

    fn context_window(&self) -> usize {
        1_000_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentPart, Message, ResponseFormat, Role};

    fn req() -> ChatRequest {
        ChatRequest {
            system: Some("Summarise.".into()),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text("Body".into())],
            }],
            model: "gemini-2.5-pro".into(),
            max_tokens: 256,
            temperature: 0.3,
            response_format: ResponseFormat::Text,
            cache_system: false,
        }
    }

    #[test]
    fn body_shape_is_gemini_compatible() {
        let body = GeminiProvider::build_body(&req());
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Body");
        let t = body["generationConfig"]["temperature"].as_f64().unwrap();
        assert!((t - 0.3).abs() < 1e-4);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 256);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Summarise.");
    }

    #[test]
    fn json_schema_sets_response_schema() {
        let mut r = req();
        r.response_format = ResponseFormat::JsonSchema(serde_json::json!({ "type": "object" }));
        let body = GeminiProvider::build_body(&r);
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(body["generationConfig"]["responseSchema"]["type"], "object");
    }

    #[test]
    fn sanitize_strips_metadata_and_rewrites_nullable_unions() {
        // OpenAI-style schema with all the bits Gemini chokes on.
        let openai_form = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://grokrxiv.org/schemas/x.schema.json",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "url": { "type": ["string", "null"] },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "key": { "type": "string" }
                        }
                    }
                }
            },
            "required": ["url", "items"]
        });
        let g = super::sanitize_schema_for_gemini(openai_form);
        // Metadata stripped.
        assert!(g.get("$schema").is_none());
        assert!(g.get("$id").is_none());
        assert!(g.get("additionalProperties").is_none());
        // Nested additionalProperties stripped too.
        assert!(g["properties"]["items"]["items"]
            .get("additionalProperties")
            .is_none());
        // Type-union rewritten to scalar + nullable: true.
        assert_eq!(g["properties"]["url"]["type"], "string");
        assert_eq!(g["properties"]["url"]["nullable"], true);
        // Non-nullable fields unchanged.
        assert_eq!(g["properties"]["items"]["items"]["properties"]["key"]["type"], "string");
        // required preserved.
        assert_eq!(g["required"], serde_json::json!(["url", "items"]));
    }

    #[test]
    fn parse_response_picks_first_candidate_text() {
        let raw = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "ok" }] },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 4
            }
        });
        let r = GeminiProvider::parse_response(raw).unwrap();
        assert_eq!(r.text, "ok");
        assert!(matches!(r.finish_reason, FinishReason::Stop));
        assert_eq!(r.usage.tokens_in, 5);
        assert_eq!(r.usage.tokens_out, 4);
    }
}
