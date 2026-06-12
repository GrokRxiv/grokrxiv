//! GrokRxiv-specific values carried through the generic AgentHero agent input.

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

/// Context key carrying the GrokRxiv paper UUID as a string.
pub const PAPER_ID_CONTEXT_KEY: &str = "paper_id";
/// Context key carrying the GrokRxiv review UUID as a string.
pub const REVIEW_ID_CONTEXT_KEY: &str = "review_id";

/// Build a generic AgentHero context map for GrokRxiv review agents.
pub fn grokrxiv_agent_context(paper_id: Uuid, review_id: Uuid) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            PAPER_ID_CONTEXT_KEY.to_string(),
            Value::String(paper_id.to_string()),
        ),
        (
            REVIEW_ID_CONTEXT_KEY.to_string(),
            Value::String(review_id.to_string()),
        ),
    ])
}

/// Build a generic AgentHero context map for review-scoped helper agents.
pub fn review_only_agent_context(review_id: Uuid) -> BTreeMap<String, Value> {
    BTreeMap::from([(
        REVIEW_ID_CONTEXT_KEY.to_string(),
        Value::String(review_id.to_string()),
    )])
}
