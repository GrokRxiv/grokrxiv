//! End-to-end provider tests against a local `wiremock` server.
//!
//! These tests do NOT call out to real provider APIs; they assert that our
//! request shaping + response parsing wire up correctly against a recorded
//! happy-path body and that 429 responses trigger the retry/backoff path.

use std::sync::Arc;

use agenthero_llm_adapter::providers::claude::ClaudeProvider;
use agenthero_llm_adapter::providers::gemini::GeminiProvider;
use agenthero_llm_adapter::providers::openai::OpenAIProvider;
use agenthero_llm_adapter::{
    ChatRequest, ContentPart, LLMError, LLMProvider, Message, ProviderConfig, ResponseFormat, Role,
    ToolChatRequest, ToolContent, ToolMessage, ToolSpec,
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

fn gemini_cfg_with_http() -> ProviderConfig {
    let mut cfg = ProviderConfig {
        google_api_key: Some("test-key".into()),
        ..ProviderConfig::default()
    };
    cfg.http = Some(Arc::new(reqwest::Client::new()));
    cfg
}

/// FP-RPT3a A2: Gemini's `functionResponse.name` semantics require the
/// function NAME to be echoed back on the user-turn tool result, NOT a
/// synthetic `call_N` id. This test drives a 2-turn conversation through
/// the parser to assert turn 1 emits a ProviderToolCall whose `id` equals
/// its `name`, then constructs a continuation message and verifies the
/// resulting Gemini body posts `functionResponse.name = "list_files"` on
/// turn 2 (not `"call_1"`).
#[tokio::test]
async fn gemini_tool_call_id_uses_function_name() {
    // Turn 1: model calls list_files. Parse the response and confirm the
    // ProviderToolCall.id equals the function name.
    let turn1_raw = json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "functionCall": {
                        "name": "list_files",
                        "args": { "glob": "**/*.tex" }
                    }
                }]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": { "promptTokenCount": 12, "candidatesTokenCount": 8 }
    });
    let parsed = GeminiProvider::parse_tools_response(turn1_raw).expect("parse turn 1");
    assert_eq!(parsed.tool_calls.len(), 1);
    let call = &parsed.tool_calls[0];
    assert_eq!(
        call.id, "list_files",
        "Gemini ProviderToolCall.id MUST be the function name, not a synthetic id; got {:?}",
        call.id
    );
    assert_eq!(call.name, "list_files");

    // Turn 2: extraction loop posts back a tool result whose
    // `tool_use_id` is the same value (= function name). Build the request
    // body and assert `functionResponse.name == "list_files"`.
    let req = ToolChatRequest {
        system: Some("you are an extractor".into()),
        messages: vec![
            ToolMessage {
                role: Role::User,
                content: vec![ToolContent::Text {
                    text: "kick off".into(),
                }],
            },
            ToolMessage {
                role: Role::Assistant,
                content: vec![ToolContent::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.arguments.clone(),
                }],
            },
            ToolMessage {
                role: Role::User,
                content: vec![ToolContent::ToolResult {
                    tool_use_id: call.id.clone(),
                    content: json!({ "files": ["main.tex"] }),
                    is_error: false,
                }],
            },
        ],
        tools: vec![ToolSpec {
            name: "list_files".into(),
            description: "list files".into(),
            input_schema: json!({"type": "object"}),
        }],
        model: "gemini-2.5-flash".into(),
        max_tokens: 256,
        temperature: 0.0,
    };
    let body = GeminiProvider::build_tools_body(&req);
    // The tool_result message becomes a user-turn `functionResponse` part.
    // Walk to it and assert the name is the function name, NOT call_1.
    let last = body["contents"]
        .as_array()
        .and_then(|a| a.last())
        .expect("contents has at least one entry");
    let parts = last["parts"].as_array().expect("parts array");
    let fr_name = parts
        .iter()
        .find_map(|p| p.get("functionResponse").and_then(|fr| fr.get("name")))
        .expect("functionResponse.name present on user turn 2");
    assert_eq!(
        fr_name.as_str(),
        Some("list_files"),
        "Gemini turn-2 functionResponse.name MUST equal the function name; got {fr_name:?}"
    );
}

/// Full 2-turn wiremock round trip: turn 1 emits list_files, turn 2 emits
/// submit. Asserts no malformed `functionResponse.name` synthesised id
/// surfaces.
#[tokio::test]
async fn gemini_two_turn_tool_loop_via_wiremock() {
    let server = MockServer::start().await;

    // Turn 1: model says "call list_files".
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": { "name": "list_files", "args": {} }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 2 }
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    // Turn 2: model says "call submit".
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": { "name": "submit", "args": { "ok": true } }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 4 }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let provider = GeminiProvider::from_config(&gemini_cfg_with_http())
        .unwrap()
        .with_base_url(server.uri());

    let tools = vec![
        ToolSpec {
            name: "list_files".into(),
            description: "list".into(),
            input_schema: json!({"type": "object"}),
        },
        ToolSpec {
            name: "submit".into(),
            description: "finalise".into(),
            input_schema: json!({"type": "object"}),
        },
    ];

    // Turn 1.
    let system_prompt: Option<String> = Some("extract.".into());
    let req1 = ToolChatRequest {
        system: system_prompt.clone(),
        messages: vec![ToolMessage {
            role: Role::User,
            content: vec![ToolContent::Text { text: "go".into() }],
        }],
        tools: tools.clone(),
        model: "gemini-2.5-flash".into(),
        max_tokens: 128,
        temperature: 0.0,
    };
    let r1 = provider.complete_with_tools(req1).await.expect("turn 1");
    assert_eq!(r1.tool_calls.len(), 1);
    let call1 = &r1.tool_calls[0];
    assert_eq!(
        call1.id, "list_files",
        "turn-1 id must be the function name"
    );

    // Turn 2: post the tool_result back using the function-name id.
    let req2 = ToolChatRequest {
        system: system_prompt,
        messages: vec![
            ToolMessage {
                role: Role::User,
                content: vec![ToolContent::Text { text: "go".into() }],
            },
            ToolMessage {
                role: Role::Assistant,
                content: vec![ToolContent::ToolUse {
                    id: call1.id.clone(),
                    name: call1.name.clone(),
                    input: call1.arguments.clone(),
                }],
            },
            ToolMessage {
                role: Role::User,
                content: vec![ToolContent::ToolResult {
                    tool_use_id: call1.id.clone(),
                    content: json!({ "files": ["main.tex"] }),
                    is_error: false,
                }],
            },
        ],
        tools,
        model: "gemini-2.5-flash".into(),
        max_tokens: 128,
        temperature: 0.0,
    };
    // Sanity: the body that goes out on turn 2 must echo the FUNCTION NAME
    // in `functionResponse.name`. Pre-flight check via build_tools_body.
    let body2 = GeminiProvider::build_tools_body(&req2);
    let last = body2["contents"].as_array().unwrap().last().unwrap();
    let fr_name = last["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|p| p.get("functionResponse").and_then(|fr| fr.get("name")))
        .expect("functionResponse.name present");
    assert_eq!(fr_name.as_str(), Some("list_files"));

    let r2 = provider.complete_with_tools(req2).await.expect("turn 2");
    assert_eq!(r2.tool_calls.len(), 1);
    assert_eq!(r2.tool_calls[0].name, "submit");
    assert_eq!(r2.tool_calls[0].id, "submit");
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
