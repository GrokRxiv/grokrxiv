//! Prompt rendering for the supervisor-as-parent MVP.
//!
//! The supervisor builds a single Markdown prompt and writes it to
//! `input_dir/prompt.md`. The prompt contains:
//! 1. A fixed system-style preamble that locks the agent contract.
//! 2. The full `review_input.json` payload, embedded as a fenced block.
//!
//! The agent must emit ONE JSON object on stdout with exactly three fields:
//! `review_md`, `verdict_json`, `audit_json`. Any prose before or after the
//! JSON object causes the supervisor to flip the run to `InvalidOutput`.

use std::path::Path;

/// Render the prompt for one review job. `review_input_json` is the bytes
/// of the prepared `review_input.json`; we embed it verbatim so the agent
/// has every field the deterministic pipeline populated.
pub fn render_review_prompt(review_input_json: &str) -> String {
    format!(
        "{PREAMBLE}\n\n## Paper context (`review_input.json`)\n\n```json\n{body}\n```\n\n{TRAILER}\n",
        body = review_input_json.trim(),
    )
}

/// Convenience — load `review_input.json` from disk and render.
pub fn render_review_prompt_from_path(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
    Ok(render_review_prompt(&bytes))
}

const PREAMBLE: &str = "\
You are a GrokRxiv specialist reviewer. You have been spawned by the GrokRxiv \
Rust supervisor as a single-shot subprocess worker. Your ONLY job is to read \
the JSON object embedded below (the paper's prepared context) and emit a \
structured review on stdout.

Hard constraints:
- You may not call other agents, other CLIs, or any external service.
- You may not write to the input directory.
- You must emit exactly one JSON object on stdout with the three fields below.
- No prose before the JSON. No prose after the JSON. No code fences around it.

Required JSON shape (top-level fields, all REQUIRED):

```
{
  \"review_md\":     \"<full markdown body of the review, audience: peer reviewer>\",
  \"verdict_json\":  {
                       \"recommendation\": \"accept|minor_revision|major_revision|reject\",
                       \"confidence\":     0.0,
                       \"summary\":        \"<one-paragraph TL;DR>\",
                       \"strengths\":      [\"<string>\", ...],
                       \"weaknesses\":     [\"<string>\", ...]
                     },
  \"audit_json\":    {\"agent\": \"<your name>\", \"model\": \"<model id>\", \"notes\": \"<reasoning trace, short>\"}
}
```

The supervisor parses your stdout and writes the three fields to
`output/review.md`, `output/verdict.json`, `output/audit.json`. Any
deviation from the shape above will cause your run to be rejected.";

const TRAILER: &str = "\
Now produce the JSON object. Remember: ONE JSON object on stdout, nothing else.";
