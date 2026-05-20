# YAML DAG agent runtime major-revision plan

**Status:** Deferred major revision.
**Current priority:** Keep the behavior-preserving supervisor decomposition and structured `revision_targets` stable first.
**Goal:** Make it easy to add review agents to the research-paper review DAG without changing Rust enums, DB constraints, render schemas, web components, verifier registries, and prompt wiring in separate edits.

## Why this is not the Tier 1 change

Adding a new first-class review agent is still moderately hard because the current system has fixed role assumptions in several layers:

- `AgentRole` is a closed Rust enum.
- `review_agents.role` and `review_cache.role` are constrained to today's role set.
- `agents/*.yaml` loading expects the known role files.
- Meta synthesis, renderers, web types, and reviewer detail views enumerate the six roles.
- Verifier ladders and schema lookup are keyed by `AgentRole`.
- The OpenAI/Codex CLI path does not yet pass per-role `model` and `reasoning_effort` through reliably enough for a role-specific "gpt-5.5 medium" choice to be enforceable.

The near-term solution is deterministic `revision_targets` generated from existing specialist outputs. This plan is the larger branch that removes those fixed-role constraints.

## Target state

The canonical review flow is declared as YAML:

```yaml
id: research_paper_review
version: 1
nodes:
  - id: prepare_review
    kind: ingest
  - id: technical_correctness
    kind: agent
    role: technical_correctness
    model: gpt-5.5
    reasoning_effort: medium
    depends_on: [prepare_review]
  - id: revision_target_checker
    kind: agent
    role: revision_target_checker
    model: gpt-5.5
    reasoning_effort: medium
    depends_on: [meta_reviewer]
  - id: moderation_ready
    kind: terminal
    depends_on: [render]
```

Each role is a runtime `RoleSpec`, not an enum variant:

```yaml
id: revision_target_checker
display_name: Revision target checker
schema: schemas/revision_targets.schema.json
prompt_template: prompts/revision_target_checker.md
fact_plugins:
  - paper_locator_index
  - git_source_index
verifier_ladder:
  - json_schema
  - target_locator_resolves
runner:
  kind: cli
  provider: openai
  model: gpt-5.5
  reasoning_effort: medium
  timeout_secs: 180
  max_retries: 1
budget:
  max_input_tokens: 60000
  max_output_tokens: 6000
```

The executor reads the YAML DAG, validates it, runs dependency layers deterministically, and persists all node outputs under string role/node ids.

## Multi-team implementation

### Team A: Runtime role registry

Owns `RoleSpec`, role ids, compatibility, and schema lookup.

- Introduce a string-keyed `RoleId` newtype with slug validation.
- Keep `AgentRole` as a compatibility adapter for the six existing roles during migration.
- Load role specs from `agents/*.yaml` without a hard-coded file list.
- Move prompt template, schema path, verifier ladder name, runner defaults, model, and reasoning effort into role config.
- Add tests for unknown roles, duplicate role ids, invalid slugs, missing schemas, missing prompts, and legacy-role compatibility.

### Team B: DB role/storage migration

Owns persistence and cache compatibility.

- Replace role CHECK constraints with validated text role ids.
- Add `node_id`, `dag_id`, and `dag_version` where needed for executor traceability.
- Add cache dimensions for prompt hash, schema hash, role spec hash, model, and reasoning effort.
- Migrate existing six-role rows without rewriting outputs.
- Keep old reads working for existing reviews.

### Team C: YAML DAG loader and executor

Owns topology.

- Add `dags/research_paper_review.yaml` as the canonical topology.
- Validate unique node ids, supported node kinds, dependencies, acyclic graph, known role ids, terminal node count, quorum constraints, and no disconnected required nodes.
- Execute by deterministic dependency layers.
- Preserve current hard-failure behavior: an unrecoverable DAG node failure withdraws or gates the review according to the existing gate semantics.
- Add executor tests for ordering, skipped dependents, failure propagation, quorum, and stable output lookup.

### Team D: Node handler and artifact abstraction

Owns per-node behavior.

- Wrap today's prepare, specialist, verifier, quorum, meta-review, render, and moderation-ready steps as node handlers.
- Introduce a minimal `ReviewArtifact` boundary only where handlers need it; do not generalize beyond paper review until a second real artifact arrives.
- Move deterministic fact overlays into configured fact plugins.
- Keep `PaperExtract` as the default paper artifact in this branch.
- Add tests proving arXiv, local PDF/TeX, and git-repo paper sources enter the same DAG path.

### Team E: Dynamic render, web, and GitHub feedback

Owns user-facing output.

- Render arbitrary agent outputs by role spec display metadata.
- Keep specialized rich views for known roles, but fall back to schema-aware JSON for new roles.
- Show `revision_targets` in web review pages, GitHub gate feedback, Markdown, HTML, and LaTeX artifacts.
- Add role display ordering from DAG topology instead of a hard-coded role list.
- Add tests for a synthetic seventh agent appearing in render/web output without TypeScript or Rust enum edits.

### Team F: Model and reasoning selection plumbing

Owns runner config enforcement.

- Thread `model` and `reasoning_effort` from RoleSpec through `AgentSpec`, CLI runners, API runners, cache records, and review metadata.
- Make Codex/OpenAI CLI invocations pass the selected model and reasoning effort explicitly.
- Add `.env.example` entries for any new runner/model knobs introduced by this branch, and keep `.env` local-only values out of docs.
- Add tests that a role configured for `gpt-5.5` + `medium` records and invokes those exact settings.
- Keep deterministic revision-target extraction as the default until this plumbing is verified.

### Team G: Synthesis and acceptance

Owns integration and final branch quality.

- Run all unit and integration suites after Teams A-F land.
- Run direct CLI smoke with `.env` loaded.
- Fix issues on the branch as they appear; do not defer broken acceptance to follow-up work.
- Capture the final smoke JSON only after the integrated branch is green.

## Test plan

- Unit tests:
  - Role registry loads known and synthetic roles.
  - Invalid role ids, missing schemas, missing prompts, and duplicate ids fail validation.
  - YAML DAG validates uniqueness, dependencies, acyclicity, terminal node count, and quorum rules.
  - Executor runs dependency layers deterministically.
  - Executor skips dependents after node failure and records the failure.
  - Model and reasoning effort flow into runner invocations and cache metadata.

- Integration tests:
  - Existing six-role review produces the same persisted rows and gate behavior.
  - Synthetic seventh agent persists, renders, and appears in web/API output.
  - arXiv, local PDF/TeX, and git-repo paper sources all use the same DAG executor.
  - Correction re-review preserves and reconciles `revision_targets`.

## Acceptance commands

Run from the repo root:

```bash
cargo test --workspace --release
```

Then run the direct CLI smoke with the repo `.env` loaded:

```bash
set -a && source .env && set +a && PATH="$PWD/target/release:$PATH" grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307
```

Equivalent direct CLI smokes are required for:

- a local PDF/TeX fixture;
- a temporary git repo containing a `.tex` paper and `--paper-path`;
- a synthetic seventh-agent DAG that writes one extra agent row and renders without special-case code.

Success means all expected agent rows have `verifier_status: pass`; the review recommendation may still be `major_revision` or `reject` and the publication gate may still fail.

## Explicit deferrals

- Broad multi-tenant/RLS design.
- OpenReview or non-GitHub publish adapters.
- General non-paper artifact review.
- Streaming progress UI.
- Runtime reviewer profiles and ensemble escalation, except where needed to prove the RoleSpec boundary.
