//! `read_file(path, byte_start?, byte_end?)` — read a slice of a file in the
//! unpacked source bundle.
//!
//! Path is interpreted RELATIVE to `ctx.workdir`. Absolute paths and any path
//! that escapes the workdir (via `..`) are rejected. Returns `{content,
//! truncated, bytes_read}`. Hard cap of 50KB per call so the LLM doesn't
//! blow its context window on one tool call.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::extraction::{Tool, ToolCtx};

/// Implements `read_file`.
pub struct ReadFileTool;

/// Hard cap on bytes returned per call.
pub const MAX_BYTES_PER_CALL: usize = 50_000;

static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

fn build_schema() -> Value {
    json!({
        "type": "object",
        "required": ["path"],
        "properties": {
            "path": { "type": "string", "description": "Path relative to the workdir." },
            "byte_start": { "type": "integer", "description": "Optional 0-based start byte." },
            "byte_end": { "type": "integer", "description": "Optional exclusive end byte." }
        }
    })
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a slice (or all) of a file under the workdir. Returns {content, truncated}."
    }
    fn schema(&self) -> &Value {
        SCHEMA.get_or_init(build_schema)
    }
    async fn call(&self, args: Value, ctx: &ToolCtx<'_>) -> anyhow::Result<Value> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("read_file requires `path`"))?;
        let byte_start = args
            .get("byte_start")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(0);
        let byte_end = args
            .get("byte_end")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        let rel = std::path::Path::new(path);
        if rel.is_absolute() {
            anyhow::bail!("read_file: absolute paths are not allowed");
        }
        let full = ctx.workdir.join(rel);
        let canon_root = ctx
            .workdir
            .canonicalize()
            .unwrap_or_else(|_| ctx.workdir.to_path_buf());
        let canon_full = match full.canonicalize() {
            Ok(p) => p,
            Err(e) => anyhow::bail!("read_file: could not open `{path}`: {e}"),
        };
        if !canon_full.starts_with(&canon_root) {
            anyhow::bail!("read_file: path escapes workdir");
        }

        let bytes = std::fs::read(&canon_full).map_err(|e| anyhow::anyhow!("read_file: {e}"))?;
        let total = bytes.len();
        let end = byte_end.unwrap_or(total).min(total);
        let start = byte_start.min(end);
        let cap_end = (start + MAX_BYTES_PER_CALL).min(end);
        let slice = &bytes[start..cap_end];
        let truncated = cap_end < end;
        // Lossy decode: extraction agents don't care about preserving
        // non-UTF-8 noise; the workdir is paper source, not binary blobs.
        let content = String::from_utf8_lossy(slice).into_owned();
        Ok(json!({
            "content": content,
            "truncated": truncated,
            "bytes_read": slice.len(),
            "total_bytes": total,
        }))
    }
}
