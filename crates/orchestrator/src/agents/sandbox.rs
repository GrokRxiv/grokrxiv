//! `SandboxPolicy::Container` execution helpers.
//!
//! Wraps a runner invocation in a Docker container with read-only CLI auth
//! mounts and a per-run scratch workdir. Track D fills in the body.
//!
//! The wrapper is invoked by `CliRunner` and `LocalInferenceRunner` when
//! their spec carries `SandboxPolicy::Container`. `CloudRunner` ignores it
//! (cloud is inherently sandboxed). `ApiRunner` ignores it (no execution
//! environment to isolate).

use crate::agents::types::{AgentInput, AgentRun, AgentSpec};

/// Run the supplied runner inside a Docker container.
///
/// Mount strategy:
/// - `${HOME}/.claude` → `/home/agent/.claude:ro`
/// - `${HOME}/.codex` → `/home/agent/.codex:ro`
/// - `${HOME}/.config/gemini` → `/home/agent/.config/gemini:ro`
/// - per-run workdir → `/workspace:rw` (input.json mounted RO; output.json
///   expected back here)
///
/// Network: `--network=none` by default.
/// Resource limits: `--memory=4g --cpus=2` defaults; configurable later.
/// Platform: auto-detected (`linux/arm64` on Mac, `linux/amd64` elsewhere) via
/// `std::env::consts::ARCH`.
///
/// Returns the parsed `AgentRun` from `/workspace/output.json` and a
/// `sandbox_ref` (container id) for audit.
pub async fn run_in_container(_spec: &AgentSpec, _input: &AgentInput) -> anyhow::Result<AgentRun> {
    // TODO(Track D):
    //   - workdir = ./.agent-workdirs/{review_id}/{role}
    //   - write {workdir}/input.json from _input
    //   - docker run --rm --network=none --memory=4g --cpus=2
    //       -v ${HOME}/.claude:/home/agent/.claude:ro
    //       -v ${HOME}/.codex:/home/agent/.codex:ro
    //       -v ${HOME}/.config/gemini:/home/agent/.config/gemini:ro
    //       -v {workdir}:/workspace:rw
    //       --platform linux/${arch}
    //       grokrxiv/agent:latest
    //       grokrxiv-agent-runner --spec /workspace/spec.json --input /workspace/input.json
    //   - read {workdir}/output.json, deserialise into AgentRun
    //   - populate sandbox_ref with the container id
    anyhow::bail!("sandbox::run_in_container is not yet implemented (Track D)")
}
