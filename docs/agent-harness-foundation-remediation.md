# Agent harness + orchestration: foundation remediation plan

**Date:** 2026-05-20
**Scope:** Deep review of the agent review harness and orchestration layer following the 2026-05-20 Part-A remediation merge, plus a framework / foundational-harness gap analysis for research-paper reviews at scale.
**Status:** Roadmap — not implemented.

---

## Where the codebase actually stands

Today's `952c4e5` (Merge fp-remediation-root) plus the post-merge fixes (`ec339df` budget supervisor timeout for corrective retries, `72ae2b5` kill cli subprocess groups on timeout) landed roughly 24 of the 27 Part A items. The CLI smoke (`tests/manual/smoke-root-72ae2b5.json`) confirms the harness runs end-to-end against arXiv `2605.17307`: 6/6 agents `verifier_status: pass`, a real PR (#31) opened, status `pr_open`. The system works.

It's also one merge away from being a real framework.

---

## What today's remediation actually accomplished

**Strong wins:**

1. **`ReviewAgent` trait collapsed to `ConfiguredAgent` struct.** `traits.rs` is 55 lines (down from 76). The 6 per-role macro-generated structs are gone. `build_agent` is a 2-line factory. This is the cleanest single piece of the new harness.

2. **Schema as `Arc<serde_json::Value>` via `AgentSchema` type alias.** No more 50KB clones per call. `AgentSpec` is now cheap to pass around.

3. **Cache + persistence honour the resolved runner.** Migration `20260520000001_agent_runner_persistence.sql` adds `runner text not null default 'api' check (runner in ('api','cli','cloud','local_inference'))` on both `review_agents` and `review_cache`. `AgentRun::from_cache` now takes `runner: AgentRunnerKind` and threads it through. Cost attribution is honest.

4. **`role_routing` deleted.** The dead parallel registry is gone. `state.agents` is the sole source of truth. ~80 lines of dead-code removed.

5. **Corrective retry actually mutates the system prompt.** `corrective_system_prompt(&system, &first_err)` differentiates `OutputFailureKind::SchemaValidation` from prose-wrap. Two tests pin the behaviour (`corrective_retry_changes_system_prompt_for_prose_wrapped_json`, `…_for_schema_failure`). The "model wrote prose, retry sends same prompt, model writes prose again" loop is broken.

6. **Supervisor-level timeout wraps every `agent.run`.** `run_agent_with_supervisor_timeout` (supervisor.rs:3141) hard-caps each call at `spec.timeout_secs + 30`. Wedged runners stop hoarding semaphore slots.

7. **CLI subprocess `kill_on_drop(true)`** plus process-group kill on timeout (`72ae2b5`). A wedged claude/codex/gemini subprocess actually dies instead of leaking.

8. **Publish-after-approval idempotency via `publish_inflight: Mutex<HashSet<Uuid>>`.** Second click logs `"publish job already in flight"` and returns `Ok(())` without enqueueing. Cleanup on enqueue failure. Test at `supervisor.rs:3307`.

9. **PR-open reconcile loop** spawns from `serve.rs:60`, polls every 300s, walks `list_pr_open_reviews_with_urls`, queries GitHub via octocrab, calls the existing `finalize_published_review`. Reconcile stats logged per pass.

10. **Graceful shutdown.** `serve.rs` installs SIGTERM/Ctrl-C handlers, drives `axum::serve(...).with_graceful_shutdown(...)`, and calls `supervisor.shutdown()` which trips a `watch::Sender<bool>`. Inflight work drains.

11. **Stateless mode disables ingest.** `should_spawn_scheduler(db.is_some(), GROKRXIV_DISABLE_SCHEDULER)` — no DB = no scheduler = no orphan work.

12. **Webhook ACK + async pattern.** `ack_and_spawn_pull_request_synchronize` and the merge path both: dedupe via `record_github_delivery`, record the event, return 200, do the work in `tokio::spawn`. The 10s GitHub timeout is no longer a duplicate-delivery vector.

13. **PR label/title marker extraction** with body fallback. `extract_review_id_from_pull_request` checks the hard-to-lose metadata first.

14. **Doctor preflight at boot.** `validate_required_cli_runners` runs in `AppState::from_config`, refuses to start when a configured CLI is missing.

15. **Bounded worker pool.** `DEFAULT_SUPERVISOR_WORKER_LIMIT` + `DEFAULT_SUPERVISOR_QUEUE_CAPACITY` knobs. The old "spawn-per-item, unbounded" pattern is gone.

## What got sidestepped or introduced

1. **Item 1 (`AgentRole::Render` variant) was sidestepped, not fixed.** RenderAgent was moved outside the configured-review-agent registry — it's a separate `RenderAgent` struct in `review_agents.rs` with its own `run()` method. `RenderAgent::spec().role` still returns `AgentRole::MetaReviewer`. The test `test_render_agent_keeps_spec_without_review_role_identity` literally pins this. **Today's collision risk is real if any new observability code keys by `spec.role`**: a render run would silently land in the MetaReviewer bucket. The architectural smell remained; only the immediate collision symptom was avoided.

2. **Item 20 (delete Cloud + LocalInference runners) didn't happen.** `cloud.rs` is still 613 lines, `local_inference.rs` 493 lines, both still in `AgentRunnerKind` enum, both still registered in the runner registry at boot — none invoked by the default CLI smoke. ~1,100 LoC of speculative code lives on.

3. **Item 18 partial — `AgentMode` and `RevisionTarget` were *relocated*, not deleted.** They moved off `AgentSpec` (good) but stayed in `runtime_config.rs` as `default_mode` and `revision_target` fields, still exposed as hidden CLI flags. `AgentMode::ReviewAndRevise` still has no caller in the live pipeline. Either wire it or remove the runtime config field — it's now an indirection over an unused enum.

4. **Item 13 partial — scheduler in-process retry only.** Per `docs/webhook-scheduler-hardening-applied.md`: "This pass did not add a persistent scheduler checkpoint table. The retry cursor is process-local; a durable database checkpoint can be added in a later schema pass." A process restart still loses the cursor.

5. **A new "revision_needed PR" code path is undocumented in schemas.** The smoke result shows `"pr_kind": "revision_needed"` and `"gate_verdict": "fail"` with a real PR opened (`#31`). That means when the meta-reviewer recommends `major_revision`, the system now opens a request-for-revision PR instead of withholding publication. This is reasonable behaviour but it's a new state machine path that diverged from the original "gate fail → withdrawn" rule. I couldn't find a documented contract for `pr_kind` in `review_bundle_metadata.schema.json` or elsewhere. Worth pinning in a schema.

6. **The CLI smoke gate checks `verifier_status` per agent, not `gate_verdict`.** Today's smoke passes the gate ("6/6 pass") even though `gate_verdict: "fail"`. That's correct as a regression test — it's "the harness ran" not "the review's verdict was accept" — but operators reading the file might mistake `gate_verdict: fail` for a regression. Worth a one-line comment in the JSON or rename `gate_verdict` to `meta_recommendation_gate`.

7. **`supervisor.rs` grew from 3,524 → 4,472 lines.** The remediation added ~950 lines net. The file is moving in the wrong direction for legibility. The split-by-concern idea from the original review (dag/prompts/merges/cache/verify modules) is more urgent now than two weeks ago.

---

# Can this become a foundational harness for research-paper reviews at scale?

## Bones of a framework that the remediation strengthened

These are now demonstrably framework-shaped:

- **`AgentRunner` trait + 4 backends.** Genuine pluggability. CliRunner, ApiRunner, CloudRunner, LocalInferenceRunner all conform to one signature. Adding a 5th (e.g. Bedrock, Together, your-own-gateway) is a single trait impl. The trait stayed cleanly minimal.
- **`AgentSpec` / `AgentInput` / `AgentRun` lifecycle.** Cheap to clone (post-`Arc<Value>`), schema-enforced, runner-agnostic. The contract is portable.
- **Verifier ladder with per-role composition.** `VerifierLadder::standard_for_role(role, schema)`. New rungs are one trait impl. Citation/metadata/render/support/tone/json_schema already swappable.
- **Fact-merge pattern.** `merge_*_into_output` per specialist. The killer architectural primitive: deterministic verifier owns ground truth, LLM owns judgment, merge before persistence. Generalizes to any domain that has "facts you can compute" plus "judgments you need a model for."
- **Cache by `(paper_id, role, content_hash)` with TTL.** Schema is paper-specific but the indexing logic is generic.
- **Schema-validated output contract per role.** Strong invariant; the model is told the schema, the verifier validates against the same schema, the cache keys against the schema-shaped output. Three layers, one source of truth.
- **Reconcile loop pattern.** `spawn_publish_reconcile` is the right shape for any "background process drifted from authoritative external state" — easily generalized to a `ReconcileLoop` trait.
- **Bounded worker pool with operator knobs.** `GROKRXIV_SUPERVISOR_WORKERS` / `GROKRXIV_SUPERVISOR_QUEUE_CAPACITY`. Scale knobs exist.

## Hard couplings still blocking the framework story

Severity-tagged. **B** = blocker for non-arXiv domains. **S** = scaling concern at >100 reviews/day.

### **B1. `AgentRole` is still a 6-variant enum in `grokrxiv_schemas`.**
Adding `triage`, `ethics`, `statistical_methodology`, `computational_chemistry` still requires touching the enum, the migration (CHECK constraint at `init.sql:70`), `role_slug`, `role_sort_key`, `ROLE_FILES`, `parse_role_slug`, plus per-role code paths in `supervisor.rs` (system prompt, merge functions, prompt builder). The remediation kept the enum. Roles are still code, not config.

### **B2. `PaperExtract` is the only `Artifact` the harness knows.**
70+ call sites take `&PaperExtract`. `specialist_facts.rs` (821 lines, untouched today) is hard-typed against paper structure. `VerifierContext.paper: &PaperExtract`. `ExtractionContext.extract: &PaperExtract`. Switching to "review a code repo" or "review a dataset" requires a typed-trait abstraction or a wide refactor.

### **B3. `review_dag.rs` is still decorative.**
340 lines, zero commits today. The executor still walks `specialist_roles` manually. `ReviewDag::canonical()` is built and validated at runtime, then ignored. The whole "different review topologies per project" story needs the executor to actually drive `ReviewDag`.

### **B4. `build_specialist_prompt` (~280 lines in `supervisor.rs`) is hardcoded paper-domain scaffolding.**
"Paper title / Abstract / Section headings / Paper body / Citation contexts / Bibliography / Verified availability facts / Verified prior-art candidates / Verified structural facts." Right for arXiv, wrong for anything else. A foundation needs `RoleSpec.render_prompt(&Artifact, &Facts) -> String` per role.

### **B5. `is_code_amenable_field`, `body_budget_chars`, proof-as-code system-prompt mutation.**
All in `supervisor.rs`, all hardcoded to arXiv field prefixes. A bio/clinical/legal/RFP review pipeline would inherit these inappropriately.

### **B6. ingest crate is `arxiv_id → PaperExtract`.**
No source-adapter trait. `crates/ingest/src/source.rs` already added local-PDF + git-repo paths (per yesterday's `2b85331` checkpoint merge), but they all converge on `PaperExtract`. A real adapter trait would let "ingest" be a registry.

### **B7. publisher crate is `OpenReviewPr → GitHub`.**
No publish-adapter trait. OpenReview, journal submission systems, email handoff, REST webhook — all would require fork-and-rewrite today.

### **S1. Cache invalidation is implicit.**
TTL-based 30-day expiry per `review_cache` plus `verifier_status='pass'` gate on read. No explicit invalidation when (a) a prompt template changes, (b) the schema changes, (c) the model upgrades. A schema-id or prompt-hash dimension on the cache key would prevent stale outputs after a deploy.

### **S2. No per-paper cost ceiling.**
Each role's model is in `agents/*.yaml`. A pathological paper (long bibliography, large body, many tables) can run up against the per-role token caps repeatedly, costing $10+ for a review the operator wanted capped at $0.50. `AgentSpec` has no budget field; supervisor has no aggregate cost tracker per review.

### **S3. No escalation policy.**
"If 2 specialists disagree, escalate to a higher-tier model" is a natural pattern at scale. Today the recommendation is the deterministic output of one meta-reviewer call. A `tiebreaker` agent role + a "specialist disagreement detector" verifier rung would let the framework express ensembles.

### **S4. No reviewer profiles.**
"Harsh statistical reviewer" vs "constructive theoretical reviewer" vs "skeptical methodology reviewer" — same role, different lens. A `Profile { role_id, prompt_overlay, model, temperature, verifier_weights }` would enable ensemble reviews and A/B experimentation. Today a role is one `AgentSpec`, period.

### **S5. No document version model.**
`papers` is keyed by `arxiv_id`, reviews FK on `paper_id`. When an author revises and re-submits, the existing review history is "superseded but lost." Foundation needs `documents` + `document_versions` + `reviews FK on (document_id, version)` with cross-version diffing.

### **S6. Multi-tenant isolation is implicit.**
There's no `tenant_id` on any table I can see. The current model assumes one operator. A SaaS framework needs per-tenant quotas, RLS per tenant, per-tenant agent configs.

### **S7. No reviewer audit trail beyond prompts.**
`tools/manual/smoke-root-*.json` captures terminal state but not what the model saw or said per turn. For research-paper review at scale you want a per-review transcript bundle (input prompt + system prompt + raw model output + verifier diagnostics) so any disputed review can be audited. The pieces exist; they need a `audit_bundle.zip` per review.

### **S8. No A/B / shadow review infrastructure.**
"Run the same paper through two model configurations and compare verdicts" — no first-class support. This is how you'd validate a new model release or prompt change.

### **S9. No streaming progress to the moderator.**
6 specialists run in parallel; the operator sees nothing until all 6 finish + meta runs. For a 5-minute review that's fine; for a longer review (e.g. a long paper with deep fact verification) it's frustrating. Server-sent events per specialist completion would help.

---

## Refactor path — three moves, then you're a framework

These supersede the prior R1/R2/R3 with today's state as the baseline. Effort estimates are post-remediation (the supervisor is cleaner now; some of the work is already partially done).

### **R1. Roles become runtime config (B1 + B4 + B5 + S2 + S3 + S4).**

Replace the `AgentRole` enum with a runtime-loaded role registry. `RoleSpec` carries everything a role needs:

```rust
struct RoleSpec {
  id: String,                        // "summary" | "stats_methodology" | "ethics"
  display_name: String,
  prompt_template: PromptTemplate,   // tera/handlebars; renders against Artifact + Facts
  system_prompt: String,             // also templatable
  schema: Arc<Value>,
  verifiers: Vec<VerifierConfig>,    // declared, not hardcoded
  facts: Vec<FactPluginRef>,         // per-role deterministic fact gathering
  budget: CostBudget,                // per-role $cap + token cap (S2)
  escalation: Option<EscalationRule>,// "if confidence < X, escalate to model Y" (S3)
  profile: Option<Profile>,          // overlay (S4)
  spec: AgentSpec,                   // provider/model/runner/timeout
}
```

DB: `review_agents.role` becomes `text` (CHECK constraint dropped or expanded with a registry-aware migration). Supervisor's role-keyed `HashMap<AgentRole, _>` becomes `HashMap<String, RoleSpec>`. Adding a new reviewer = add `agents/<role>.yaml` + `schemas/<role>.schema.json` + `prompts/<role>.md`. No code change, no migration.

This also fixes the dormant `AgentRole::Render` placeholder (Render becomes one role registry entry, with its own schema, no enum collision).

**Effort: ~10-14 days. Unlocks everything downstream.**

### **R2. `PaperExtract` → `Artifact` trait (B2 + B6 + S5).**

```rust
trait Artifact: Send + Sync {
  fn kind(&self) -> &str;                         // "arxiv_paper" | "code_repo" | ...
  fn id(&self) -> &str;
  fn version(&self) -> u32;                        // S5: first-class versioning
  fn primary_text(&self) -> Option<&str>;
  fn structured(&self) -> &Value;
  fn metadata(&self) -> &Map<String, Value>;
  fn fact_inputs(&self) -> FactInputs;             // urls, citations, tables — what facts plugins read
}
```

`AgentInput.artifact: Arc<dyn Artifact>`. `VerifierContext.artifact: &dyn Artifact`. `specialist_facts.rs`'s `gather_*_facts` become role-config-attached `FactPlugin` impls instead of supervisor functions.

DB: `artifacts` table with `kind` discriminator + `version`; `papers` becomes a view (or migrate via UNION); `reviews.paper_id → artifact_id + artifact_version`. Backwards-compat view for the existing UI.

Source-adapter trait (B6) falls out of this naturally: `trait SourceAdapter { fn fetch(source_ref: &SourceRef) -> Box<dyn Artifact> }`.

**Effort: ~8-12 days. Unlocks domain-portability.**

### **R3. DAG composition driver (B3 + S3 + S8).**

Make `review_dag.rs` the supervisor's actual executor. Each `ReviewNodeKind` becomes a handler trait:

```rust
trait NodeHandler {
  async fn run(&self, ctx: &DagCtx, deps: &[NodeOutput]) -> NodeOutput;
}
```

DAG topology is config:

```yaml
# dags/arxiv_review.yaml
nodes:
  - { id: prep, kind: prepare }
  - { id: summary, kind: specialist, role: summary, depends: [prep] }
  - { id: tc, kind: specialist, role: technical_correctness, depends: [prep] }
  ...
  - { id: tiebreaker, kind: conditional_specialist,
      role: tiebreaker, run_if: "specialists_disagree({{tc.confidence}}, {{novelty.confidence}})",
      depends: [tc, novelty] }
  - { id: meta, kind: meta_reviewer, min_quorum: 3, depends: [...] }
```

Different projects (or different artifact kinds) = different DAG yamls. The "review-and-revise" path becomes a DAG with an `apply_revisions` node, not a hidden enum.

Shadow-review and A/B (S8) become DAG configurations: run two parallel `specialist` nodes for the same role with different specs, compare outputs.

**Effort: ~5-7 days. Unlocks per-project topologies, ensembles, escalation, A/B.**

## Stretch (not on the critical path but valuable at scale)

- **S7 audit bundle**: after every review, persist `audit/<review_id>.zip` containing prompts, raw outputs, verifier diagnostics, fact-plugin results. Two days of work.
- **S6 multi-tenant**: `tenant_id` column on every table + per-tenant RLS + per-tenant agent config dir. Two-three weeks of careful migration.
- **S1 cache invalidation by prompt-hash / schema-id**: add columns to `review_cache`, invalidate on deploy. Two days.
- **S2 cost ceiling enforcement**: per-`RoleSpec` budget field + per-review aggregator + early-abort on cap. Three days.
- **S9 streaming progress**: SSE endpoint that emits `specialist.completed` events as agents finish. Two days.

## What's NOT a foundation story (don't chase)

- Re-doing the runner abstraction. It's already framework-shaped.
- Adding more runner backends speculatively. Cloud + LocalInference are still dormant — adding Bedrock or Together without a real user is more dead code.
- Switching the verifier ladder design. The current `(name, status, notes)` shape composes well and persists cleanly.

---

## Recommendations on sequencing

1. **Close the three sidestepped items first.** They're cheap and prevent future confusion:
   - Add `AgentRole::Render` enum variant + migration (1 day) so the placeholder is gone.
   - Delete Cloud + LocalInference runners + enum variants (~1 day; recoverable from git).
   - Document the `pr_kind: revision_needed` state machine in `schemas/review_bundle_metadata.schema.json` (2 hours).

2. **Split `supervisor.rs` into modules.** It's 4,472 lines. The dag/prompts/merges/cache/verify split from earlier reviews now has even more cause. Pure refactor, no behavior change. ~3 days.

3. **Then commit to R1 (RoleSpec)**. It's the highest-leverage foundational move and is most disruptive to land — better to do it before the codebase grows further.

4. **R2 and R3 can land after R1 in either order**, but R3 is faster (~1 week vs R2's ~2 weeks).

5. **Pick the framework's first second domain** before doing R1+R2+R3. Without a concrete second-domain consumer the abstractions will be speculative; with one (e.g., "review my company's RFCs" or "review research-grant proposals") every refactor decision has a forcing function.

6. **Add an audit-bundle stage and a per-paper cost ceiling early in the foundation work**. Both are operator-facing scale features that pay back immediately, even without R1-R3, and they constrain how the foundation is designed.

---

## Bottom line

The remediation closed the bleeding. The harness is now production-shaped: bounded workers, graceful shutdown, idempotent publish, real reconcile loop, honest cost telemetry, structured corrective retries, supervisor-level timeouts, subprocess cleanup. That's most of what "at scale" needs from a single-domain operational perspective.

What's left for "foundational research-paper review framework at scale" is a clean separation between *configuration* (roles, prompts, DAGs, schemas, profiles) and *executor* (the supervisor that walks any DAG over any artifact through any runner). The runner is already there. The artifact and role aren't. Three focused refactors land the framework. Pick a second domain before starting them.

---

## Verification (when items from this plan land)

Every change goes through the cost-control regression gate:

```
grokrxiv review --runner cli --extractor cli --no-cache --json 2605.17307
```

All 6 agents must report `verifier_status: pass`. Exit 0. Capture JSON to `tests/manual/smoke-<sha>.json` and commit alongside the change. `tests/m1-pipeline.sh` is deprecated and is NOT a gate.

If the smoke regresses, stop and fix it in the same diff. Revert if the fix scope-creeps.
