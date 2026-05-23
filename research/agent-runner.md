# GrokRxiv Agent Runner Architecture

> Implementation plan for turning GrokRxiv's current six typed LLM task nodes
> into real code-owned agent workers, while keeping the Rust DAG supervisor as
> the deterministic control plane.

## 1. Problem Statement

GrokRxiv currently uses the word "agent" for the six review roles:

- `summary`
- `technical_correctness`
- `novelty`
- `reproducibility`
- `citation`
- `meta_reviewer`

That naming is slightly ahead of the implementation. Today, each role is mostly
a typed LLM task node: the Rust supervisor builds a prompt, selects a provider
and model, calls `LLMProvider.complete(...)`, parses the JSON, verifies it,
caches it, persists it, and moves the DAG forward.

That is acceptable for review-only inference, but it is not a full agent
abstraction. A full agent is not "an API call." The API call is just the
inference step inside an agent runtime.

A real agent has:

- a bounded objective
- state and inputs
- tools or execution privileges
- a loop or policy for retries/fixes
- typed output contracts
- verifier gates
- an execution backend
- side-effect boundaries

The key correction is:

```text
LLM API       = inference transport
LLM task node = prompt + schema + one inference call
Agent         = code-owned worker with tools, loop, verifier, cache, and result
DAG supervisor = deterministic scheduler and side-effect owner
Sandbox       = isolated environment where risky/file/tool work executes
```

## 2. Target Architecture

Keep the Rust supervisor. Do not move the DAG into Claude, Codex, Gemini, or a
cloud agent product. The supervisor remains the durable, auditable control
plane for review lifecycle, concurrency, persistence, moderation, rendering,
and publishing.

Add two layers under the supervisor:

```rust
trait ReviewAgent {
    async fn run(&self, input: AgentInput) -> anyhow::Result<AgentRun>;
}

trait AgentRunner {
    async fn run(&self, spec: AgentSpec, input: AgentInput) -> anyhow::Result<AgentRun>;
}
```

### `ReviewAgent`

`ReviewAgent` is the role-level worker. There are six concrete role workers:

```text
SummaryAgent
TechnicalCorrectnessAgent
NoveltyAgent
ReproducibilityAgent
CitationAgent
MetaReviewerAgent
```

Each role owns:

- objective and role slug
- prompt construction
- model/provider route
- schema selection
- cache key construction
- cache read/write policy
- inference or spawned-agent execution through `AgentRunner`
- parse/repair retry
- verifier ladder
- token/latency accounting
- structured `AgentRun` result

### `AgentRunner`

`AgentRunner` is the execution backend. It answers: "How does this role do its
work?"

Runners should be swappable per role and per deployment:

```text
ApiInferenceRunner
ClaudeCliRunner
CodexCliRunner
GeminiCliRunner
LocalContainerRunner
E2BRunner
VercelOpenAgentsRunner
```

The existing `LLMProvider` path becomes the simplest runner:

```text
ReviewDagSupervisor
  -> TechnicalCorrectnessAgent
     -> ApiInferenceRunner
        -> LLMProvider.complete(...)
```

For more capable agents:

```text
ReviewDagSupervisor
  -> RevisionAgent
     -> LocalContainerRunner
        -> claude/codex/gemini process with tools in an isolated workdir
```

## 3. Runner Backends

### 3.1 `ApiInferenceRunner`

Use this for pure review-only work where the role only needs to read a prepared
input artifact and return a JSON artifact.

This is the current GrokRxiv path:

```text
system prompt + user prompt + JSON schema + model settings
  -> provider API / local OpenAI-compatible endpoint
  -> JSON text
  -> parse + verifier + cache + persist
```

Use it for:

- `summary`
- `citation`
- `novelty`
- `reproducibility`
- `technical_correctness`
- `meta_reviewer`

unless a role needs to run tools or modify files.

This runner is enough for typed review-only agents because the "work" is
reasoning over a prepared paper extract. It is not enough for revision agents
that need to inspect source trees, run compilers, or propose patches.

### 3.2 `ClaudeCliRunner`

Spawn Claude Code locally as a child process for tool-using agents.

Representative command shape:

```bash
claude -p "$PROMPT" \
  --model opus \
  --permission-mode plan \
  --tools "Read,Grep,Glob,Bash" \
  --add-dir "$WORKDIR" \
  --output-format json
```

Use Claude CLI when:

- the role needs strong long-context code/paper reasoning
- the agent should use file/search/shell tools
- local subscription economics are preferred over API spend
- the workdir is trusted or externally sandboxed

Do not run Claude CLI with broad write permissions against the main repo for
production workloads. Put each run in a dedicated worktree, container, or cloud
sandbox first.

### 3.3 `CodexCliRunner`

Spawn Codex non-interactively for code review, formatting review, patch
proposal, or local OpenAI-backed agent work.

Representative command shape:

```bash
codex exec -C "$WORKDIR" \
  -s workspace-write \
  -a never \
  --json \
  --output-schema "$SCHEMA_PATH" \
  "$PROMPT"
```

For local OSS routing:

```bash
codex exec --oss --local-provider ollama \
  -C "$WORKDIR" \
  "$PROMPT"
```

Use Codex CLI when:

- the role should inspect or patch source artifacts
- structured final output is required
- OpenAI/Codex reasoning is preferred
- local or OSS routing is useful for cost control

### 3.4 `GeminiCliRunner`

Spawn Gemini CLI for independent peer review or a second-opinion reviewer.

Representative command shape:

```bash
gemini -p "$PROMPT" \
  --model "$RESEARCH_GEMINI_MODEL" \
  --approval-mode plan \
  --sandbox
```

Gemini also supports local Gemma routing through `gemini gemma setup/start`.

Use Gemini CLI when:

- the role is a peer-review or critique pass
- provider diversity is more important than tool depth
- a cheap independent reviewer improves confidence

### 3.5 `LocalContainerRunner`

Run each agent in a local isolated workdir or container.

Minimum viable local isolation:

```text
one git worktree per agent
one temp directory per run
only copied input artifacts
no production secrets
explicit output artifact directory
```

Better local isolation:

```text
one Docker/Podman container per agent
read-only mounted inputs
writable scratch/output volume
network disabled unless required
only role-specific environment variables
container deleted after capture
```

Use local containers for:

- LaTeX compile/test loops
- Haskell/Lean verification
- patch proposal against paper source
- generated-code execution
- any future `review_and_revise` role

### 3.6 `E2BRunner`

E2B provides isolated Linux sandboxes created on demand for agents. The
GrokRxiv control plane would run outside E2B and use the E2B SDK to upload
inputs, run commands, fetch outputs, and destroy or archive the sandbox.

Use E2B when:

- the workload needs stronger isolation than local containers
- cloud execution is acceptable
- we need Linux parity for compilers/build tools
- the agent must run untrusted generated code

E2B should be an execution backend, not the GrokRxiv control plane.

### 3.7 `VercelOpenAgentsRunner`

Vercel Open Agents uses a three-layer model:

```text
Web -> Agent workflow -> Sandbox VM
```

The important design point is that the agent workflow runs outside the sandbox.
The sandbox is the filesystem/shell/git/dev-server execution environment.

That separation matches the architecture GrokRxiv should use:

```text
Rust supervisor / workflow code
  -> agent runner
     -> sandbox VM for risky execution
```

Use Vercel Open Agents when:

- we want durable cloud agent runs
- we want sandbox hibernation/resume
- GitHub branch/PR workflow matters
- web UI and streaming run visibility matter

Do not make Vercel Open Agents the first dependency for review-only GrokRxiv.
Add the runner interface first; then make Vercel one possible backend.

## 4. Data Flow

### 4.1 Review-only flow

```text
paper extract
  -> ReviewDagSupervisor
  -> SpecialistAgent.run(input)
  -> cache lookup
  -> ApiInferenceRunner
  -> parse strict JSON
  -> verifier ladder
  -> cache write if pass
  -> AgentRun
  -> review_agents row
```

After all five specialists pass:

```text
specialist outputs
  -> MetaReviewerAgent.run(meta_input)
  -> same cache/runner/parse/verify flow
  -> meta_review
```

### 4.2 Tool-using flow

```text
paper source bundle
  -> ReviewDagSupervisor
  -> RevisionAgent.run(input)
  -> create isolated workdir/sandbox
  -> copy inputs
  -> spawn claude/codex/gemini or API-driven tool loop
  -> run compile/tests/checks
  -> collect structured artifact
  -> verifier validates artifact + patch
  -> persist proposed patch
  -> human/policy gate applies patch
```

The agent may propose work. Deterministic code applies side effects.

## 5. Public Interfaces

### `AgentSpec`

Minimum fields:

```rust
struct AgentSpec {
    role: AgentRole,
    runner: AgentRunnerKind,
    provider: Option<String>,
    model: String,
    prompt_template: String,
    schema: serde_json::Value,
    tool_policy: ToolPolicy,
    sandbox_policy: SandboxPolicy,
    max_retries: u8,
}
```

### `AgentInput`

Minimum fields:

```rust
struct AgentInput {
    paper_id: Uuid,
    review_id: Uuid,
    role: AgentRole,
    content_hash_material: serde_json::Value,
    artifact: serde_json::Value,
    source_bundle_path: Option<String>,
}
```

### `AgentRun`

Minimum fields:

```rust
struct AgentRun {
    role: AgentRole,
    runner: AgentRunnerKind,
    model: String,
    output: serde_json::Value,
    verifier_status: VerifierStatus,
    verifier_notes: serde_json::Value,
    tokens_in: Option<i32>,
    tokens_out: Option<i32>,
    latency_ms: i32,
    cache_hit: bool,
    sandbox_ref: Option<String>,
}
```

### Role config

Current `agents/*.yaml` should grow a runner field later:

```yaml
id: technical_correctness
provider: claude
model: claude-opus-4-7
runner: api_inference
prompt_template: prompts/technical_correctness.md
schema: schemas/technical_correctness.schema.json
```

For a future tool-using role:

```yaml
id: revision_agent
runner: local_container
model: claude-opus-4-7
tools:
  - read
  - grep
  - bash
  - edit
sandbox:
  network: false
  writable_paths:
    - /workspace/out
```

## 6. Side-Effect Boundaries

The supervisor owns:

- DAG topology
- concurrency
- DB transactions
- cache tables
- moderation queue
- render/publish lifecycle
- final patch application

Agents own:

- role reasoning
- local inspection inside their sandbox/workdir
- proposed structured artifacts
- proposed patches
- role-local verification loops

Agents must not directly:

- write production DB rows
- publish PRs
- mutate the canonical repo
- approve moderation
- bypass verifier gates
- access global secrets

The runner receives only the inputs and credentials needed for that role.

## 7. Implementation Phases

### Phase 1: Rename the mental model, no behavior change

Introduce `ReviewAgent` wrappers around the existing six role paths. Each
wrapper still calls the current `LLMProvider` path.

Acceptance:

- six roles still run in the same DAG order
- output rows are unchanged
- cache behavior is unchanged
- all existing M1 pipeline checks pass

### Phase 2: Add `AgentRunner`

Add `ApiInferenceRunner` as the default and route all six existing agents
through it.

Acceptance:

- role config can select `runner: api_inference`
- missing runner defaults to `api_inference`
- no external CLI is required
- unit tests cover runner selection

### Phase 3: Add local CLI runners

Implement `ClaudeCliRunner`, `CodexCliRunner`, and `GeminiCliRunner` as
experimental backends.

Acceptance:

- command construction is unit-tested
- each runner supports a timeout
- each runner captures stdout/stderr/exit code
- each runner can write final JSON to a configured output path
- no runner is enabled by default in production config

### Phase 4: Add isolated execution

Add `LocalContainerRunner` or worktree-based isolation before enabling any
write-capable local CLI agent.

Acceptance:

- each run gets a unique workdir
- inputs are copied or mounted read-only
- outputs are collected from a known directory
- cleanup behavior is explicit
- network/secrets are opt-in per role

### Phase 5: Add cloud sandbox backend

Choose one cloud backend after the runner abstraction exists:

- E2B for generic isolated Linux execution
- Vercel Open Agents for durable workflow plus sandbox VM plus GitHub/PR UX

Acceptance:

- cloud runner is behind config
- credentials are role-scoped
- failed sandbox creation falls back only when configured
- sandbox id/ref is persisted for audit

### Phase 6: Add `review_and_revise`

Create revision-capable agents that emit structured patch artifacts instead of
directly modifying canonical source.

Acceptance:

- patch artifact validates against schema
- compile/test evidence is attached
- human/policy gate is required before apply
- applied patches are traceable to an agent run

## 8. Testing Strategy

Unit tests:

- `AgentRunnerKind` parsing/defaulting
- command construction for Claude/Codex/Gemini
- cache key includes runner/model/prompt/schema/verifier version
- `AgentRun` serializes to the existing provenance shape

Integration tests:

- fake runner proves specialist fan-out then meta execution
- fake cache proves all-five-specialist hits produce meta cache hit
- fake failing runner records verifier/runner failure without publishing
- local temp-workdir runner proves input/output collection

Smoke tests:

- existing `tests/m1-pipeline.sh` remains the API-inference smoke
- optional `tests/agent-runner-local.sh` exercises one local CLI runner when
  credentials and binaries exist
- optional `tests/agent-runner-sandbox.sh` exercises container or cloud sandbox
  when configured

## 9. Recommendation

Use this rule:

```text
If the role only reads a prepared artifact and returns JSON, use ApiInferenceRunner.
If the role needs files, shell, compilers, patches, or generated artifacts, use an agent runner.
If the role can mutate or execute untrusted code, put it in a sandbox.
```

For current GrokRxiv review-only:

```text
Keep the six roles on ApiInferenceRunner.
Rename/structure them as ReviewAgents internally.
Do not pay the complexity cost of cloud agents yet.
```

For future revision mode:

```text
Use spawned local/cloud agents.
Give each agent an isolated workdir or sandbox.
Require structured patch artifacts.
Let deterministic code validate and apply side effects.
```

## 10. References

- Vercel Open Agents template: `https://vercel.com/templates/template/open-agents`
- E2B documentation: `https://www.e2b.dev/docs`
- Existing GrokRxiv provider boundary: `crates/llm-adapter/src/lib.rs`
- Existing GrokRxiv DAG: `crates/orchestrator/src/supervisor.rs`
- Existing role configs: `agents/*.yaml`

---

*End of document.*
