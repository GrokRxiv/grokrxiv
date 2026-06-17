//! The tool-call loop that drives every extraction agent.
//!
//! ```text
//! loop:
//!   resp = runner.complete_with_tools(messages, tools, ctx)
//!   for each tool_call in resp.tool_calls:
//!     if name == "submit":
//!         validate payload vs agent.schema()
//!         return Ok(ExtractionRun{...})
//!     else:
//!         result = registry[name].call(args, ctx)
//!         messages.append(assistant tool_use + user tool_result)
//!   if no tool_calls: bail
//!   if cost > ceiling: bail
//!   if iters > max_iters: bail
//! ```

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};

use crate::extraction::{
    tool_ctx_from_extraction, ExtractionAgent, ExtractionContext, ExtractionRun, ToolCallRecord,
    ToolCtx,
};
use agenthero_agent_runtime::AgentRunner;
use agenthero_agent_runtime::{AgentSpec, Message, ToolCall, ToolContent};
use grokrxiv_llm_adapter::Role as LlmRole;

/// Run the multi-turn tool-call loop until the agent calls `submit(...)`.
///
/// `max_iters` bounds the number of model turns (NOT the number of tool calls
/// — a single turn may produce many parallel tool calls). `max_cost_usd` is a
/// best-effort dollar ceiling; we bail as soon as we cross it.
pub async fn run_tool_loop(
    agent: &dyn ExtractionAgent,
    runner: Arc<dyn AgentRunner>,
    spec: &AgentSpec,
    ctx: ExtractionContext<'_>,
    max_iters: usize,
    max_cost_usd: f32,
) -> anyhow::Result<ExtractionRun> {
    let started = Instant::now();
    let tool_ctx = tool_ctx_from_extraction(&ctx);
    let tools = agent.tools();

    let mut messages: Vec<Message> = vec![Message {
        role: LlmRole::User,
        content: vec![ToolContent::Text {
            text: agent.user_kickoff(&ctx),
        }],
    }];

    let mut prior_messages = with_system(agent.system_prompt(), messages.clone());

    let mut tool_call_log: Vec<ToolCallRecord> = Vec::new();
    let mut iters: u32 = 0;
    let mut cost_accum: f32 = 0.0;
    let mut retried_submit = false;
    let mut retried_empty_tool_calls = false;

    while (iters as usize) < max_iters {
        let resp = runner
            .complete_with_tools(spec, &prior_messages, &tools, &tool_ctx)
            .await?;
        cost_accum += estimated_cost_usd(spec, &resp.usage);
        if cost_accum > max_cost_usd {
            anyhow::bail!(
                "extraction agent {} exceeded cost ceiling ${} (accum=${})",
                agent.name(),
                max_cost_usd,
                cost_accum
            );
        }

        if resp.tool_calls.is_empty() {
            if !retried_empty_tool_calls {
                retried_empty_tool_calls = true;
                if !resp.text.trim().is_empty() {
                    messages.push(Message {
                        role: LlmRole::Assistant,
                        content: vec![ToolContent::Text {
                            text: resp.text.clone(),
                        }],
                    });
                }
                messages.push(Message {
                    role: LlmRole::User,
                    content: vec![ToolContent::Text {
                        text: "You returned no tool calls. Continue by returning exactly one \
                               GrokRxiv tool-call envelope whose tool_calls array contains at \
                               least one call. If extraction is complete, call submit(...)."
                            .to_string(),
                    }],
                });
                prior_messages = with_system(agent.system_prompt(), messages.clone());
                iters += 1;
                continue;
            }
            anyhow::bail!(
                "extraction agent {} stopped without submit() on iter {}",
                agent.name(),
                iters
            );
        }

        // Append the assistant turn that issued the tool calls.
        let mut assistant_content: Vec<ToolContent> = Vec::new();
        if !resp.text.is_empty() {
            assistant_content.push(ToolContent::Text {
                text: resp.text.clone(),
            });
        }
        for call in &resp.tool_calls {
            assistant_content.push(ToolContent::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.arguments.clone(),
            });
        }
        messages.push(Message {
            role: LlmRole::Assistant,
            content: assistant_content,
        });

        // Process each tool call. If we see `submit`, validate + return.
        let mut user_results: Vec<ToolContent> = Vec::new();
        for call in &resp.tool_calls {
            let call_started = Instant::now();
            if call.name == "submit" {
                let normalized_submit = normalize_submit_payload(agent.name(), &call.arguments);
                match validate_submit(&normalized_submit, agent.schema()) {
                    Ok(()) => {
                        tool_call_log.push(ToolCallRecord {
                            iter: iters,
                            tool: "submit".to_string(),
                            arguments: normalized_submit.clone(),
                            result: normalized_submit.clone(),
                            ok: true,
                            latency_ms: call_started.elapsed().as_millis() as i64,
                        });
                        return Ok(ExtractionRun {
                            output: normalized_submit,
                            tool_calls: tool_call_log,
                            cost_usd: cost_accum,
                            latency_ms: started.elapsed().as_millis() as i64,
                            iters: iters + 1,
                        });
                    }
                    Err(e) if !retried_submit => {
                        // One corrective retry: append a tool_result that
                        // explains the validation failure and let the agent
                        // try again on the next turn.
                        retried_submit = true;
                        let err_msg = format!(
                            "submit() payload failed schema validation: {e}.\n\n\
                             Your output MUST validate against this exact JSON Schema. The \
                             top-level object has ONLY the keys the schema declares — do NOT \
                             wrap results in `nodes`/`edges`/`title`; emit the single declared \
                             array directly and fold any dependency edges into each item's \
                             `depends_on` field. Call submit again with a corrected payload.\n\n\
                             Schema:\n{schema}",
                            schema = serde_json::to_string(agent.schema()).unwrap_or_default(),
                        );
                        tool_call_log.push(ToolCallRecord {
                            iter: iters,
                            tool: "submit".to_string(),
                            arguments: normalized_submit,
                            result: json!({ "_error": err_msg.clone() }),
                            ok: false,
                            latency_ms: call_started.elapsed().as_millis() as i64,
                        });
                        user_results.push(ToolContent::ToolResult {
                            tool_use_id: call.id.clone(),
                            content: Value::String(err_msg),
                            is_error: true,
                        });
                    }
                    Err(e) => {
                        anyhow::bail!(
                            "extraction agent {} submit() failed validation twice: {e}",
                            agent.name(),
                        );
                    }
                }
            } else {
                let result = invoke_tool(call, &ctx, &tool_ctx).await;
                let latency_ms = call_started.elapsed().as_millis() as i64;
                match result {
                    Ok(v) => {
                        tool_call_log.push(ToolCallRecord {
                            iter: iters,
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: v.clone(),
                            ok: true,
                            latency_ms,
                        });
                        user_results.push(ToolContent::ToolResult {
                            tool_use_id: call.id.clone(),
                            content: v,
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        tool_call_log.push(ToolCallRecord {
                            iter: iters,
                            tool: call.name.clone(),
                            arguments: call.arguments.clone(),
                            result: json!({ "_error": err_msg.clone() }),
                            ok: false,
                            latency_ms,
                        });
                        user_results.push(ToolContent::ToolResult {
                            tool_use_id: call.id.clone(),
                            content: Value::String(err_msg),
                            is_error: true,
                        });
                    }
                }
            }
        }

        if !user_results.is_empty() {
            messages.push(Message {
                role: LlmRole::User,
                content: user_results,
            });
        }
        prior_messages = with_system(agent.system_prompt(), messages.clone());
        iters += 1;
    }

    anyhow::bail!(
        "extraction agent {} exhausted max_iters={}",
        agent.name(),
        max_iters
    );
}

fn with_system(system: String, mut body: Vec<Message>) -> Vec<Message> {
    // External audit (H2): the runners' `complete_with_tools` impls send
    // `system: None`, and this function used to drop the agent's system
    // prompt entirely. As a result every extraction agent ran without its
    // persona / output contract, which is why agents "stopped without
    // submit()" - they had no idea what they were supposed to produce.
    //
    // Until the AgentRunner trait grows a dedicated `system` parameter, we
    // smuggle the prompt in as a leading user message so the model sees it
    // on every turn. This is suboptimal for Anthropic's prompt-caching (the
    // system slot is the cacheable one) but it's correct for behaviour.
    let system = system.trim().to_string();
    if system.is_empty() {
        return body;
    }
    body.insert(
        0,
        Message {
            role: LlmRole::User,
            content: vec![ToolContent::Text { text: system }],
        },
    );
    body
}

async fn invoke_tool(
    call: &ToolCall,
    ctx: &ExtractionContext<'_>,
    tool_ctx: &ToolCtx<'_>,
) -> anyhow::Result<Value> {
    let tool = ctx
        .registry
        .get(&call.name)
        .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", call.name))?;
    tool.call(call.arguments.clone(), tool_ctx).await
}

fn normalize_submit_payload(agent_name: &str, payload: &Value) -> Value {
    let mut out = payload.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };

    match agent_name {
        "macro_expander" => {
            obj.entry("reason".to_string()).or_insert(Value::Null);
            if let Some(Value::Array(items)) = obj.get_mut("expansions_applied") {
                for item in items.iter_mut() {
                    if let Some(s) = item.as_str() {
                        *item = normalize_macro_expansion_string(s);
                    }
                }
            }
        }
        "theorem_graph_extractor" => {
            normalize_theorem_graph_shape(obj);
            normalize_reason_to_null_when_nonempty(obj, "theorem_graph");
        }
        "equation_canonicalizer" => {
            normalize_reason_to_null_when_nonempty(obj, "equations");
            if let Some(Value::Array(items)) = obj.get_mut("equations") {
                for item in items.iter_mut() {
                    if let Some(eq) = item.as_object_mut() {
                        if !eq.contains_key("mathml") || eq.get("mathml") == Some(&Value::Null) {
                            eq.insert("mathml".to_string(), Value::String(String::new()));
                        }
                    }
                }
            }
        }
        _ => {}
    }

    out
}

/// LLMs (Claude especially) frequently emit the theorem graph in its natural
/// `{nodes, edges, title}` object form instead of the schema's `{theorem_graph: [...],
/// reason}` (a flat array of nodes, each carrying its own `depends_on`). A correct,
/// content-complete extraction must not be discarded over that structural rename, so
/// coerce it: lift `nodes` -> `theorem_graph`, fold a separate `edges` (`from`->`to`,
/// relation `depends_on`) list into each node's `depends_on`, map `kind`->`type` and
/// `location`->`section`, keep ONLY schema-allowed node keys, and reduce the top level to
/// exactly `{theorem_graph, reason}` (the schema is `additionalProperties: false`).
fn normalize_theorem_graph_shape(obj: &mut serde_json::Map<String, Value>) {
    // The node array may already be `theorem_graph` (array), or under `nodes`, or nested as
    // `theorem_graph.{nodes,edges}` if the whole graph object was placed there.
    let mut nodes: Option<Vec<Value>> = None;
    let mut edges: Option<Vec<Value>> = None;
    match obj.get("theorem_graph") {
        Some(Value::Array(arr)) => nodes = Some(arr.clone()),
        Some(Value::Object(graph)) => {
            if let Some(Value::Array(arr)) = graph.get("nodes") {
                nodes = Some(arr.clone());
            }
            if let Some(Value::Array(e)) = graph.get("edges") {
                edges = Some(e.clone());
            }
        }
        _ => {}
    }
    if nodes.is_none() {
        if let Some(Value::Array(arr)) = obj.get("nodes") {
            nodes = Some(arr.clone());
        }
    }
    if edges.is_none() {
        if let Some(Value::Array(e)) = obj.get("edges") {
            edges = Some(e.clone());
        }
    }
    let Some(nodes) = nodes else {
        return;
    };

    // id -> [depends_on...] from any separate `edges` list (relation == "depends_on").
    let mut edge_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for edge in edges.into_iter().flatten() {
        let rel = edge
            .get("relation")
            .and_then(Value::as_str)
            .unwrap_or("depends_on");
        if rel != "depends_on" {
            continue;
        }
        if let (Some(from), Some(to)) = (
            edge.get("from").and_then(Value::as_str),
            edge.get("to").and_then(Value::as_str),
        ) {
            edge_map
                .entry(from.to_string())
                .or_default()
                .push(to.to_string());
        }
    }

    let cleaned: Vec<Value> = nodes
        .into_iter()
        .filter_map(|node| {
            let src = node.as_object()?;
            let id = src.get("id").and_then(Value::as_str)?.to_string();
            let mut out = serde_json::Map::new();
            out.insert("id".to_string(), Value::String(id.clone()));
            out.insert(
                "type".to_string(),
                src.get("type")
                    .or_else(|| src.get("kind"))
                    .cloned()
                    .unwrap_or_else(|| Value::String("theorem".to_string())),
            );
            out.insert(
                "section".to_string(),
                src.get("section")
                    .or_else(|| src.get("location"))
                    .cloned()
                    .unwrap_or_else(|| Value::String(String::new())),
            );
            out.insert(
                "statement".to_string(),
                src.get("statement")
                    .or_else(|| src.get("label"))
                    .or_else(|| src.get("note"))
                    .cloned()
                    .unwrap_or_else(|| Value::String(String::new())),
            );
            let depends_on = match src.get("depends_on") {
                Some(Value::Array(existing)) => Value::Array(existing.clone()),
                _ => Value::Array(
                    edge_map
                        .get(&id)
                        .map(|tos| tos.iter().cloned().map(Value::String).collect())
                        .unwrap_or_default(),
                ),
            };
            out.insert("depends_on".to_string(), depends_on);
            if let Some(src_tex) = src.get("source_tex") {
                out.insert("source_tex".to_string(), src_tex.clone());
            }
            // typed_transcription / theorem_ir have strict sub-schemas; drop them here so a
            // malformed optional block can't sink an otherwise-correct extraction. The LLM
            // Lean author works from `statement`, and the deterministic IR is only a hint.
            Some(Value::Object(out))
        })
        .collect();

    let reason = obj.get("reason").cloned();
    obj.clear();
    obj.insert("theorem_graph".to_string(), Value::Array(cleaned));
    if let Some(reason) = reason {
        obj.insert("reason".to_string(), reason);
    }
}

fn normalize_reason_to_null_when_nonempty(
    obj: &mut serde_json::Map<String, Value>,
    collection_key: &str,
) {
    let has_items = obj
        .get(collection_key)
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    if has_items {
        obj.insert("reason".to_string(), Value::Null);
    }
}

fn normalize_macro_expansion_string(s: &str) -> Value {
    let (name, body) = s
        .split_once("→")
        .or_else(|| s.split_once("->"))
        .map(|(name, body)| (name.trim(), body.trim()))
        .unwrap_or_else(|| (s.trim(), ""));
    json!({
        "name": name,
        "body": body,
        "occurrences": 1,
    })
}

fn validate_submit(payload: &Value, schema: &Value) -> anyhow::Result<()> {
    // Empty / null schema = no constraint (used by unit tests with stub agents).
    if schema.is_null()
        || (schema.is_object() && schema.as_object().map(|m| m.is_empty()).unwrap_or(false))
    {
        return Ok(());
    }
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| anyhow::anyhow!("invalid agent schema: {e}"))?;
    let errors: Vec<String> = validator
        .iter_errors(payload)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        anyhow::bail!("{}", errors.join("; "));
    }
    Ok(())
}

/// Best-effort USD cost estimate using a flat per-million rate. We deliberately
/// avoid a real price table here — the orchestrator's costs.yaml is the source
/// of truth for production; this is a coarse ceiling check.
fn estimated_cost_usd(_spec: &AgentSpec, usage: &grokrxiv_llm_adapter::Usage) -> f32 {
    // Rough average across providers: $3/M input, $15/M output. Good enough
    // for the cost-ceiling guard; precise accounting happens downstream.
    let in_usd = (usage.tokens_in as f32) * 3.0 / 1_000_000.0;
    let out_usd = (usage.tokens_out as f32) * 15.0 / 1_000_000.0;
    in_usd + out_usd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::{ExtractionAgent, ExtractionRole, ToolRegistry};

    #[test]
    fn theorem_graph_nodes_edges_shape_is_normalized_and_validates() {
        // The exact malformation Claude Opus emitted on arXiv:2606.12482: a `{nodes, edges,
        // title}` graph object instead of the schema's `{theorem_graph: [...], reason}`.
        let payload = json!({
            "title": "Categorical Hopf map",
            "nodes": [
                {"id": "Ehresconn", "kind": "definition", "location": "sec-4",
                 "statement": "Let G be a Lie group and pi:P->M a principal G-bundle...",
                 "source_tex": null, "note": "Definition 1"},
                {"id": "giveHopf", "kind": "proposition", "location": "sec-8",
                 "statement": "The categorical Hopf map exists.", "depends_on": ["Ehresconn"]}
            ],
            "edges": [
                {"from": "giveHopf", "to": "priGtoG", "relation": "depends_on"},
                {"from": "Ehresconn", "to": "cocycleg", "relation": "mentions"}
            ]
        });

        let normalized = normalize_submit_payload("theorem_graph_extractor", &payload);

        // Top level is exactly {theorem_graph, reason} — nodes/edges/title gone.
        let obj = normalized.as_object().unwrap();
        assert!(obj.contains_key("theorem_graph") && obj.contains_key("reason"));
        assert!(!obj.contains_key("nodes") && !obj.contains_key("edges") && !obj.contains_key("title"));
        let tg = obj["theorem_graph"].as_array().unwrap();
        assert_eq!(tg.len(), 2);
        // kind->type, location->section, depends_on present (node's own kept; edge folded in).
        assert_eq!(tg[0]["type"], "definition");
        assert_eq!(tg[0]["section"], "sec-4");
        assert_eq!(tg[0]["depends_on"], json!([])); // only `mentions` edge existed -> not folded
        assert_eq!(tg[1]["depends_on"], json!(["Ehresconn"])); // node's own depends_on preserved
        assert_eq!(obj["reason"], serde_json::Value::Null);

        // The whole point: it now validates against the REAL theorems schema.
        let agent = crate::extraction::theorems::TheoremGraphExtractorAgent::new();
        validate_submit(&normalized, agent.schema())
            .expect("normalized theorem graph must validate against theorems.schema.json");
    }

    use agenthero_agent_runtime::AgentSpec;
    use async_trait::async_trait;
    use grokrxiv_llm_adapter::{FinishReason, ProviderToolCall, ToolCompletion, ToolSpec, Usage};
    use grokrxiv_schemas::PaperExtract;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// A scripted runner: hand it a queue of [`ToolCompletion`]s and it
    /// returns them in order, ignoring the messages/tools it receives. Used
    /// by every loop-level unit test.
    pub struct ScriptedRunner {
        queue: Mutex<Vec<ToolCompletion>>,
        pub seen_tools: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedRunner {
        pub fn new(turns: Vec<ToolCompletion>) -> Self {
            Self {
                queue: Mutex::new(turns),
                seen_tools: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AgentRunner for ScriptedRunner {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn run(
            &self,
            _spec: &AgentSpec,
            _input: &agenthero_agent_runtime::AgentInput,
        ) -> anyhow::Result<agenthero_agent_runtime::AgentRun> {
            unimplemented!("scripted runner is tool-only")
        }
        async fn complete_with_tools(
            &self,
            _spec: &AgentSpec,
            _messages: &[Message],
            tools: &[ToolSpec],
            _ctx: &ToolCtx<'_>,
        ) -> anyhow::Result<ToolCompletion> {
            self.seen_tools
                .lock()
                .unwrap()
                .push(tools.iter().map(|t| t.name.clone()).collect());
            let mut q = self.queue.lock().unwrap();
            if q.is_empty() {
                anyhow::bail!("scripted runner queue exhausted");
            }
            Ok(q.remove(0))
        }
    }

    /// A trivial extraction agent whose schema accepts anything. We use it
    /// for all loop tests.
    struct FakeAgent {
        tools: Vec<ToolSpec>,
        schema: serde_json::Value,
    }
    impl FakeAgent {
        fn new(extra_schema: Option<serde_json::Value>) -> Self {
            let registry = ToolRegistry::with_core_tools();
            let tools = registry.specs();
            Self {
                tools,
                schema: extra_schema.unwrap_or_else(|| json!({})),
            }
        }
    }
    #[async_trait]
    impl ExtractionAgent for FakeAgent {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn role(&self) -> ExtractionRole {
            ExtractionRole::MacroExpander
        }
        fn schema(&self) -> &serde_json::Value {
            &self.schema
        }
        fn tools(&self) -> Vec<ToolSpec> {
            self.tools.clone()
        }
    }

    fn paper_extract() -> PaperExtract {
        PaperExtract {
            arxiv_id: "2401.00001v1".into(),
            title: "A Toy Paper".into(),
            authors: vec![],
            abstract_: "abs".into(),
            field: None,
            sections: vec![],
            figures: vec![],
            bibliography: vec![],
            source_format: None,
        }
    }

    fn ctx<'a>(
        workdir: &'a std::path::Path,
        extract: &'a PaperExtract,
        registry: Arc<ToolRegistry>,
    ) -> ExtractionContext<'a> {
        ExtractionContext {
            workdir,
            extract,
            semantic_ast: None,
            paper_id: uuid::Uuid::nil(),
            arxiv_id: "2401.00001v1",
            registry,
            max_cost_usd: 1.0,
            max_iters: 5,
        }
    }

    fn fake_spec() -> AgentSpec {
        AgentSpec::api_default("summary", "claude".to_string(), "claude-test".to_string())
    }

    fn turn_submit(payload: serde_json::Value) -> ToolCompletion {
        ToolCompletion {
            tool_calls: vec![ProviderToolCall {
                id: "call_submit".into(),
                name: "submit".into(),
                arguments: payload,
            }],
            text: String::new(),
            finish_reason: FinishReason::ToolUse,
            usage: Usage::default(),
            raw: json!({}),
        }
    }

    fn turn_call(name: &str, args: serde_json::Value, id: &str) -> ToolCompletion {
        ToolCompletion {
            tool_calls: vec![ProviderToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args,
            }],
            text: String::new(),
            finish_reason: FinishReason::ToolUse,
            usage: Usage::default(),
            raw: json!({}),
        }
    }

    fn turn_empty() -> ToolCompletion {
        ToolCompletion {
            tool_calls: vec![],
            text: "I give up".into(),
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            raw: json!({}),
        }
    }

    #[tokio::test]
    async fn happy_path_submit_terminates() {
        let agent = FakeAgent::new(None);
        let runner: Arc<dyn AgentRunner> =
            Arc::new(ScriptedRunner::new(vec![turn_submit(json!({"ok": 1}))]));
        let tmp = tempdir();
        let pe = paper_extract();
        let registry = Arc::new(ToolRegistry::with_core_tools());
        let ec = ctx(tmp.path(), &pe, registry.clone());
        let spec = fake_spec();
        let run = run_tool_loop(&agent, runner, &spec, ec, 5, 1.0)
            .await
            .unwrap();
        assert_eq!(run.output, json!({"ok": 1}));
        assert_eq!(run.iters, 1);
        assert_eq!(run.tool_calls.len(), 1);
        assert_eq!(run.tool_calls[0].tool, "submit");
    }

    #[tokio::test]
    async fn invokes_tools_then_submits() {
        let agent = FakeAgent::new(None);
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_call("list_files", json!({}), "c1"),
            turn_submit(json!({"done": true})),
        ]));
        let tmp = tempdir();
        // touch a file so list_files has something
        std::fs::write(tmp.path().join("main.tex"), "hi").unwrap();
        let pe = paper_extract();
        let registry = Arc::new(ToolRegistry::with_core_tools());
        let ec = ctx(tmp.path(), &pe, registry.clone());
        let spec = fake_spec();
        let run = run_tool_loop(&agent, runner, &spec, ec, 5, 1.0)
            .await
            .unwrap();
        assert_eq!(run.output, json!({"done": true}));
        assert_eq!(run.iters, 2);
        assert!(
            run.tool_calls
                .iter()
                .any(|c| c.tool == "list_files" && c.ok),
            "expected a successful list_files entry, got {:?}",
            run.tool_calls
        );
    }

    #[tokio::test]
    async fn cost_ceiling_aborts() {
        let agent = FakeAgent::new(None);
        // Cheap usage isn't enough; we craft an expensive turn that
        // immediately busts the ceiling.
        let mut t = turn_call("list_files", json!({}), "c1");
        t.usage = Usage {
            tokens_in: 10_000_000,
            tokens_out: 10_000_000,
            cache_hits: 0,
        };
        let runner: Arc<dyn AgentRunner> =
            Arc::new(ScriptedRunner::new(vec![t, turn_submit(json!({}))]));
        let tmp = tempdir();
        let pe = paper_extract();
        let registry = Arc::new(ToolRegistry::with_core_tools());
        let ec = ctx(tmp.path(), &pe, registry.clone());
        let spec = fake_spec();
        let err = run_tool_loop(&agent, runner, &spec, ec, 5, 0.01)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("cost ceiling"),
            "expected cost ceiling error, got {err}"
        );
    }

    #[tokio::test]
    async fn schema_invalid_submit_retries_once() {
        // Schema requires `id: integer`. First submit is wrong type, second is right.
        let schema = json!({
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "integer" } }
        });
        let agent = FakeAgent::new(Some(schema));
        let runner: Arc<dyn AgentRunner> = Arc::new(ScriptedRunner::new(vec![
            turn_submit(json!({"id": "oops"})),
            turn_submit(json!({"id": 7})),
        ]));
        let tmp = tempdir();
        let pe = paper_extract();
        let registry = Arc::new(ToolRegistry::with_core_tools());
        let ec = ctx(tmp.path(), &pe, registry.clone());
        let spec = fake_spec();
        let run = run_tool_loop(&agent, runner, &spec, ec, 5, 1.0)
            .await
            .unwrap();
        assert_eq!(run.output, json!({"id": 7}));
        // First submit recorded as failure, second as success.
        let submits: Vec<&ToolCallRecord> = run
            .tool_calls
            .iter()
            .filter(|c| c.tool == "submit")
            .collect();
        assert_eq!(submits.len(), 2);
        assert!(!submits[0].ok);
        assert!(submits[1].ok);
    }

    #[tokio::test]
    async fn no_tool_call_aborts() {
        let agent = FakeAgent::new(None);
        let runner: Arc<dyn AgentRunner> =
            Arc::new(ScriptedRunner::new(vec![turn_empty(), turn_empty()]));
        let tmp = tempdir();
        let pe = paper_extract();
        let registry = Arc::new(ToolRegistry::with_core_tools());
        let ec = ctx(tmp.path(), &pe, registry.clone());
        let spec = fake_spec();
        let err = run_tool_loop(&agent, runner, &spec, ec, 5, 1.0)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("stopped without submit"),
            "expected stopped-without-submit error, got {err}"
        );
    }

    /// Minimal temp-dir helper: makes a fresh directory under `$TMPDIR` and
    /// returns it as a guard that removes itself on drop. We don't want to
    /// pull in `tempfile` just for these tests.
    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    pub fn tempdir() -> TempDir {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grokrxiv-extraction-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
