# AgentHero App Dispatch Research

Date: 2026-05-22

## Current Decision

Keep process adapters for this patch. The production path should prefer
compiled app adapter binaries; `cargo run` remains a development fallback only.
Do not link `agenthero-orchestrator` directly to GrokRxiv or C2Rust app crates
until the broader dispatch model is chosen.

## Options

1. In-process trait dispatch behind workspace features.
   Fast local startup and simple debugging, but every enabled app becomes a
   compile-time dependency of the platform binary. This weakens the installable
   DAGOps app boundary unless features are generated from an app registry.

2. Dynamic plugin loading.
   Preserves app isolation and can avoid subprocess startup overhead, but adds
   ABI/versioning risk and a more complex packaging story. This needs a
   deliberate plugin contract before implementation.

3. Process adapters with compiled binaries.
   Keeps app boundaries clean and works for any language. The cost is process
   startup and a final JSON response boundary. This is the current stable
   choice for local and distributed DAGApps.

## Next Research Step

Benchmark `agh app run grokrxiv -- validate-citations --dry-run --json` across:

- compiled adapter binary
- `cargo run` fallback
- a prototype in-process feature build

The winning direction must preserve the core rule: AgentHero orchestrates apps;
apps own their domain code and can be Rust, CLI, or another language.
