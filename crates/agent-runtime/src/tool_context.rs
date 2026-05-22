//! Generic tool execution context passed from runner backends to app tools.

use std::path::Path;
use std::sync::Arc;

/// Borrowed context handed to app-owned tools.
pub struct ToolCtx<'a> {
    /// Working directory the app prepared for tool execution.
    pub workdir: &'a Path,
    /// Optional semantic AST available to tools that understand it.
    pub semantic_ast: Option<&'a serde_json::Value>,
    /// Source identifier for app tools that need external lookups.
    pub source_id: &'a str,
    /// Backward-compatible arXiv identifier field for GrokRxiv tools during
    /// the app split. New app code should prefer `source_id`.
    pub arxiv_id: &'a str,
    /// Shared HTTP client for network-capable tools.
    pub http: Arc<reqwest::Client>,
}

impl<'a> ToolCtx<'a> {
    /// Override the HTTP client, mainly for tests.
    pub fn with_http(mut self, http: Arc<reqwest::Client>) -> Self {
        self.http = http;
        self
    }
}
