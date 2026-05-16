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
    /// Run one turn of a tool-call loop. The provider translates `req.tools`
    /// into its native tool-call format, sends the conversation, and returns
    /// any emitted tool calls plus the assistant message that produced them.
    ///
    /// Default impl errors so non-tool providers fail loudly rather than
    /// silently degrading.
    async fn complete_with_tools(
        &self,
        _req: ToolChatRequest,
    ) -> Result<ToolCompletion, LLMError> {
        Err(LLMError::Provider(format!(
            "provider `{}` does not implement complete_with_tools",
            self.name()
        )))
    }
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
// Tool-call types
// ---------------------------------------------------------------------------

/// A tool the LLM may call. The provider translates this into its native
/// representation (Anthropic `tools[]`, OpenAI `tools[].function`, Gemini
/// `tools[].functionDeclarations[]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name (stable identifier).
    pub name: String,
    /// One-line description shown to the LLM.
    pub description: String,
    /// JSON Schema for the tool's `input` / `parameters`.
    pub input_schema: serde_json::Value,
}

/// One message in a tool-using conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMessage {
    /// Speaker role.
    pub role: Role,
    /// Ordered content parts. Tool-using assistants may emit `ToolUse` blocks;
    /// users may emit `ToolResult` blocks. Text is also allowed.
    pub content: Vec<ToolContent>,
}

/// One content block in a tool-using conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    /// UTF-8 text segment.
    Text {
        /// Body of the text segment.
        text: String,
    },
    /// Assistant invoking a tool.
    ToolUse {
        /// Provider-issued call id (echoed back in `ToolResult.tool_use_id`).
        id: String,
        /// Tool name (matches a `ToolSpec.name`).
        name: String,
        /// JSON arguments to the tool.
        input: serde_json::Value,
    },
    /// User returning a tool's result to the assistant.
    ToolResult {
        /// Echoes the `ToolUse.id` that produced this result.
        tool_use_id: String,
        /// Body of the result (typically a JSON value).
        content: serde_json::Value,
        /// Whether the tool reported an error.
        #[serde(default)]
        is_error: bool,
    },
}

/// Request body for a tool-using turn.
#[derive(Debug, Clone)]
pub struct ToolChatRequest {
    /// Optional system prompt.
    pub system: Option<String>,
    /// Full conversation including any prior tool_use / tool_result blocks.
    pub messages: Vec<ToolMessage>,
    /// Tools the model may call this turn.
    pub tools: Vec<ToolSpec>,
    /// Model identifier.
    pub model: String,
    /// Max tokens for this turn.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
}

/// Result of one tool-using turn.
#[derive(Debug, Clone)]
pub struct ToolCompletion {
    /// Tool calls the assistant issued this turn (possibly empty).
    pub tool_calls: Vec<ProviderToolCall>,
    /// Any free-form text the assistant produced.
    pub text: String,
    /// Why the model stopped generating.
    pub finish_reason: FinishReason,
    /// Token-usage breakdown.
    pub usage: Usage,
    /// Raw provider payload for debugging.
    pub raw: serde_json::Value,
}

/// One tool call emitted by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderToolCall {
    /// Provider-issued call id. Tool results MUST quote this id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON arguments.
    pub arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Provider error kinds.
#[derive(Error, Debug)]
pub enum LLMError {
    /// HTTP 429 (or provider-specific equivalent). `retry_after` may be set
    /// when the provider returned a `Retry-After` header; `None` means the
    /// header was absent, which on practical providers (OpenAI Tier-2+,
    /// Anthropic, Gemini) usually indicates a TRANSIENT concurrent-request
    /// reject, NOT an actual quota exhaustion. Don't blame "rate limit"
    /// without checking the body for `error.code = "insufficient_quota"` —
    /// which the providers DO emit as `LLMError::QuotaExceeded` instead.
    #[error("HTTP 429 from provider{}", match .0 {
        Some(d) => format!(" (retry after {d:?})"),
        None => " (no Retry-After header; usually transient concurrent-request reject, not a quota)".to_string(),
    })]
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

    // ---------------------------------------------------------------
    // Regression guards (RPT2 G): the "rate limited (retry after None)"
    // error string was misleading — it doesn't say which provider, and
    // sub-second concurrent-request rejects look indistinguishable from
    // real per-minute quota exhaustion. These tests pin the new message
    // so a future refactor can't silently regress.
    // ---------------------------------------------------------------

    #[test]
    fn rate_limited_without_retry_after_explains_likely_cause() {
        // No Retry-After header → should explicitly call out that this is
        // probably a transient concurrent-request reject, not a quota.
        let msg = LLMError::RateLimited(None).to_string();
        assert!(
            msg.contains("HTTP 429"),
            "should label the underlying signal as HTTP 429, got: {msg}"
        );
        assert!(
            msg.contains("no Retry-After")
                || msg.contains("transient")
                || msg.contains("concurrent"),
            "no-Retry-After case must hint at concurrent-request burst, not a quota; got: {msg}"
        );
        // The old wording was the literal `"retry after None"` debug print.
        // That was the source of the bias-toward-OpenAI bug. Never bring
        // that back.
        assert!(
            !msg.contains("retry after None"),
            "must not surface raw Debug of Option<Duration>; got: {msg}"
        );
    }

    #[test]
    fn rate_limited_with_retry_after_shows_duration() {
        let msg =
            LLMError::RateLimited(Some(std::time::Duration::from_secs(30))).to_string();
        assert!(
            msg.contains("HTTP 429"),
            "must still mention HTTP 429 even with Retry-After, got: {msg}"
        );
        assert!(
            msg.contains("retry after") && msg.contains("30"),
            "should expose the retry-after seconds, got: {msg}"
        );
    }

    #[test]
    fn quota_exceeded_is_distinct_from_rate_limited() {
        // We MUST distinguish "billing/quota empty" from "burst rejected".
        // The OpenAI provider already does this — keep the variants distinct
        // and ensure their error strings tell the operator something useful.
        let q = LLMError::QuotaExceeded("body=…".to_string()).to_string();
        assert!(q.contains("quota"), "QuotaExceeded must say 'quota': {q}");
        assert!(!q.contains("rate limited") && !q.contains("HTTP 429"),
            "QuotaExceeded must NOT be confused with rate-limit: {q}");
        // QuotaExceeded is NOT retryable. RateLimited IS.
        assert!(LLMError::RateLimited(None).is_retryable());
        assert!(!LLMError::QuotaExceeded("x".into()).is_retryable());
    }
}
