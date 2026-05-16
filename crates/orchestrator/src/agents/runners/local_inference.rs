//! `LocalInferenceRunner` — local OSS models via Ollama (or LiteLLM gateway).
//!
//! Generic OpenAI-compatible HTTP client. Prefers `GROKRXIV_LITELLM_URL`
//! when set; falls back to `OLLAMA_HOST` directly. vLLM/MLX/llama.cpp stay
//! deployment-time choices behind LiteLLM — no runtime switch.
//!
//! Endpoint resolution:
//! - If `$GROKRXIV_LITELLM_URL` is set: `{url}/v1/chat/completions`.
//! - Else: `${OLLAMA_HOST:-http://localhost:11434}/v1/chat/completions`
//!   (Ollama's OpenAI-compat path, available since v0.5).
//!
//! Request shape is OpenAI Chat Completions with `response_format =
//! {type: "json_object"}`. One corrective retry on parse failure.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;

use crate::agents::traits::AgentRunner;
use crate::agents::types::{AgentInput, AgentRun, AgentRunnerKind, AgentSpec};

/// Default Ollama host when neither `GROKRXIV_LITELLM_URL` nor `OLLAMA_HOST`
/// are set.
const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

/// Default per-call timeout in seconds when `GROKRXIV_LOCAL_TIMEOUT_SECS` is
/// not set or not parseable.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// HTTP client targeting `{LITELLM_URL or OLLAMA_HOST}/v1/chat/completions`.
#[derive(Default)]
pub struct LocalInferenceRunner;

impl LocalInferenceRunner {
    /// Construct with defaults.
    pub fn new() -> Self {
        Self
    }
}

/// Resolve the chat-completions endpoint from environment variables.
///
/// Order of precedence:
/// 1. `GROKRXIV_LITELLM_URL` (the gateway path — LiteLLM routes to whatever
///    provider it's configured with).
/// 2. `OLLAMA_HOST` (direct to Ollama's OpenAI-compat endpoint).
/// 3. `DEFAULT_OLLAMA_HOST` (the documented localhost default).
fn resolve_endpoint() -> String {
    let base = std::env::var("GROKRXIV_LITELLM_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("OLLAMA_HOST").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string());
    let trimmed = base.trim_end_matches('/');
    format!("{trimmed}/v1/chat/completions")
}

/// Resolve per-call timeout from `GROKRXIV_LOCAL_TIMEOUT_SECS` or fall back to
/// [`DEFAULT_TIMEOUT_SECS`].
fn resolve_timeout() -> Duration {
    let secs = std::env::var("GROKRXIV_LOCAL_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Build the OpenAI-compatible chat completions request body.
fn build_body(model: &str, system_prompt: &str, user_prompt: &str) -> serde_json::Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user",   "content": user_prompt},
        ],
        "response_format": {"type": "json_object"},
        "temperature": 0.0,
        "max_tokens": 4096,
    })
}

/// POST a request body to the endpoint and return the parsed response value.
async fn post_chat_completions(
    client: &reqwest::Client,
    endpoint: &str,
    body: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let resp = client
        .post(endpoint)
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("local inference HTTP send failed: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("local inference body read failed: {e}"))?;
    if !status.is_success() {
        anyhow::bail!("local inference HTTP {status}: {text}");
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| anyhow::anyhow!("local inference response is not JSON: {e}; body={text}"))
}

/// Pull `choices[0].message.content` as a string from an OpenAI-compatible
/// response.
fn extract_content(resp: &serde_json::Value) -> anyhow::Result<String> {
    resp.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow::anyhow!("response missing choices[0].message.content: {resp}")
        })
}

/// Extract `(tokens_in, tokens_out)` from `usage` when present.
fn extract_usage(resp: &serde_json::Value) -> (Option<i32>, Option<i32>) {
    let usage = resp.get("usage");
    let prompt = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_i64())
        .and_then(|n| i32::try_from(n).ok());
    let completion = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_i64())
        .and_then(|n| i32::try_from(n).ok());
    (prompt, completion)
}

/// Try strict JSON parse of the content payload.
fn parse_content_json(content: &str) -> anyhow::Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(content.trim())
        .map_err(|e| anyhow::anyhow!("content is not valid JSON: {e}"))
}

#[async_trait]
impl AgentRunner for LocalInferenceRunner {
    fn name(&self) -> &'static str {
        "local_inference"
    }

    async fn run(
        &self,
        spec: &AgentSpec,
        input: &AgentInput,
    ) -> anyhow::Result<AgentRun> {
        let endpoint = resolve_endpoint();
        let timeout = resolve_timeout();
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build reqwest client: {e}"))?;

        let started = Instant::now();

        // First attempt.
        let body = build_body(&spec.model, &input.system_prompt, &input.user_prompt);
        let resp = post_chat_completions(&client, &endpoint, &body).await?;
        let content = extract_content(&resp)?;

        let (parsed, final_resp) = match parse_content_json(&content) {
            Ok(v) => (v, resp),
            Err(first_err) => {
                // One-shot corrective retry. Amend the user prompt to remind
                // the model to return strict JSON only. Mirrors the
                // supervisor.rs `call_with_schema` pattern.
                let corrective_user = format!(
                    "{user}\n\nYour previous output did not parse as JSON \
                     (or did not validate against the required schema). \
                     Return strict JSON only, with no surrounding prose, \
                     code fences, or commentary.",
                    user = input.user_prompt
                );
                let retry_body = build_body(&spec.model, &input.system_prompt, &corrective_user);
                let retry_resp = post_chat_completions(&client, &endpoint, &retry_body).await?;
                let retry_content = extract_content(&retry_resp)?;
                match parse_content_json(&retry_content) {
                    Ok(v) => (v, retry_resp),
                    Err(retry_err) => {
                        anyhow::bail!(
                            "local inference parse failure after corrective retry: \
                             first={first_err}; retry={retry_err}; \
                             raw_first={content:?}; raw_retry={retry_content:?}"
                        );
                    }
                }
            }
        };

        let (tokens_in, tokens_out) = extract_usage(&final_resp);
        let latency_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);

        Ok(AgentRun {
            role: spec.role,
            runner: AgentRunnerKind::LocalInference,
            model: spec.model.clone(),
            output: parsed,
            tokens_in,
            tokens_out,
            latency_ms,
            cache_hit: false,
            sandbox_ref: None,
            verifier_status: None,
            verifier_notes: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::types::AgentSpec;
    use grokrxiv_schemas::AgentRole;
    use std::sync::Mutex;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Env vars are process-global. Serialise tests that mutate them so they
    /// don't race when `cargo test` runs with multiple threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that restores the previous value of an env var on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prev }
        }

        fn unset(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn test_spec() -> AgentSpec {
        AgentSpec::api_default(
            AgentRole::Summary,
            "ollama".to_string(),
            "qwen2.5:7b-instruct".to_string(),
        )
    }

    fn test_input() -> AgentInput {
        AgentInput {
            paper_id: Uuid::nil(),
            review_id: Uuid::nil(),
            role: AgentRole::Summary,
            content_hash_material: serde_json::json!({}),
            artifact: serde_json::json!({}),
            system_prompt: "you are a reviewer".to_string(),
            user_prompt: "review this paper".to_string(),
            source_bundle_path: None,
        }
    }

    fn ok_response(content: &str, with_usage: bool) -> serde_json::Value {
        let mut v = serde_json::json!({
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
            }]
        });
        if with_usage {
            v["usage"] = serde_json::json!({
                "prompt_tokens": 123,
                "completion_tokens": 45,
                "total_tokens": 168,
            });
        }
        v
    }

    #[tokio::test]
    async fn test_litellm_url_preferred_over_ollama() {
        let _lock = ENV_LOCK.lock().unwrap();
        let litellm = MockServer::start().await;
        let ollama = MockServer::start().await;

        // Litellm responds with valid JSON content.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"hit\":\"litellm\"}", true)),
            )
            .mount(&litellm)
            .await;
        // Ollama would respond with a different marker — but it should never be hit.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"hit\":\"ollama\"}", true)),
            )
            .mount(&ollama)
            .await;

        let _g1 = EnvGuard::set("GROKRXIV_LITELLM_URL", &litellm.uri());
        let _g2 = EnvGuard::set("OLLAMA_HOST", &ollama.uri());

        let runner = LocalInferenceRunner::new();
        let run = runner.run(&test_spec(), &test_input()).await.unwrap();
        assert_eq!(run.output, serde_json::json!({"hit": "litellm"}));
        // litellm got a request; ollama did not.
        let lit_reqs = litellm.received_requests().await.unwrap();
        assert_eq!(lit_reqs.len(), 1);
        let oll_reqs = ollama.received_requests().await.unwrap();
        assert!(oll_reqs.is_empty(), "ollama should not be hit when litellm set");
    }

    #[tokio::test]
    async fn test_falls_back_to_ollama_when_litellm_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        let ollama = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"hit\":\"ollama\"}", true)),
            )
            .mount(&ollama)
            .await;

        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::set("OLLAMA_HOST", &ollama.uri());

        let runner = LocalInferenceRunner::new();
        let run = runner.run(&test_spec(), &test_input()).await.unwrap();
        assert_eq!(run.output, serde_json::json!({"hit": "ollama"}));
        assert_eq!(ollama.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_default_ollama_host() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::unset("OLLAMA_HOST");
        let url = resolve_endpoint();
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
    }

    #[tokio::test]
    async fn test_response_format_json_object_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"ok\":true}", true)),
            )
            .mount(&server)
            .await;

        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::set("OLLAMA_HOST", &server.uri());

        let runner = LocalInferenceRunner::new();
        runner.run(&test_spec(), &test_input()).await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(
            body.get("response_format").unwrap(),
            &serde_json::json!({"type": "json_object"})
        );
        // Sanity-check the rest of the payload while we're here.
        assert_eq!(body.get("model").and_then(|v| v.as_str()), Some("qwen2.5:7b-instruct"));
        assert_eq!(
            body.get("messages")
                .and_then(|m| m.get(0))
                .and_then(|m| m.get("role"))
                .and_then(|v| v.as_str()),
            Some("system")
        );
    }

    #[tokio::test]
    async fn test_token_accounting_from_usage() {
        let _lock = ENV_LOCK.lock().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"ok\":true}", true)),
            )
            .mount(&server)
            .await;

        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::set("OLLAMA_HOST", &server.uri());

        let runner = LocalInferenceRunner::new();
        let run = runner.run(&test_spec(), &test_input()).await.unwrap();
        assert_eq!(run.tokens_in, Some(123));
        assert_eq!(run.tokens_out, Some(45));
    }

    #[tokio::test]
    async fn test_token_accounting_missing_usage() {
        let _lock = ENV_LOCK.lock().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_response("{\"ok\":true}", false)),
            )
            .mount(&server)
            .await;

        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::set("OLLAMA_HOST", &server.uri());

        let runner = LocalInferenceRunner::new();
        let run = runner.run(&test_spec(), &test_input()).await.unwrap();
        assert_eq!(run.tokens_in, None);
        assert_eq!(run.tokens_out, None);
    }

    #[tokio::test]
    async fn test_corrective_retry_on_parse_fail() {
        let _lock = ENV_LOCK.lock().unwrap();
        let server = MockServer::start().await;

        // First call: non-JSON content (will fail to parse).
        // Second call: valid JSON. wiremock matches in mount order, and
        // `up_to_n_times(1)` lets the first mock retire after one hit so the
        // second mock catches the retry.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(ok_response("not actually json at all", true)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(ok_response("{\"retry\":\"worked\"}", true)),
            )
            .mount(&server)
            .await;

        let _g1 = EnvGuard::unset("GROKRXIV_LITELLM_URL");
        let _g2 = EnvGuard::set("OLLAMA_HOST", &server.uri());

        let runner = LocalInferenceRunner::new();
        let run = runner.run(&test_spec(), &test_input()).await.unwrap();
        assert_eq!(run.output, serde_json::json!({"retry": "worked"}));
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 2, "expected initial call + one corrective retry");
        // The corrective retry should mention strict JSON in the user message.
        let second: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
        let user_content = second
            .get("messages")
            .and_then(|m| m.get(1))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            user_content.contains("strict JSON"),
            "retry user prompt should include strict-JSON reminder, got: {user_content}"
        );
    }
}
