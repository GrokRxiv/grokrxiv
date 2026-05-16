//! `submit(payload)` — sentinel tool. Calling `submit` ends the tool-call
//! loop; the payload is validated against the agent's schema by
//! [`crate::agents::extraction::run_tool_loop`] BEFORE this tool's `call()` is
//! reached. If the loop somehow reaches `call()` directly, that's a logic bug
//! and we surface it loudly.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::extraction::{Tool, ToolCtx};

/// Implements `submit`.
pub struct SubmitTool;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_schema() -> Value {
    // The submit tool's argument schema is the agent's own output schema —
    // each `ExtractionAgent` overrides this in its `tools()` listing. The
    // default registered spec here is a "any JSON object" placeholder so
    // unit tests still see a sensible shape.
    json!({
        "type": "object",
        "description": "Final payload matching the agent's output schema."
    })
}

#[async_trait]
impl Tool for SubmitTool {
    fn name(&self) -> &'static str {
        "submit"
    }
    fn description(&self) -> &'static str {
        "Finalise extraction. The argument MUST validate against the agent's output schema."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, _args: Value, _ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        anyhow::bail!("submit must be intercepted by the tool-call loop")
    }
}
