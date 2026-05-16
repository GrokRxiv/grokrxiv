//! GrokRxiv LLM adapter.
//!
//! Defines a single [`LLMProvider`] trait that every concrete provider
//! ([`providers::claude::ClaudeProvider`], [`providers::gemini::GeminiProvider`],
//! [`providers::openai::OpenAIProvider`], [`providers::vllm::VllmProvider`])
//! implements, plus the request/response value types they share.
//!
//! ## Retry policy
//!
//! All providers use [`retry::with_backoff`] which retries up to 3 times with
//! exponential delay + jitter on [`LLMError::RateLimited`] and 5xx
//! [`LLMError::Provider`] errors. Other errors propagate immediately.
//!
//! ## Structured output
//!
//! When `ChatRequest::response_format` is [`ResponseFormat::JsonSchema`], each
//! provider attempts native structured-output mode (OpenAI `response_format`,
//! Gemini `responseSchema`, Claude tool-use) and additionally appends a short
//! reminder to the system prompt instructing the model to return only JSON
//! matching the schema. The resulting text is not validated against the
//! schema here — callers (typically the verifier crate) perform that check.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod providers;
pub mod retry;

#[cfg(feature = "claude")]
pub use providers::claude::ClaudeProvider;
#[cfg(feature = "gemini")]
pub use providers::gemini::GeminiProvider;
#[cfg(feature = "openai")]
pub use providers::openai::OpenAIProvider;
#[cfg(feature = "vllm")]
pub use providers::vllm::VllmProvider;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Common interface every LLM provider must implement.
#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Run a single chat completion.
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LLMError>;
    /// Short, stable name of the provider (e.g. `"claude"`).
    fn name(&self) -> &'static str;
    /// Whether the provider can accept image content parts.
    fn supports_vision(&self) -> bool {
        false
    }
    /// Maximum context window in tokens.
    fn context_window(&self) -> usize;
}

// ---------------------------------------------------------------------------
// Request/response value types
// ---------------------------------------------------------------------------

/// A single chat-completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Optional system prompt sent as the first turn.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub system: Option<String>,
    /// Chronologically-ordered conversation messages.
    pub messages: Vec<Message>,
    /// Provider-specific model identifier (e.g. `claude-opus-4-7`).
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
    /// Desired response format.
    pub response_format: ResponseFormat,
    /// Hint that the system prompt should be cached if the provider supports
    /// prompt-caching (Anthropic).
    #[serde(default)]
    pub cache_system: bool,
}

/// Speaker role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Human user / orchestrator-supplied turn.
    User,
    /// Model-generated turn.
    Assistant,
}

/// A multi-part message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Speaker role.
    pub role: Role,
    /// Ordered content parts; text-only providers should fall back to text
    /// extraction.
    pub content: Vec<ContentPart>,
}

/// One piece of a multi-modal message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum ContentPart {
    /// UTF-8 text segment.
    Text(String),
    /// URL pointing at an image asset.
    ImageUrl(String),
    /// In-memory image bytes plus MIME type (e.g. `image/png`).
    ImageBytes(ImageBytes),
}

/// In-memory image (raw bytes + MIME) carried inside a [`ContentPart`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBytes {
    /// Raw image bytes.
    pub bytes: Vec<u8>,
    /// MIME type, e.g. `image/png`.
    pub mime: String,
}

/// Desired response format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ResponseFormat {
    /// Free-form text.
    Text,
    /// Structured JSON conforming to the supplied JSON Schema (the value must
    /// be a valid JSON Schema document).
    JsonSchema(serde_json::Value),
}

/// Result of a chat completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Concatenated text output (provider-specific stop tokens stripped).
    pub text: String,
    /// Why the model stopped generating.
    pub finish_reason: FinishReason,
    /// Token-usage breakdown.
    pub usage: Usage,
    /// Raw provider payload for debugging / verifier inspection.
    pub raw: serde_json::Value,
}

/// Reason a generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Model hit a natural stop sequence.
    Stop,
    /// Hit `max_tokens` before stopping naturally.
    Length,
    /// Provider invoked a tool / function call.
    ToolUse,
    /// Filtered by safety / moderation.
    ContentFilter,
    /// Unknown / not reported.
    Other,
}

/// Token usage breakdown.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens consumed.
    pub tokens_in: u32,
    /// Completion tokens generated.
    pub tokens_out: u32,
    /// Tokens served from prompt cache, if reported by the provider.
    pub cache_hits: u32,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Provider error kinds.
#[derive(Error, Debug)]
pub enum LLMError {
    /// HTTP 429 (or provider-specific equivalent). `retry_after` may be set.
    #[error("rate limited (retry after {0:?})")]
    RateLimited(Option<std::time::Duration>),
    /// Provider account has no usable API spend available.
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
    /// Provider returned an HTTP-level failure or a logical error in the body.
    #[error("provider error: {0}")]
    Provider(String),
    /// The model's output failed schema validation.
    #[error("schema validation failed: {0}")]
    Schema(String),
    /// Transport-level reqwest error.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// Request exceeded the configured per-call timeout.
    #[error("timeout")]
    Timeout,
}

impl LLMError {
    /// Whether this error should trigger a retry under [`retry::with_backoff`].
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LLMError::RateLimited(_) | LLMError::Provider(_) | LLMError::Timeout
        )
    }
}

// ---------------------------------------------------------------------------
// Provider config + factory
// ---------------------------------------------------------------------------

/// Aggregate provider config; pulled from environment by the orchestrator and
/// passed in to keep this crate environment-agnostic.
#[derive(Debug, Clone, Default)]
pub struct ProviderConfig {
    /// Shared HTTP client (Arc'd so providers can clone cheaply).
    pub http: Option<Arc<reqwest::Client>>,
    /// `ANTHROPIC_API_KEY`.
    pub anthropic_api_key: Option<String>,
    /// `GOOGLE_GENERATIVE_AI_API_KEY`.
    pub google_api_key: Option<String>,
    /// `OPENAI_API_KEY`.
    pub openai_api_key: Option<String>,
    /// `VLLM_BASE_URL`, e.g. `http://localhost:8000/v1`.
    pub vllm_base_url: Option<String>,
    /// `VLLM_API_KEY` (vLLM may run without auth; this is optional).
    pub vllm_api_key: Option<String>,
}

impl ProviderConfig {
    /// Construct from process environment variables.
    pub fn from_env() -> Self {
        Self {
            http: None,
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            google_api_key: std::env::var("GOOGLE_GENERATIVE_AI_API_KEY").ok(),
            openai_api_key: std::env::var("OPENAI_API_KEY").ok(),
            vllm_base_url: std::env::var("VLLM_BASE_URL").ok(),
            vllm_api_key: std::env::var("VLLM_API_KEY").ok(),
        }
    }

    /// Borrow (or lazily build) the shared HTTP client.
    pub fn http(&self) -> Arc<reqwest::Client> {
        self.http.clone().unwrap_or_else(|| {
            Arc::new(
                reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .expect("build reqwest client"),
            )
        })
    }
}

/// Build a provider by short name (`"claude" | "gemini" | "openai" | "vllm"`).
pub fn provider_by_name(
    name: &str,
    cfg: &ProviderConfig,
) -> Result<Arc<dyn LLMProvider>, LLMError> {
    match name {
        #[cfg(feature = "claude")]
        "claude" => Ok(Arc::new(ClaudeProvider::from_config(cfg)?)),
        #[cfg(feature = "gemini")]
        "gemini" => Ok(Arc::new(GeminiProvider::from_config(cfg)?)),
        #[cfg(feature = "openai")]
        "openai" => Ok(Arc::new(OpenAIProvider::from_config(cfg)?)),
        #[cfg(feature = "vllm")]
        "vllm" => Ok(Arc::new(VllmProvider::from_config(cfg)?)),
        other => Err(LLMError::Provider(format!("unknown provider: {other}"))),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serializes_lowercase() {
        let r = serde_json::to_value(Role::User).unwrap();
        assert_eq!(r, serde_json::Value::String("user".into()));
    }

    #[test]
    fn finish_reason_serializes_snake_case() {
        let f = serde_json::to_value(FinishReason::ToolUse).unwrap();
        assert_eq!(f, serde_json::Value::String("tool_use".into()));
    }

    #[test]
    fn unknown_provider_errors() {
        let cfg = ProviderConfig::default();
        let err = match provider_by_name("nope", &cfg) {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn rate_limited_is_retryable() {
        assert!(LLMError::RateLimited(None).is_retryable());
        assert!(LLMError::Timeout.is_retryable());
        assert!(!LLMError::Schema("x".into()).is_retryable());
    }
}
