//! `CloudRunner` — durable cloud workflow.
//!
//! Vercel Open Agents primary; E2B alternate. Selected by
//! `GROKRXIV_CLOUD_PROVIDER`. Cloud runners are inherently sandboxed; they
//! ignore `SandboxPolicy`. Track E.
//!
//! ### Flow (Vercel)
//! 1. POST `${VERCEL_OPEN_AGENTS_URL}/api/run` with the run spec.
//! 2. Poll `GET ${VERCEL_OPEN_AGENTS_URL}/api/run/{run_id}` every 2s until
//!    `status` is `completed` or `failed`, capped at `spec.timeout_secs`.
//! 3. On `completed`, parse `output` as JSON. On parse failure, do exactly
//!    one corrective retry by posting a new run with an amended user prompt.
//!
//! ### Flow (E2B)
//! Not yet wired; the runner returns a clean error pointing users at the
//! Vercel path. See the TODO inside `dispatch_e2b`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::traits::AgentRunner;
use crate::agents::types::{AgentInput, AgentRun, AgentRunnerKind, AgentSpec};

/// Polling interval used while waiting for a Vercel run to finish.
const POLL_INTERVAL: Duration = Duration::from_millis(2_000);

/// Resolved configuration for the Vercel Open Agents path.
#[derive(Debug, Clone)]
struct VercelConfig {
    /// Base URL (e.g. `https://agents.vercel.app`).
    base_url: String,
    /// Bearer token, if set. Omitted from the `Authorization` header when
    /// `None` so local mock servers don't need to handle auth.
    token: Option<String>,
}

/// Posts to the configured cloud agent endpoint and polls for completion.
#[derive(Default, Clone)]
pub struct CloudRunner {
    /// Override for the polling cadence. Defaults to [`POLL_INTERVAL`].
    /// Wired in for tests so we don't sleep for 2s between polls.
    poll_interval: Option<Duration>,
    /// Optional Vercel override; when set, the runner ignores
    /// `$VERCEL_OPEN_AGENTS_URL` / `$VERCEL_OPEN_AGENTS_TOKEN`. Used by tests.
    vercel_override: Option<VercelConfig>,
    /// Optional provider override; when set the runner ignores
    /// `$GROKRXIV_CLOUD_PROVIDER`. Used by tests.
    provider_override: Option<String>,
}

impl CloudRunner {
    /// Construct with defaults — reads provider + URL + token from env.
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the polling interval (tests only).
    #[cfg(test)]
    fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = Some(interval);
        self
    }

    /// Override the Vercel endpoint (tests only).
    #[cfg(test)]
    fn with_vercel(mut self, base_url: String, token: Option<String>) -> Self {
        self.vercel_override = Some(VercelConfig { base_url, token });
        self
    }

    /// Force the selected provider regardless of env (tests only).
    #[cfg(test)]
    fn with_provider(mut self, provider: String) -> Self {
        self.provider_override = Some(provider);
        self
    }

    fn provider(&self) -> String {
        if let Some(p) = &self.provider_override {
            return p.clone();
        }
        std::env::var("GROKRXIV_CLOUD_PROVIDER").unwrap_or_else(|_| "vercel".into())
    }

    fn poll_interval(&self) -> Duration {
        self.poll_interval.unwrap_or(POLL_INTERVAL)
    }

    fn resolve_vercel_config(&self) -> anyhow::Result<VercelConfig> {
        if let Some(cfg) = &self.vercel_override {
            return Ok(cfg.clone());
        }
        let base_url = std::env::var("VERCEL_OPEN_AGENTS_URL").map_err(|_| {
            anyhow::anyhow!(
                "VERCEL_OPEN_AGENTS_URL is not set; required when \
                 GROKRXIV_CLOUD_PROVIDER=vercel"
            )
        })?;
        let token = std::env::var("VERCEL_OPEN_AGENTS_TOKEN").ok();
        Ok(VercelConfig { base_url, token })
    }
}

#[async_trait]
impl AgentRunner for CloudRunner {
    fn name(&self) -> &'static str {
        "cloud"
    }

    async fn run(&self, spec: &AgentSpec, input: &AgentInput) -> anyhow::Result<AgentRun> {
        let provider = self.provider();
        match provider.as_str() {
            "vercel" => self.dispatch_vercel(spec, input).await,
            "e2b" => self.dispatch_e2b(spec, input).await,
            other => anyhow::bail!("unsupported cloud provider: {other}"),
        }
    }
}

impl CloudRunner {
    /// Build the POST body sent to `${base}/api/run`.
    fn build_run_payload(spec: &AgentSpec, input: &AgentInput, user_prompt: &str) -> Value {
        json!({
            "agent": "grokrxiv-review",
            "role": spec.role,
            "model": spec.model,
            "provider": spec.provider,
            "system_prompt": input.system_prompt,
            "user_prompt": user_prompt,
            "schema": spec.schema,
        })
    }

    /// Vercel Open Agents path.
    async fn dispatch_vercel(
        &self,
        spec: &AgentSpec,
        input: &AgentInput,
    ) -> anyhow::Result<AgentRun> {
        let cfg = self.resolve_vercel_config()?;
        let http = reqwest::Client::new();

        let started = Instant::now();
        let timeout = Duration::from_secs(spec.timeout_secs.max(1) as u64);

        // First attempt: original user prompt.
        let (first_output, first_sandbox) = self
            .start_and_wait(
                &http,
                &cfg,
                spec,
                input,
                &input.user_prompt,
                started,
                timeout,
            )
            .await?;

        let (parsed, sandbox_ref) = match parse_strict_json(&first_output) {
            Ok(v) => (v, first_sandbox),
            Err(parse_err) => {
                // One-shot corrective retry: rerun with an amended prompt that
                // asks the model for strict JSON.
                tracing::warn!(
                    err = %parse_err,
                    "cloud(vercel): first run produced non-JSON output; retrying with corrective prompt",
                );
                let corrective = format!(
                    "{prompt}\n\nYour previous output did not validate against the schema; \
                     return strict JSON only, with no surrounding prose, code fences, or commentary.",
                    prompt = input.user_prompt,
                );
                let (retry_output, retry_sandbox) = self
                    .start_and_wait(&http, &cfg, spec, input, &corrective, started, timeout)
                    .await?;
                let parsed = parse_strict_json(&retry_output).map_err(|e| {
                    anyhow::anyhow!(
                        "cloud(vercel) parse failure after corrective retry: first={parse_err}; \
                         retry={e}"
                    )
                })?;
                (parsed, retry_sandbox)
            }
        };

        let latency_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;
        Ok(AgentRun {
            role: spec.role,
            runner: AgentRunnerKind::Cloud,
            model: spec.model.clone(),
            output: parsed,
            tokens_in: None,
            tokens_out: None,
            latency_ms,
            cache_hit: false,
            sandbox_ref,
            verifier_status: None,
            verifier_notes: None,
        })
    }

    /// POST a single Vercel run, then poll until it completes or fails.
    /// Returns the raw `output` (serialised back to a string) and the
    /// `sandbox_ref` reported by the service.
    async fn start_and_wait(
        &self,
        http: &reqwest::Client,
        cfg: &VercelConfig,
        spec: &AgentSpec,
        input: &AgentInput,
        user_prompt: &str,
        started: Instant,
        timeout: Duration,
    ) -> anyhow::Result<(String, Option<String>)> {
        let payload = Self::build_run_payload(spec, input, user_prompt);

        let mut start_req = http
            .post(format!("{}/api/run", cfg.base_url.trim_end_matches('/')))
            .json(&payload);
        if let Some(tok) = &cfg.token {
            start_req = start_req.bearer_auth(tok);
        }
        let start_resp = start_req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("cloud(vercel) POST /api/run failed: {e}"))?;
        if !start_resp.status().is_success() {
            anyhow::bail!(
                "cloud(vercel) POST /api/run returned HTTP {}",
                start_resp.status()
            );
        }
        let start_body: RunStatus = start_resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("cloud(vercel) decode start body: {e}"))?;
        let run_id = start_body
            .run_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("cloud(vercel) response missing run_id"))?;

        // If the initial response already came back terminal, short-circuit.
        if let Some(terminal) = self.terminal_outcome(&start_body)? {
            return Ok(terminal);
        }

        // Poll.
        loop {
            if started.elapsed() >= timeout {
                anyhow::bail!(
                    "cloud(vercel) run {run_id} timed out after {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(self.poll_interval()).await;

            let mut poll_req = http.get(format!(
                "{}/api/run/{}",
                cfg.base_url.trim_end_matches('/'),
                run_id
            ));
            if let Some(tok) = &cfg.token {
                poll_req = poll_req.bearer_auth(tok);
            }
            let poll_resp = poll_req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("cloud(vercel) GET /api/run/{run_id} failed: {e}"))?;
            if !poll_resp.status().is_success() {
                anyhow::bail!(
                    "cloud(vercel) GET /api/run/{run_id} returned HTTP {}",
                    poll_resp.status()
                );
            }
            let body: RunStatus = poll_resp
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("cloud(vercel) decode poll body: {e}"))?;
            if let Some(terminal) = self.terminal_outcome(&body)? {
                return Ok(terminal);
            }
        }
    }

    /// Inspect a poll response: if `status` is terminal, return either the
    /// extracted output (success) or an error (failure). If still in flight,
    /// return `Ok(None)` so the caller continues polling.
    fn terminal_outcome(
        &self,
        body: &RunStatus,
    ) -> anyhow::Result<Option<(String, Option<String>)>> {
        match body.status.as_deref() {
            Some("completed") => {
                let output = body.output.clone().ok_or_else(|| {
                    anyhow::anyhow!("cloud(vercel) status=completed but no output field")
                })?;
                let as_string = match output {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other)
                        .map_err(|e| anyhow::anyhow!("cloud(vercel) serialise output: {e}"))?,
                };
                Ok(Some((as_string, body.sandbox_ref.clone())))
            }
            Some("failed") => {
                let msg = body
                    .error
                    .clone()
                    .unwrap_or_else(|| "no error reported".into());
                anyhow::bail!("cloud(vercel) run failed: {msg}")
            }
            // queued / running / unknown -> keep polling
            _ => Ok(None),
        }
    }

    /// E2B path — stubbed. Returns a clean error pointing users at Vercel.
    async fn dispatch_e2b(
        &self,
        _spec: &AgentSpec,
        _input: &AgentInput,
    ) -> anyhow::Result<AgentRun> {
        // TODO(Track E follow-up): wire E2B properly. Outline:
        //   1. POST https://api.e2b.dev/sandboxes  body: { "template": $E2B_TEMPLATE }
        //   2. PUT /sandboxes/{id}/files/input.json with serialised AgentInput
        //   3. POST /sandboxes/{id}/processes  cmd=claude|codex|gemini per spec.provider
        //   4. Poll GET /sandboxes/{id}/processes/{pid} until exit
        //   5. GET /sandboxes/{id}/files/output.json, parse + validate
        //   6. DELETE /sandboxes/{id}
        // Today we surface a clean error that names the alternative so the
        // operator isn't left guessing.
        anyhow::bail!(
            "e2b: not yet wired; use --cloud-provider vercel or set GROKRXIV_CLOUD_PROVIDER=vercel"
        )
    }
}

/// Shape returned by both `POST /api/run` and `GET /api/run/{id}`. All
/// fields optional so we can decode partial responses defensively.
#[derive(Debug, Clone, serde::Deserialize)]
struct RunStatus {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    sandbox_ref: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Parse strict JSON; if the input is plain text wrapping JSON (e.g. fenced
/// code blocks), strip fences and try again. Mirrors `supervisor::parse_strict_json`.
fn parse_strict_json(s: &str) -> anyhow::Result<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty output");
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => Ok(v),
        Err(_) => {
            let stripped = strip_fences(trimmed);
            serde_json::from_str::<Value>(stripped)
                .map_err(|e| anyhow::anyhow!("not valid JSON: {e}"))
        }
    }
}

fn strip_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokrxiv_schemas::AgentRole;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use uuid::Uuid;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn sample_spec() -> AgentSpec {
        let mut spec = AgentSpec::api_default(
            AgentRole::Summary,
            "claude".into(),
            "claude-opus-4-7".into(),
        );
        spec.runner = AgentRunnerKind::Cloud;
        spec.schema = json!({ "type": "object" });
        spec.timeout_secs = 5; // keep tests bounded
        spec
    }

    fn sample_input() -> AgentInput {
        AgentInput {
            paper_id: Uuid::nil(),
            review_id: Uuid::nil(),
            role: AgentRole::Summary,
            content_hash_material: json!({}),
            artifact: json!({}),
            system_prompt: "be helpful".into(),
            user_prompt: "summarise this paper".into(),
            source_bundle_path: None,
        }
    }

    #[tokio::test]
    async fn test_unsupported_provider_errors() {
        let runner = CloudRunner::new().with_provider("foo".into());
        let err = runner
            .run(&sample_spec(), &sample_input())
            .await
            .expect_err("must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unsupported cloud provider"),
            "expected unsupported-provider message; got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_vercel_requires_url() {
        // Force vercel provider without configuring an override URL; reading
        // env is the only path left. We do NOT mutate the global env in the
        // test (other tests may run concurrently); instead we assert that
        // when the override is absent and the env var is unset, the resolver
        // errors. To keep this hermetic, we temporarily clear the env var
        // for the duration of this call only.
        let runner = CloudRunner::new().with_provider("vercel".into());

        // Snapshot + clear the env var on a single-threaded section.
        let prev = std::env::var("VERCEL_OPEN_AGENTS_URL").ok();
        // SAFETY: tests in this module run on the tokio test runtime; we
        // restore the value before returning.
        std::env::remove_var("VERCEL_OPEN_AGENTS_URL");

        let result = runner.run(&sample_spec(), &sample_input()).await;

        if let Some(v) = prev {
            std::env::set_var("VERCEL_OPEN_AGENTS_URL", v);
        }

        let err = result.expect_err("must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("VERCEL_OPEN_AGENTS_URL"),
            "expected URL-missing message; got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_vercel_payload_shape() {
        let server = MockServer::start().await;
        let captured = Arc::new(std::sync::Mutex::new(None::<Value>));
        let captured_clone = captured.clone();

        Mock::given(method("POST"))
            .and(path("/api/run"))
            .respond_with(move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
                *captured_clone.lock().unwrap() = Some(body);
                ResponseTemplate::new(200).set_body_json(json!({
                    "run_id": "run-1",
                    "status": "completed",
                    "output": { "ok": true },
                    "sandbox_ref": "sb-1"
                }))
            })
            .mount(&server)
            .await;

        let runner = CloudRunner::new()
            .with_provider("vercel".into())
            .with_vercel(server.uri(), None)
            .with_poll_interval(Duration::from_millis(10));

        let run = runner
            .run(&sample_spec(), &sample_input())
            .await
            .expect("vercel completed");

        assert_eq!(run.runner, AgentRunnerKind::Cloud);
        let body = captured.lock().unwrap().clone().expect("body was captured");
        for key in [
            "agent",
            "role",
            "model",
            "provider",
            "system_prompt",
            "user_prompt",
            "schema",
        ] {
            assert!(
                body.get(key).is_some(),
                "expected payload key `{key}` in {body}"
            );
        }
        assert_eq!(body["agent"], json!("grokrxiv-review"));
    }

    #[tokio::test]
    async fn test_vercel_polls_until_completed() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "run_id": "run-poll",
                "status": "queued"
            })))
            .mount(&server)
            .await;

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();

        Mock::given(method("GET"))
            .and(path_regex("^/api/run/run-poll$"))
            .respond_with(move |_req: &Request| {
                let n = calls_clone.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "run_id": "run-poll",
                        "status": "running"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "run_id": "run-poll",
                        "status": "completed",
                        "output": { "ok": true },
                        "sandbox_ref": "sb-poll"
                    }))
                }
            })
            .mount(&server)
            .await;

        let runner = CloudRunner::new()
            .with_provider("vercel".into())
            .with_vercel(server.uri(), None)
            .with_poll_interval(Duration::from_millis(10));

        let run = runner
            .run(&sample_spec(), &sample_input())
            .await
            .expect("vercel polled to completion");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "expected exactly two GET polls before completion"
        );
        assert_eq!(run.output, json!({ "ok": true }));
        assert_eq!(run.sandbox_ref.as_deref(), Some("sb-poll"));
    }

    #[tokio::test]
    async fn test_e2b_stub_returns_clean_error() {
        let runner = CloudRunner::new().with_provider("e2b".into());
        let err = runner
            .run(&sample_spec(), &sample_input())
            .await
            .expect_err("e2b stub must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("vercel"),
            "expected hint about the vercel alternative; got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_sandbox_ref_propagated_in_agentrun() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/run"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "run_id": "run-sb",
                "status": "completed",
                "output": { "ok": true },
                "sandbox_ref": "sandbox-deadbeef"
            })))
            .mount(&server)
            .await;

        let runner = CloudRunner::new()
            .with_provider("vercel".into())
            .with_vercel(server.uri(), None)
            .with_poll_interval(Duration::from_millis(10));

        let run = runner
            .run(&sample_spec(), &sample_input())
            .await
            .expect("vercel completed");

        assert_eq!(run.sandbox_ref.as_deref(), Some("sandbox-deadbeef"));
        assert_eq!(run.runner, AgentRunnerKind::Cloud);
        assert_eq!(run.role, AgentRole::Summary);
        assert!(!run.cache_hit);
        assert!(run.tokens_in.is_none());
        assert!(run.tokens_out.is_none());
    }
}
