//! Generic AgentHero agent runtime contracts.
//!
//! This crate contains the neutral contracts used by DAG apps and runner
//! backends. App-specific agent implementations live in app crates.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app_protocol;
pub mod runner;
pub mod tool_context;
pub mod types;

pub use app_protocol::{AppAdapterRequest, AppAdapterResponse, APP_ADAPTER_PROTOCOL};
pub use runner::AgentRunner;
pub use tool_context::ToolCtx;
pub use types::{
    AgentInput, AgentMode, AgentRun, AgentRunnerKind, AgentSchema, AgentSpec, Message,
    RevisionTarget, RoleSpecMap, SandboxPolicy, ToolCall, ToolCompletion, ToolContent, ToolMessage,
    ToolSpec,
};
