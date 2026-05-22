# DAG Abstraction Gap Analysis

Scope: comprehensive audit of the DAG-Type Agent Abstraction landing on branch
`feature/extract-tool-agent-orchestration` against two plans:

- `~/.claude/plans/piped-bubbling-brook.md` — Part A (27 remediation items)
  plus Part B (B1–B10 foundational blockers + R1/R2/R3 refactors).
- The Merged DAG-Type Agent Abstraction Plan (review-time inline plan).

Reference docs that frame remaining work:

- `docs/ingest-extraction-gap-analysis.md`
- `docs/grokrxiv-env-reference-applied.md`

All findings are anchored to file:line in the current branch state.

## Executive summary

| Area | Verdict | Headline |
|---|---|---|
| DAG abstraction | Partial | Manifests on disk + loaded; runtime is still imperative; many node kinds have no executor. |
| Tool/agent abstraction | Blocker on add-role-without-Rust | Runtime maps still `HashMap<AgentRole, _>`; declarative YAML fields parse/validate but are not yet the execution source. |
| Ingest + extraction | Mostly good with declarative gaps | `paper-extract.yaml` loaded but descriptive; arXiv hardcoded; agent-enabled mode untested. |
| Review path (Tiers 1–4) | 8 pass / 5 partial / 14 fail | Tier 1 hazards 1, 6, 7 still live. |
| Publish/web/github/output | Mostly pass | Reconcile loop, idempotency, webhook ACK, generic-role web rendering all landed; 2 env vars doc-only; revision-apply loop is sketch. |

The branch has executed Part A's safety items well (idempotent publish,
fail-loud `GITHUB_TOKEN`, async webhook, CLI-binary check at boot,
runner-on-cache) and meaningfully advanced the declarative surface
(`DagManifest`, paper-extract enrichment, web generic-role types). It has not
executed the runtime side of the Merged DAG plan (R1/R3 in Part B language):
roles are still gated on the `AgentRole` enum at the only place that matters,
the supervisor.

## Implementation update — 2026-05-22

This pass closed the first runtime-contract and remediation blockers without
attempting the full executor migration:

- `DagNode.kind` is now a closed `DagNodeKind` enum. Unknown manifest node
  kinds fail during YAML load.
- DAG nodes now support `feeds_meta` and gate policy metadata
  (`gate.min_usable`, `gate.sources`), and `paper-review.yaml` declares the
  specialist quorum from manifest data.
- Tool nodes must point at a registered top-level tool definition.
- `DagExecutionMode` and `DagRoleKey` provide the shared runtime vocabulary
  for `<dag_type>.<role_id>` role keys and `one_shot`/`tool_loop` execution.
- Agent YAML parsing now keeps the declarative runtime fields that were
  previously ignored: `kind`, `execution_mode`, prompt/schema paths, verifier
  names, tool names, max iteration/cost limits, and escalation. Startup
  validation now fails on missing declared prompt/schema files and invalid
  tool-loop knobs.
- `ReviewStatus::SystemFailed` is wired through Rust schemas, DB status
  decoding, web status types/badge labels, and a new `reviews.status` CHECK
  migration. Fatal review DAG/runtime failures now transition to
  `system_failed` instead of overloading `withdrawn`.
- `RevisionTarget.source_role` is now an optional string and the meta-review
  JSON schema accepts custom role keys.

Still open after this pass: the generic DAG executor, string-keyed execution
path, prompt-template rendering, verifier construction from YAML names,
native/CLI tool handler registry, scaffold-agent/scaffold-tool CLI, and the
paper-extract/paper-review migrations onto the generic executor.

## 1. DAG abstraction

### Present

- `crates/dag-runtime/src/lib.rs` — `DagTypeId`, `RoleId`, `DagRoleKey`,
  `AgentKind`, `DagNodeKind`, and `DagExecutionMode` enums; manifest
  validation (duplicate IDs, missing role refs, missing tool refs, missing
  nodes, cycles, `KindNotAccepted`, unknown node kinds, missing tool-node
  refs), topological layering, `compatible_dag_ids`.
- `DagRunReport` / `DagNodeReport` / `DagNodeStatus` (lib.rs:281–317) added.
- All four manifests on disk; `paper-extract.yaml` has been enriched into a
  real tool DAG (tools registry, `ingest_source` / `tool` / `artifact` /
  `dag_call` / `agent` node kinds, declarative inputs/outputs).
- `paper-extract.yaml` *is* loaded at runtime in
  `crates/orchestrator/src/ingest_pipeline.rs:~2620`.
- `paper-review.yaml` is loaded at `crates/orchestrator/src/state.rs:444`
  and `crates/orchestrator/src/supervisor/review_flow.rs:303-304`.

### Gaps

- `feeds_meta` exists in manifests, but the meta-reviewer input is still
  constructed from the hardcoded five-role list at
  `crates/orchestrator/src/supervisor/review_flow.rs:315-323` (the
  `match role_id { "summary" => Summary, ... }` block). The merged plan's
  commitment — "Review meta input is built from DAG nodes marked
  `feeds_meta=true`, not from a hardcoded five-role list" — remains
  unfulfilled.
- Gate policy is declared in `paper-review.yaml`, but the runtime still uses a
  Rust constant. The quorum minimum comes from `MIN_SPECIALIST_QUORUM`
  (`crates/orchestrator/src/supervisor.rs:77`)
  aliased to `DEFAULT_MIN_SPECIALIST_QUORUM = 3`
  (`crates/orchestrator/src/review_dag.rs:5`). Used at
  `crates/orchestrator/src/supervisor/review_flow.rs:341`.
- Orphan node kinds (declared, no executor):
  - `prepare_inputs` — no executor branch.
  - `ingest_source acquire_source` — bypassed; ingest is imperative.
  - `artifact materialize_review_input` — no executor; persistence is direct
    `PaperArtifacts::persist`.
  - `dag_call dag_type: paper-review` — no executor; chaining is imperative
    in `crates/orchestrator/src/supervisor.rs`.
  - `tool` — each id resolved by hardcoded string match in
    `crates/orchestrator/src/ingest_pipeline.rs`, not via registry.
- `DagRunReport` is unused. Defined but never emitted anywhere. Observability
  promise hanging.
- Startup validation now checks declared review-agent prompt/schema paths from
  YAML. `DagManifest::from_path` remains pure manifest validation and does not
  inspect external files itself.
- `crates/orchestrator/src/review_dag.rs` (341 LoC) is still alive.
  `crates/orchestrator/src/lib.rs:22` declares the module;
  `crates/orchestrator/src/supervisor.rs:77` and
  `crates/orchestrator/src/db.rs:211` re-export its
  `DEFAULT_MIN_SPECIALIST_QUORUM` constant. Dead-codishly held alive by the
  gate constant.

## 2. Tool + agent abstraction

### Present

- `AgentRunner` trait with 4 backends (`api` / `cli` / `cloud` /
  `local_inference`) — correct per plan, no spurious `ToolLoop` variant
  added.
- `complete_with_tools` implemented on
  `crates/orchestrator/src/agents/runners/api.rs:206` and
  `crates/orchestrator/src/agents/runners/cli.rs:308`; tool-loop dispatch at
  `crates/orchestrator/src/agents/extraction/loop.rs:64` and
  `crates/orchestrator/src/agents/runners/api.rs:564` goes through the trait.
- `crates/orchestrator/src/agents/types.rs:289-310` —
  `AgentRun::from_cache(..., runner: AgentRunnerKind)` now carries the real
  runner (Tier 2 item 8).
- `apps/web/lib/types.ts:59` — `AgentRole = KnownAgentRole | string` (generic
  strings on the web side, with fallback labels).

### Blockers

- Zero-migration role addition still fails closed.
  `crates/orchestrator/src/supervisor/review_flow.rs:325-328` still bails:

  ```rust
  anyhow::bail!(
      "DAG role `{other}` is configured as executable, but the legacy
       runner adapter still requires a built-in AgentRole; add a runner/
       schema adapter for this role before enabling it"
  );
  ```

  This is the load-bearing failure mode against the merged plan's named
  acceptance criterion.
- Runtime maps still enum-keyed.
  `crates/orchestrator/src/state.rs:26` —
  `HashMap<AgentRole, Arc<ConfiguredAgent>>`,
  `state.rs:31` — `HashMap<AgentRole, AgentSchema>`,
  `state.rs:34` — `HashMap<AgentRole, VerifierLadder>`. The plan softened to
  "keep compatibility constants only where useful for tests or display, but
  runtime dispatch must not require enum widening" — these three maps are
  runtime dispatch.
- `AgentRouting` now parses the full declared YAML surface, including
  `kind`, `execution_mode`, prompt/schema paths, verifier/tool names,
  iteration/cost caps, retries, timeout, and escalation. Most of those fields
  are validated but not yet executable runtime inputs.
- Schemas still embedded via `include_str!`. Eight `include_str!` calls in
  `crates/orchestrator/src/state.rs:518-560`
  (`build_agent_schemas_and_verifiers`) keyed by `AgentRole::{Summary,
  TechnicalCorrectness, ...}`. The per-role YAML's
  `input_schema:` / `output_schema:` paths are dead config.
- Verifier ladder hardcoded. `crates/verifier/src/lib.rs:74-75`:

  ```rust
  pub fn standard_for_role(role: AgentRole, schema: Option<Value>) -> Self {
      Self::with_citation(schema, matches!(role, AgentRole::Citation))
  ```

  No code path reads the YAML's `verifiers: [...]` list.
- Prompt rendering still match-role.
  `crates/orchestrator/src/supervisor/prompts.rs:3, 22, 98, 316, 329, 337,
  343` — seven match blocks. `prompt_template:` paths in agent YAML are
  unread.
- `execution_mode` now parses and validates, but does not yet route one-shot
  vs tool-loop execution through the review runtime.
- `source_role` is now an optional string in Rust and the meta-review JSON
  schema, so custom DAG roles can be represented in revision targets.
- `--model-for` / `--runner-for` accept bare-role only.
  `crates/orchestrator/src/runtime_config.rs:544-552`'s `parse_role_model`
  calls `parse_role(role_s)` which is `AgentRole::from_str`. No support for
  `<dag-id>.<role-id>=<value>` form.
- `grokrxiv dag scaffold-agent` missing.
  `crates/orchestrator/src/cli.rs::DagCommand` (lines 462–505) has only
  `Validate / AddAgent / RemoveAgent`. The plan named `scaffold-agent
  --intent <text>` as the LLM-facing authoring affordance.
- Cloud + LocalInference runners are still speculative. ~1100 LoC in
  `crates/orchestrator/src/agents/runners/{cloud,local_inference}.rs` with
  no `complete_with_tools` impl and no default-path invocation (Part A item
  20).

## 3. Ingest + extraction

### Closed gaps (per `docs/ingest-extraction-gap-analysis.md`)

- `source_manifest.json` records all entries with sha256 + `kind`
  (string-typed; `crates/orchestrator/src/ingest_pipeline.rs:1960-1983,
  2603-2628`). Figures detected and routed.
- `body.md` preserves Pandoc citation markers; `references.json` carries
  citation contexts and unmatched-key tracking.
- BibTeX keys with DOI/colon punctuation now resolve (e.g., `Barra:2012aa`).
- `extraction_report.json` validates against data-repo schema.
- `paper-extract.yaml` exposes the tool DAG declaratively.
- Citation extraction rewritten as a tool-loop agent
  (`crates/orchestrator/src/agents/extraction/citations/mod.rs:1-203`,
  `run_tool_loop` at line 119, `max_iters=80`, `max_cost_usd=$0.05`).

### Remaining gaps

- `validation: null` in default `pandoc_enabled` path.
  `crates/orchestrator/src/ingest_pipeline.rs:2081` emits `Value::Null`.
  Schema declares `validation` as an object with status enum; only
  `agent_enabled` (`ingest_pipeline.rs:966-970`) populates it. The
  agent-enabled path is not exercised by any smoke or CI test.
- LaTeXML is opt-in with silent fallback. `crates/ingest/src/tex.rs:~150`
  (`latexml_enabled()`). When disabled, code falls back to Pandoc Markdown
  with no warn-banner in `extraction_report.json` flagging that the semantic
  AST is absent.
- Figure binaries inventoried but not materialized in dry-run.
  `crates/orchestrator/src/ingest_pipeline.rs:1995-2003` collects figures;
  persistence only fires when Tier-2 storage is enabled
  (`GROKRXIV_DRY_RUN_STORAGE!=1`). This is documented and intentional but it
  means a development copy of `papers/<arxiv_id>/figures/` will be empty.
- Theorem extraction is a markdown/AST scanner with LLM-agent fallback.
  `crates/orchestrator/src/agents/extraction/theorems/mod.rs:77-87` is a
  tool-using agent (deterministic-fallback at
  `crates/orchestrator/src/ingest_pipeline.rs:~2200`). Both scan post-Pandoc
  markdown — if Pandoc fails to convert `\begin{theorem}` blocks (custom
  theorem environments), they are invisible.
- `paper-extract.yaml` is descriptive, not authoritative. Manifest is loaded
  at `crates/orchestrator/src/ingest_pipeline.rs:~2620` but the orchestrator
  does NOT walk it. Stages 4-7 (`ingest_pipeline.rs:966-1050`) call
  `run_agent_when()` / `run_agent_safe()` in hardcoded order. `dag_call`,
  `ingest_source`, `artifact`, `tool` node kinds have no executor branches.
- arXiv coupling. `crates/orchestrator/src/ingest_pipeline.rs:942` always
  calls `grokrxiv_ingest::pipeline::ingest_staged(arxiv_id)`.
  `crates/ingest/src/source.rs` defines `SourceKind::{Arxiv, LocalFile,
  GitRepo}` but only `Arxiv` flows through the orchestrator's main pipeline.
  Part B B4 still open.
- Extraction agents wired directly, not via manifest. The agent YAML
  `config:` paths in `paper-extract.yaml` are documentation; agents are
  constructed by hand in Rust.

## 4. Review path — Tier 1–4 status

### Tier 1 (production hazards) — PASS 3, FAIL 3, PARTIAL 1

| # | Item | Status | Evidence |
|---|---|---|---|
| 1 | `RenderAgent::role()` placeholder | FAIL | `crates/orchestrator/src/agents/review_agents.rs:46-74`; no `AgentRole::Render` variant; collision risk persists. |
| 2 | `/internal/v1/*` stubs | PARTIAL | `crates/orchestrator/src/routes/internal.rs:185-208` — approve/reject/render/apply-revisions/verify return 501 (fail-closed); `/internal/v1/review` IS wired to supervisor. |
| 3 | `run_publish` silent SIMULATED | PASS | `crates/orchestrator/src/supervisor/publish.rs:299-300` bails: "GITHUB_TOKEN not set; required to open review PR". |
| 4 | `publish_after_approval` idempotency | PASS | `crates/orchestrator/src/supervisor.rs:173-196` — `publish_inflight: Arc<Mutex<HashSet<Uuid>>>`. |
| 5 | `build_agent_registry` permissive fallback | PASS | `crates/orchestrator/src/state.rs:320-336` — `validate_role_configs()` bails on missing/malformed YAML. |
| 6 | Reconcile loop for `PrOpen → Published` | PASS (verify spawn) | `crates/orchestrator/src/supervisor/publish.rs:8-52` — `spawn_publish_reconcile()` ticks every 300s, calls `list_pr_open_reviews_with_urls`, reconciles via `is_pr_merged`. Spawn site call should be double-checked. |
| 7 | CLI bypasses retry/job-tracking | FAIL | `crates/orchestrator/src/supervisor.rs:223-227` — `run_review_for_paper_blocking()` calls `review_flow::run_one_paper_full()` directly, not via `run_item()`. Skips `mark_running/done/failed` and `MAX_RETRIES`. |

### Tier 2 (observability/scale) — PASS 4, FAIL 4, PARTIAL 1

| # | Item | Status | Evidence |
|---|---|---|---|
| 8 | `AgentRun::from_cache` runner | PASS | `crates/orchestrator/src/agents/types.rs:289-310` carries `runner: AgentRunnerKind`. |
| 9 | `mark_*` swallowed errors | PARTIAL | `crates/orchestrator/src/supervisor/jobs.rs:44-46` propagates with `?`. CLI path (item 7) doesn't call these at all. |
| 10 | Webhook inline DB work | PASS | `crates/orchestrator/src/routes/webhook.rs:139-173` — record→spawn→ACK pattern. |
| 11 | mpsc capacity 128 | PARTIAL | `crates/orchestrator/src/supervisor.rs:90-92` — `GROKRXIV_SUPERVISOR_QUEUE_CAPACITY` env-configurable, default 4096. No per-source semaphore. |
| 12 | Backfill blocks startup | PASS | `crates/orchestrator/src/scheduler.rs:187-203` — backfill in sibling `tokio::spawn()`. |
| 13 | Scheduler `fetch_listing` retry | FAIL | `crates/orchestrator/src/scheduler.rs:258-275` retries within a single tick, no persistent last-completed-date in DB. |
| 14 | PR marker correlation | PASS | `crates/orchestrator/src/routes/webhook.rs:117-130` — marker now read from PR metadata first, body as fallback. |
| 15 | Supervisor-level timeout | FAIL | `spec.timeout_secs` enforced inside runners only; no tokio `timeout()` wrapping `agent.run()` at `crates/orchestrator/src/supervisor/jobs.rs`. PR `72ae2b5` kills CLI subprocess groups on timeout but does not address API/cloud/local-inference. |
| 16 | CLI binary check at boot | PASS | `crates/orchestrator/src/state.rs:338-362` — `validate_required_cli_runners()` runs in `AppState::from_config`. |

### Tier 3 (dead code / parallel registries) — FAIL 3, PARTIAL 2, NOT AUDITED 3

| # | Item | Status | Evidence |
|---|---|---|---|
| 17 | `ReviewAgent` trait ceremony | PARTIAL | PR `e53e064` thinned `review_agents.rs`, but full collapse to `(spec, runner)` not done; `crates/orchestrator/src/agents/traits.rs:24-55` keeps `AgentRunner`. |
| 18 | Dead config (`tool_policy`, `.mode`, `.sandbox`) | PARTIAL | `crates/orchestrator/src/agents/types.rs:191-210` — only `sandbox: SandboxPolicy` survives; `tool_policy` / `mode` gone. `sandbox` still inert. |
| 19 | Parallel role registries | PARTIAL | `crates/orchestrator/src/state.rs:26-34` — single `AgentRegistry` now; `role_routing` legacy removed. But schema/verifier maps duplicate the keying. |
| 20 | Cloud + LocalInference speculative | FAIL | Both files still ~600 LoC each, no default-path invocation. |
| 21 | `max_tokens_for_model` substring switch | Not audited (llm-adapter scope) | Should be addressed when R1 lands. |
| 22 | ApiRunner repeats same prompt on retry | Not audited | Worth a follow-up. |
| 23 | API-allowed guard duplicated | Not audited | Likely still duplicated per Part A. |
| 24 | `AgentSpec` cloned with 50KB schema | PASS | `crates/orchestrator/src/agents/types.rs:205` — `schema: AgentSchema = Arc<Value>`. |

### Tier 4 — PARTIAL 1, FAIL 1, PASS 1

- 25: shutdown signal exists, no graceful JoinSet drain — PARTIAL.
- 26: stateless mode still enqueues blindly — FAIL.
- 27: resolved runner persisted via `insert_review_agent` /
  `crates/orchestrator/src/supervisor/review_flow.rs:774-791` — PASS.

## 5. Publish / web / github / output

| Area | Verdict | Evidence |
|---|---|---|
| Publish idempotency | PASS | `crates/orchestrator/src/supervisor.rs:173-196` HashSet dedupe. |
| Publish fail-loud | PASS | `crates/orchestrator/src/supervisor/publish.rs:299-300` bails on missing token. |
| Reconcile loop | PASS (verify spawn) | `crates/orchestrator/src/supervisor/publish.rs:8-52`. |
| GitHub webhook correlation | PASS | `crates/orchestrator/src/routes/webhook.rs:117-130` — PR metadata first, body fallback. Tier 2 #14 addressed. |
| Webhook ACK pattern | PASS | `crates/orchestrator/src/routes/webhook.rs:28-174` — verify→record→spawn→ACK. |
| Output formats | PASS | `crates/orchestrator/src/supervisor/rendering.rs:35-163` — HTML/MD/TeX/ZIP + per-agent JSON. |
| HTML quality harness | PASS | `crates/orchestrator/src/supervisor/rendering.rs:146-150` — `html_review::review_and_fix_html`. |
| Web generic roles | PASS | `apps/web/lib/types.ts:59` — `AgentRole = KnownAgentRole \| string`; `apps/web/components/agent-accordion.tsx:72-73` fallback to raw string. |
| Public API `/api/v1/reviews/[id]` | PASS | Selects `dag_type, node_id, agent_type` from `review_agents`. |
| `/legal` disclaimer isolated | PASS | No disclaimer in render artifacts. |
| Stripe billing | PASS | `apps/web/app/api/billing/webhook/route.ts:17-32` verifies signature; checkout/portal/webhook all wired. |
| Account workflows | PASS | `apps/web/app/api/admin/users/[userId]/{role,quota,billing}` + `apps/web/app/api/account/reviews` routes wired. |
| Revision-apply loop | PARTIAL | Schema + DB table + UI + `apply_revisions()` exist; no CLI/API path that drives the patch→re-ingest→re-review loop end-to-end. |

### Env-var coverage (against `docs/grokrxiv-env-reference-applied.md`)

- Documented and consumed: `GROKRXIV_PREVIEW_PROVIDER`,
  `GROKRXIV_CITATION_PROMPT_MAX_BIB_ENTRIES`,
  `GROKRXIV_DOCKER_INSTALL_PANDOC`, `GROKRXIV_PUBLIC_REVIEWS_REPO`,
  `GROKRXIV_PRIVATE_REVIEWS_REPO`, `GROKRXIV_EXTRACTION_MODE`,
  `GROKRXIV_SUPERVISOR_QUEUE_CAPACITY`.
- Documented but not yet confirmed via grep (verify before relying on or
  removing):
  - `GROKRXIV_TEX_ENABLE_LATEXML` / `GROKRXIV_TEX_DISABLE_LATEXML` —
    `latexml_enabled()` lives in `crates/ingest/src/tex.rs` and likely reads
    both; confirm.
  - `GROKRXIV_FREE_REVIEW_LIMIT` — documented as default `3` but no
    consumer found in the audit. Either consumed via a wrapper helper or
    doc-only.

## 6. Cross-cutting blockers

Three failures repeat across every audit area. Fixing any one of them
unblocks several others.

1. The `AgentRole`-gated review path.
   `crates/orchestrator/src/supervisor/review_flow.rs:325-329` bails on
   unknown roles. This single guard makes the merged plan's
   zero-migration-role-addition promise impossible and blocks Part B R1 from
   landing incrementally. Every other gap downstream of this (B1, B2, R1)
   clears once this guard is replaced with a string-keyed dispatch.
2. Per-role YAML fields read at boot.
   `crates/orchestrator/src/state.rs:240-250` `AgentRouting` only reads 5
   fields. Wiring `kind, prompt_template, input_schema, output_schema,
   verifiers, execution_mode` into this struct (and then through
   `build_agent_registry`, `build_agent_schemas_and_verifiers`,
   `standard_for_role`) collapses all the hardcoded `include_str!` +
   `match role { ... }` sites simultaneously. About one week of focused
   work.
3. Manifest descriptiveness vs authority. Every loaded manifest gets
   translated back into imperative orchestration:
   - `paper-review.yaml` → `match role_id { "summary" => Summary, ... }`
     (`crates/orchestrator/src/supervisor/review_flow.rs:314`).
   - `paper-extract.yaml` → hardcoded stage calls in
     `crates/orchestrator/src/ingest_pipeline.rs:966-1050`.
   - Node kinds `dag_call`, `ingest_source`, `artifact`, `tool`,
     `prepare_inputs` declared but no executor branches.
   - `DagRunReport` defined but never emitted.
   - Gate policy hardcoded (`DEFAULT_MIN_SPECIALIST_QUORUM`).
   - `feeds_meta` absent.

   This is Part B R3 (real DAG composition). Part A item 19 ("two parallel
   role registries") is the surface symptom; the deeper one is two parallel
   orchestration models (declarative manifest + imperative supervisor).

## 7. Concrete remediation order (10 leaves, root-to-leaf)

The branch is well-set-up for the small-diff team workflow described in
`piped-bubbling-brook.md` Step 2. Suggested leaves:

1. `runtime/string-keyed-registry` — convert `AgentRegistry`,
   `AgentSchemaMap`, `VerifierMap` to `HashMap<RoleId, _>`; expose
   `AgentRole::from_str` thin layer for legacy display sites. Delete the
   bail at `crates/orchestrator/src/supervisor/review_flow.rs:325-329`.
   Unblocks B1, B2.
2. `runtime/expand-agent-routing` — extend `AgentRouting` to deserialize
   `kind, execution_mode, prompt_template, input_schema, output_schema,
   verifiers, tools, max_iters, max_cost_usd`. Wire schema and verifier
   construction off the parsed fields, not `include_str!`. Drop
   `standard_for_role(AgentRole)` in favor of
   `VerifierLadder::from_names(&[String])`. Unblocks B4, H2.
3. `runtime/prompt-template-loader` — collapse
   `crates/orchestrator/src/supervisor/prompts.rs:3, 22, 98, 316, 329, 337,
   343` into one `render(role.prompt_template, ctx)` call. Move per-role
   prompt text to `prompts/<dag-id>/<role-id>.md`. Unblocks B3, Part A
   item 17 in full.
4. `runtime/feeds-meta-and-gate` — add `feeds_meta: bool` and
   `gate: { min_usable, sources: [...] }` to `DagNode`. Replace
   `MIN_SPECIALIST_QUORUM` constant with manifest read. Delete
   `crates/orchestrator/src/review_dag.rs` once the constant is gone.
   Unblocks H1, M4.
5. `runtime/dag-executor` — implement executor branches for the orphan node
   kinds (`prepare_inputs`, `ingest_source`, `artifact`, `dag_call`, `tool`).
   Have `crates/orchestrator/src/ingest_pipeline.rs` walk
   `dags/paper-extract.yaml` instead of calling stages by name. Emit
   `DagRunReport`. Closes R3.
6. `cli/scaffold-agent` — add `grokrxiv dag scaffold-agent --intent <text>`
   (the LLM-facing authoring tool). Extend `--model-for` / `--runner-for`
   to accept `<dag>.<role>=value`.
7. `tier1/cli-retry-tracking` — Part A item 7: route
   `run_review_for_paper_blocking` through `run_item`. Part A item 26
   fall-out: refuse stateless mode (or persist to JSONL).
8. `tier1/render-role` — Part A item 1: either add `AgentRole::Render` or
   (preferred, per R1) drop `RenderAgent` off the trait and treat it as a
   string role.
9. `tier2/supervisor-timeout-and-scheduler-state` — Part A items 13
   (persistent last-completed-date) and 15 (tokio `timeout()` at
   supervisor).
10. `extract/agent-enabled-smoke` — add a smoke test for
    `GROKRXIV_EXTRACTION_MODE=agent_enabled` so `validation:` is exercised.
    Pair with a fail-loud branch when LaTeXML is needed but disabled.

Leaves 1–4 are the critical path: they collectively deliver the merged
plan's acceptance test (add a `type_theory_validator` role without a DB
migration, run a review, observe it works). Leaves 5+ are the
foundational-harness work from Part B.

## 8. What's in good shape

Do not undo:

- Web side already generic. `apps/web/lib/types.ts:59` + accordion fallback
  labels mean the UI is ready for arbitrary roles the moment the Rust side
  opens up.
- DB columns and queries already DAG-aware. `dag_type`, `node_id`,
  `node_kind`, `agent_type` all thread through
  `crates/orchestrator/src/db.rs`. The migration is sloppy (duplicate
  ALTERs from the prior review) but the data path is right.
- Publish/webhook hygiene is solid. Idempotency, fail-loud, ACK-then-process,
  marker-on-metadata — Tier 1 items 3+4 and Tier 2 item 14 all delivered.
- `dags/paper-extract.yaml` is the right shape. Tools registry, declarative
  inputs/outputs, `dag_call` chain. Once an executor walks it (leaf 5),
  extraction stops being a 3230-LoC imperative blob.
- Citations as a tool-loop agent.
  `crates/orchestrator/src/agents/extraction/citations/mod.rs` is a strong
  proof point for `execution_mode: tool_loop` once that field exists.

## 9. Plan-level reconciliation

One inconsistency surfaced during the audit and is worth resolving before
relying on this report:

- Publish reconcile loop: `crates/orchestrator/src/supervisor/publish.rs:8-52`
  defines `spawn_publish_reconcile()`. Whether the function is called from
  `Supervisor::spawn` should be verified directly with
  `rg "spawn_publish_reconcile" crates/`. If there is exactly one hit (the
  definition), it is a no-op and Tier 1 item 6 remains a hazard. If there
  is a call site too, item 6 is delivered.
