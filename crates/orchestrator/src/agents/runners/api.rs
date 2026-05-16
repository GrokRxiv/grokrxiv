//! `ApiRunner` — direct LLM provider API calls.
//!
//! The default backend for every role at RPT2 ship time. Wraps the existing
//! `LLMProvider` trait from `crates/llm-adapter`. Track A lifts the body of
//! the legacy `supervisor.rs::call_with_schema` helper into `ApiRunner::run`
//! so the supervisor can delegate via `ReviewAgent::run`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use grokrxiv_llm_adapter::{
    ChatRequest, ContentPart, LLMProvider, Message, ResponseFormat, Role,
};

use crate::agents::traits::AgentRunner;
use crate::agents::types::{AgentInput, AgentRun, AgentRunnerKind, AgentSpec};

/// Holds a registry of `LLMProvider` impls keyed by name (`"claude"`,
/// `"openai"`, `"gemini"`, `"deepseek"`, etc.). The runner dispatches to the
/// right one based on `spec.provider`.
pub struct ApiRunner {
    providers: HashMap<String, Arc<dyn LLMProvider>>,
}

impl ApiRunner {
    /// Construct from an existing provider registry.
    pub fn new(providers: HashMap<String, Arc<dyn LLMProvider>>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl AgentRunner for ApiRunner {
    fn name(&self) -> &'static str {
        "api"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
        let provider = self.providers.get(&spec.provider).ok_or_else(|| {
            anyhow::anyhow!(
                "ApiRunner: no provider registered for `{}` (role={:?})",
                spec.provider,
                spec.role
            )
        })?;

        let schema = spec.schema.clone();
        let model = spec.model.clone();
        let system = input.system_prompt.clone();
        let prompt = input.user_prompt.clone();

        let make_req = |user_prompt: String, schema: serde_json::Value| ChatRequest {
            system: Some(system.clone()),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentPart::Text(user_prompt)],
            }],
            model: model.clone(),
            max_tokens: 6_000,
            temperature: 0.2,
            response_format: ResponseFormat::JsonSchema(schema),
            cache_system: true,
        };

        let started = Instant::now();
        let resp = provider
            .complete(make_req(prompt.clone(), schema.clone()))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let (output, usage) = match parse_strict_json(&resp.text) {
            Ok(v) => (v, resp.usage),
            Err(first_err) => {
                // Single corrective retry: tell the model its prior output failed
                // schema validation and ask for strict JSON. No `{"raw": ...}`
                // fallback — if the retry also fails the caller surfaces a hard
                // error and the verifier records the parse error in
                // `verifier_notes.parse_error`.
                let corrective = format!(
                    "{prompt}\n\nYour previous output did not validate against the schema; \
                     return strict JSON only, with no surrounding prose, code fences, or commentary."
                );
                let retry = provider
                    .complete(make_req(corrective, schema.clone()))
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                match parse_strict_json(&retry.text) {
                    Ok(v) => (v, retry.usage),
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "parse failure after corrective retry: first={first_err}; retry={e}; \
                             raw_first={raw_first:?}; raw_retry={raw_retry:?}",
                            raw_first = resp.text,
                            raw_retry = retry.text,
                        ));
                    }
                }
            }
        };
        let latency_ms = started.elapsed().as_millis() as i32;

        Ok(AgentRun {
            role: input.role,
            runner: AgentRunnerKind::Api,
            model: spec.model.clone(),
            output,
            verifier_status: None,
            verifier_notes: None,
            tokens_in: Some(usage.tokens_in as i32),
            tokens_out: Some(usage.tokens_out as i32),
            latency_ms,
            cache_hit: false,
            sandbox_ref: None,
        })
    }
}

/// Try strict JSON parse; on failure, strip ```json fences and retry; on
/// failure again, return `Err`. Never returns `{"raw": ...}`.
fn parse_strict_json(s: &str) -> anyhow::Result<serde_json::Value> {
    let trimmed = s.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => Ok(v),
        Err(_) => {
            let stripped = strip_fences(trimmed);
            serde_json::from_str::<serde_json::Value>(stripped)
                .map_err(|e| anyhow::anyhow!("not valid JSON: {e}"))
        }
    }
}

fn strip_fences(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("```json") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    if let Some(rest) = s.strip_prefix("```") {
        return rest.trim_start_matches('\n').trim_end_matches("```").trim();
    }
    s
}
