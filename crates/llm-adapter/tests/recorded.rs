//! End-to-end provider tests against a local `wiremock` server.
//!
//! These tests do NOT call out to real provider APIs; they assert that our
//! request shaping + response parsing wire up correctly against a recorded
//! happy-path body and that 429 responses trigger the retry/backoff path.

use std::sync::Arc;

use grokrxiv_llm_adapter::providers::claude::ClaudeProvider;
use grokrxiv_llm_adapter::providers::openai::OpenAIProvider;
use grokrxiv_llm_adapter::{
    ChatRequest, ContentPart, LLMError, LLMProvider, Message, ProviderConfig, ResponseFormat, Role,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req() -> ChatRequest {
    ChatRequest {
        system: Some("Be terse.".into()),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentPart::Text("Say hi.".into())],
        }],
        model: "claude-opus-4-7".into(),
        max_tokens: 64,
        temperature: 0.0,
        response_format: ResponseFormat::Text,
        cache_system: false,
    }
}

fn cfg_with_http() -> ProviderConfig {
    let mut cfg = ProviderConfig {
        anthropic_api_key: Some("test-key".into()),
        ..ProviderConfig::default()
    };
    cfg.http = Some(Arc::new(reqwest::Client::new()));
    cfg
}

fn openai_cfg_with_http() -> ProviderConfig {
    let mut cfg = ProviderConfig {
        openai_api_key: Some("test-key".into()),
        ..ProviderConfig::default()
    };
    cfg.http = Some(Arc::new(reqwest::Client::new()));
    cfg
}

#[tokio::test]
async fn claude_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "hi" }],
            "model": "claude-opus-4-7",
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 10, "output_tokens": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = ClaudeProvider::from_config(&cfg_with_http())
        .unwrap()
        .with_base_url(format!("{}/v1/messages", server.uri()));

    let resp = provider.complete(req()).await.expect("claude call");
    assert_eq!(resp.text, "hi");
    assert_eq!(resp.usage.tokens_in, 10);
    assert_eq!(resp.usage.tokens_out, 1);
}

#[tokio::test]
async fn claude_retries_on_429_then_succeeds() {
    let server = MockServer::start().await;

    // First call: 429 with retry-after 0s
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    // Subsequent: success
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = ClaudeProvider::from_config(&cfg_with_http())
        .unwrap()
        .with_base_url(format!("{}/v1/messages", server.uri()));

    let resp = provider.complete(req()).await.expect("retried success");
    assert_eq!(resp.text, "ok");
}

#[tokio::test]
async fn claude_gives_up_after_persistent_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .mount(&server)
        .await;

    let provider = ClaudeProvider::from_config(&cfg_with_http())
        .unwrap()
        .with_base_url(format!("{}/v1/messages", server.uri()));

    let err = provider.complete(req()).await.expect_err("must fail");
    assert!(matches!(err, LLMError::RateLimited(_)));
}

#[tokio::test]
async fn openai_insufficient_quota_is_not_retried_as_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "message": "You exceeded your current quota, please check your plan and billing details",
                "type": "insufficient_quota",
                "code": "insufficient_quota"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_config(&openai_cfg_with_http())
        .unwrap()
        .with_base_url(server.uri());
    let mut request = req();
    request.model = "gpt-5.5".into();

    let err = provider.complete(request).await.expect_err("must fail");
    assert!(matches!(err, LLMError::QuotaExceeded(_)));
}
