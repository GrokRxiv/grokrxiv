//! vLLM (OpenAI-compatible) provider.
//!
//! Points to an external OpenAI-compatible inference endpoint via
//! `VLLM_BASE_URL` (Modal / RunPod / Together / self-hosted GPU). Railway is
//! NOT assumed to host GPU inference. Vision support depends on the served
//! model; we expose it as a runtime flag rather than hard-coding.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::providers::openai::OpenAIProvider;
use crate::retry::with_backoff;
use crate::{ChatRequest, ChatResponse, LLMError, LLMProvider, ProviderConfig};

/// Self-hosted vLLM provider.
#[derive(Clone)]
pub struct VllmProvider {
    http: Arc<reqwest::Client>,
    base_url: String,
    api_key: Option<String>,
    vision: bool,
    context_window: usize,
}

impl VllmProvider {
    /// Build from a populated [`ProviderConfig`].
    pub fn from_config(cfg: &ProviderConfig) -> Result<Self, LLMError> {
        let base_url = cfg
            .vllm_base_url
            .clone()
            .ok_or_else(|| LLMError::Provider("VLLM_BASE_URL not set".into()))?;
        Ok(Self {
            http: cfg.http(),
            base_url,
            api_key: cfg.vllm_api_key.clone(),
            vision: false,
            context_window: 32_000,
        })
    }

    /// Override the base URL (used by tests).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Toggle vision support based on the served model.
    pub fn with_vision(mut self, vision: bool) -> Self {
        self.vision = vision;
        self
    }

    /// Override the advertised context window.
    pub fn with_context_window(mut self, window: usize) -> Self {
        self.context_window = window;
        self
    }
}

#[async_trait::async_trait]
impl LLMProvider for VllmProvider {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LLMError> {
        // Reuse the OpenAI body builder; vLLM is wire-compatible.
        let body = OpenAIProvider::build_body(&req);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let http = self.http.clone();
        let key = self.api_key.clone();
        with_backoff(|| {
            let http = http.clone();
            let url = url.clone();
            let key = key.clone();
            let body = body.clone();
            async move {
                let mut builder = http.post(&url).json(&body);
                if let Some(k) = &key {
                    builder = builder.bearer_auth(k);
                }
                let resp = builder.send().await.map_err(LLMError::from)?;
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
                OpenAIProvider::parse_response(value)
            }
        })
        .await
    }

    fn name(&self) -> &'static str {
        "vllm"
    }

    fn supports_vision(&self) -> bool {
        self.vision
    }

    fn context_window(&self) -> usize {
        self.context_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatRequest, ContentPart, Message, ResponseFormat, Role};

    #[test]
    fn vllm_uses_openai_body_shape() {
        let req = ChatRequest {
            system: None,
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text("hi".into())],
            }],
            model: "llama-3-70b".into(),
            max_tokens: 64,
            temperature: 0.0,
            response_format: ResponseFormat::Text,
            cache_system: false,
        };
        let body = OpenAIProvider::build_body(&req);
        assert_eq!(body["model"], "llama-3-70b");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
    }
}
