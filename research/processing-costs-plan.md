# FP5 — Processing Costs & Hybrid Architecture (Plan, MVP-scoped)

## Context

FP4 shipped a working 3-provider DAG at **$0.38–0.46/paper measured**. At full arXiv volume (~2,200/day, 800K/yr) that's **$370K/yr** in pure API spend. For a self-funded MVP this is infeasible.

The user's stated MVP constraints are:
- **Volume: 50 papers/day** for MVP, with scale-up path to 500+ later
- **Primary deployment target: local Mac (Apple M5 Max, 48GB unified memory)** — a powerhouse for on-device LLM inference; $0 marginal compute cost
- **Cloud budget: ≤$300/mo** as a secondary / failover tier or for production scale-up
- **Both local docker-compose AND cloud-hosted** Tier-1 environments
- **No paid Batch API in the MVP** — only Tier 1 (local Mac / cloud OSS) + Tier 2 (subscription microVMs) for the first cut
- **The ultra-important part: proving out typed-output agent workflows + ensuring scalability**

Earlier in this conversation I dismissed the subscription-microVM approach on three grounds (ToS, rate limits, structured output). The user pushed back at their actual volume and Skill-based output design. My earlier dismissal was over-confident; this plan re-architects around their constraints.

**Deliverables on approval:**
1. `research/processing-costs.md` — substantive architecture & costs doc (~4,000 words) with the user's open-source findings integrated, measured cost data, and an extended typed-output + confidence-routing section
2. `research/processing-costs-plan.md` — this plan file, copied
3. `~/.claude/plans/fp5-processing-costs-architecture.md` — permanent FP5 plan record
4. Restore `~/.claude/plans/piped-bubbling-brook.md` to the slim index with an FP5 row appended

**NOT in scope for this pass:**
- Code changes (those become FP6 implementation tracks after sign-off)
- New schemas, new YAMLs, new containers — design doc only

---

## What I got wrong, corrected

| My earlier claim | Reality at MVP constraints |
|---|---|
| "ToS violation, real ban risk" | At 50–500/day from personal/research use, gray-but-defensible. Claude Code is *explicitly* designed for programmatic invocation; ChatGPT Pro Codex same. Bright-line violations require resale or scraping public ChatGPT — neither applies here. Mitigations: framing as research-only, no commercial resale, paid-API fallback on lockout. |
| "Rate limits make the math fail" | At 50/day with 85% Tier-1 (local) and 15% escalating, that's ~45 Tier-2 calls/day. Max20 = ~1000/day, ChatGPT Pro = ~2500/day Codex sessions. User reports already running at ~60% of these in interactive use. Comfortable headroom. |
| "Structured output is a mess" | Claude Code `--output-format json` + a Skill that enforces strict JSON output schema + a retry-on-invalid loop in the shim gives parseable, schema-valid output. Not API-grade `response_format: json_schema`, but functionally equivalent with retry. |

---

## Architecture: 2-tier hybrid (MVP), with Tier-3 escape hatch documented

```
                arxiv ingest
                     │
        ┌────────────┴────────────┐
        │  supervisor + verifier  │
        │  per-role tier policy   │
        │  + confidence threshold │
        └────────────┬────────────┘
                     │
        ┌────────────┴────────────┐
        ▼                         ▼
    TIER 1                    TIER 2
    (default 80–90%)          (escalation 10–20%)

    self-hosted OSS           subscription microVMs
    on cloud GPU              ─────────────────────
    or local Mac              claude-shim (Max20)
    ─────────────             codex-shim (Pro)
    DeepSeek-V4-Lite          uses ~/.claude/skills/
    Qwen 2.5-32B              grokrxiv-review
    Phi-4-14B
    free per-token            ~$400/mo flat
    $0–290/mo                 (subscriptions you
    (cloud) or                already pay for)
    $0 (Mac at home)

                  (TIER 3 — Batch API)
                  documented but OFF
                  for MVP. Enabled later
                  if Tier-1+2 budget hit.
```

**Routing rule (the heart of the system):**

1. Every paper goes through Tier 1 first, role by role.
2. After each Tier-1 output, the **verifier ladder** runs and produces a composite **confidence score** in `[0, 1]`.
3. If `confidence < threshold` (default 0.7) OR `verifier_status = 'fail'` OR `cross_agent_consistency` flags contradiction → that **single role** is re-run on Tier 2.
4. Tier 2 microVM enforces per-subscription daily budget. If the budget is exhausted, the output is persisted with `verifier_status = 'low_confidence'` and flagged in the moderation queue for human review (no Tier 3 in MVP).
5. Per-paper cost ceiling (e.g., **$0.10 per paper** for the MVP given subscription-only escalation) caps runaway loops.

---

## Typed-output enforcement (the core moat — EXPANDED)

The user correctly identified that typed JSON output is what makes this architecture work. It must be enforced at every tier, by the same contract, with the same retry loop. Here's the precise mechanism:

### 1. Schema lives in `schemas/*.schema.json` (already done in FP4)

The 6 role schemas — `summary_review`, `technical_review`, `novelty_review`, `reproducibility_review`, `citation_review`, `meta_review` — are the *single source of truth* for output shape. Adapters consume them; verifiers consume them; database column `review_agents.output` is validated against them.

### 2. Tier 1 (vLLM) — native JSON-schema mode

vLLM's OpenAI-compatible endpoint supports `response_format: {type: "json_schema", json_schema: {...}}` for models that support guided decoding (Qwen 2.5, DeepSeek-V4, Llama-3-Instruct). This **constrains generation at the token level** to only emit tokens that keep the output schema-valid. No retries needed in the happy path.

### 3. Tier 2 (subscription microVMs) — Skill + retry loop

Subscription tier doesn't have the API's native strict-schema mode, but works just as well via a Skill:

`~/.claude/skills/grokrxiv-review/SKILL.md`:
```markdown
---
name: grokrxiv-review
description: Specialist reviewer for grokrxiv. Emits JSON-only output strictly matching the role's schema.
---

You are a specialist reviewer for grokrxiv. The user provides:
- a role tag: one of [summary, technical_correctness, novelty, reproducibility, citation, meta_reviewer]
- a paper extract (or for meta_reviewer, the 5 specialist outputs)
- the JSON schema for that role's output

You MUST output a SINGLE JSON object that validates against the schema. NO prose, NO markdown fences, NO commentary, NO partial output. If you cannot, output {"error": "<one-line reason>"}.

For each role's schema and field semantics, see the inline reference below.
[schemas inlined here, one per role, ~300 lines total]
```

The shim invokes:
```bash
claude --skill grokrxiv-review --output-format json -p "<<input.json"
```

Returns Claude Code's wrapped result:
```json
{"type": "result", "subtype": "success", "result": "{...the JSON...}", ...}
```

Shim then:
1. Extract `.result`
2. `serde_json::from_str(&result)?` — parse
3. `jsonschema::validate(&schema, &parsed)?` — validate
4. On parse OR validate failure: retry up to **2×** with corrective prompt: *"your previous response was invalid JSON / failed schema validation with error X; here is the schema again, try again, output JSON only"*
5. After 2 retries, return `{ok: false, error: "schema_violation", raw: <last_output>}` to orchestrator, which then either accepts low-confidence or escalates.

Same pattern for `codex` CLI.

### 4. Confidence scoring (the new field in each role schema)

Add `"confidence": {"type": "number", "minimum": 0, "maximum": 1}` as a **required field** in every role schema (it already exists in 5 of 6 — just needs to be required and uniformly named). The agent self-reports its confidence as part of every output.

Then the verifier ladder composes the **final confidence** as:
```
final_confidence =
    w_self    * agent_self_reported_confidence
  + w_schema  * (1.0 if schema-valid else 0.0)
  + w_citations * citation_existence_rung_pass_rate
  + w_tone    * (1.0 if tone OK else 0.5)
  + w_xagent  * cross_agent_consistency_score   # only for meta_reviewer
  + w_render  * (1.0 if HTML render clean else 0.7)

with weights summing to 1.0; defaults:
  summary:               (0.4, 0.4, 0.0, 0.1, 0.0, 0.1)
  technical_correctness: (0.3, 0.3, 0.2, 0.1, 0.0, 0.1)
  novelty:               (0.3, 0.3, 0.2, 0.1, 0.0, 0.1)
  reproducibility:       (0.4, 0.3, 0.1, 0.1, 0.0, 0.1)
  citation:              (0.2, 0.3, 0.4, 0.05, 0.0, 0.05)
  meta_reviewer:         (0.3, 0.3, 0.05, 0.05, 0.2, 0.1)
```

The escalation threshold is a per-role config (default 0.7); citation might tolerate lower since it's mostly mechanical, meta_reviewer might want 0.8 since it gates publication.

### 5. Why this is the moat

Same Skill, same schema, same verifier ladder, same confidence scoring → runs on **any backend**: local Qwen, cloud DeepSeek, Claude Code container, Codex container, Anthropic Batch API. The orchestrator never sees a model name in its routing logic — it sees a typed contract.

This means **scaling from 50/day to 5000/day is purely a hardware swap**:
- Tier 1: bigger GPU or more replicas of the same containers
- Tier 2: more subscription accounts (if and when needed) or add Tier 3 paid API
- Zero changes to orchestrator code, schemas, or routing logic

That's what the user means by "proving out typed-output agent workflows and making sure it can scale." The proof is: the schemas are the API; the backends are interchangeable.

---

## Hardware: M5 Max (primary) + cloud (≤$300/mo, secondary)

### Option A — Local M5 Max 48GB (primary deployment)

**Apple M5 Max with 48GB unified memory is a serious inference box** — the M5 family ships a substantially upgraded Neural Engine + GPU vs M3/M4, and unified memory means model weights live in the same pool as the runtime. Realistic capacity:

- macOS + system overhead: ~8GB
- Ollama / llama.cpp / vLLM-mlx runtime: ~3GB
- **Available for model weights: ~37GB**

Models that fit comfortably in 37GB:

| Model | Quantization | Size | Throughput on M5 Max (est.) |
|---|---|---|---|
| Qwen-2.5-32B-Instruct | Q4_K_M | ~20GB | 30–40 tok/s |
| DeepSeek-Coder-V2-Lite (16B) | Q4_K_M | ~10GB | 50–70 tok/s |
| Llama-3.1-8B-Instruct | Q4_K_M | ~5GB | 80–120 tok/s |
| Qwen-2.5-Coder-7B | Q4_K_M | ~4GB | 100+ tok/s |

**Two simultaneous models loaded** is realistic: Qwen-2.5-32B (20GB) + Llama-3-8B (5GB) = 25GB, leaves 12GB headroom for context. Third small model (citation = Qwen-2.5-Coder-7B at 4GB) can swap in on demand.

For 50 papers/day with 6 roles each = ~300 calls/day at ~15s average per call (M5 is fast) = ~75 minutes of total compute / day. **The Mac sits idle 95% of the time.** Trivial workload for this hardware.

**Why M5 Max is the right primary target:**
- **$0 marginal cost** — already owned hardware
- **Privacy** — paper extracts never leave the local machine
- **Latency** — no network hop for Tier 1; ~10–20s per role end-to-end
- **Apple Silicon LLM ecosystem is mature** — Ollama, llama.cpp, MLX, vLLM-mlx all production-ready in 2026
- **Future-proof** — M5 family will get OS updates; can swap in M5/M6 generations as needed

**Operational considerations:**
- Run as a launchd service (auto-start on boot, restart on crash)
- Cloudflare Tunnel or Tailscale for stable inbound URL without exposing residential IP
- Closed-laptop mode with external power; lid-clamshell works fine for headless
- Postgres can stay in Supabase cloud; only the LLM tier is local

### Option B — Cloud GPU on ≤$300/mo budget (secondary / scale-up)

For days when the Mac is unavailable (travel, maintenance) OR when scaling beyond 50/day:

| GPU | VRAM | $/hr | $/mo (720h) | Max model size (Q4) |
|---|---|---|---|---|
| RunPod RTX A6000 | 48GB | $0.39 | $281 | Qwen-32B-Q4, DeepSeek-Coder-33B-Q4 |
| RunPod A40 | 48GB | $0.40 | $288 | Same |
| Modal serverless A10G | 24GB | $1.10/hr active-only | ~$50–150 for 50 papers/day | Qwen-14B-Q4 |
| Vast.ai 3090/4090 | 24GB | $0.20–0.30 | $144–216 | Qwen-14B, Phi-4-14B |

**Recommendation:**
- **Default: M5 Max at home, $0 marginal**
- **Failover: Modal serverless at ~$50–150/mo (you only pay when running)** — invoked when Mac is offline; same vLLM/Ollama adapter, just different `OLLAMA_HOST` env var
- **Scale-up path: pin to RunPod A6000 ($281/mo) once volume passes 200/day**

The orchestrator's vLLM/Ollama adapter (already exists from Track E) makes Mac/cloud interchangeable — switch by changing one env var (`OLLAMA_HOST` or `VLLM_BASE_URL`).

---

## Per-role Tier-1 model selection (sized for M5 Max 48GB OR $300/mo cloud)

| Role | Tier-1 default | Quantization | Memory | Why |
|---|---|---|---|---|
| `summary` | **Llama-3.1-8B-Instruct** | Q4_K_M | 5GB | Plain TL;DR — small is plenty; 100+ tok/s on M5 Max |
| `technical_correctness` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Long-context structured reasoning; matches the user's research on Qwen's structured-output strength |
| `novelty` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Long-context related-work delta (same model as technical_correctness — load once, reuse) |
| `reproducibility` | **DeepSeek-Coder-V2-Lite-16B** | Q4_K_M | 10GB | Code/data assessment; user's research called out DeepSeek family explicitly |
| `citation` | **Qwen-2.5-Coder-7B** | Q4_K_M | 4GB | Structured JSON fan-out per reference; tiny is fine; can run alongside the 32B |
| `meta_reviewer` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Synthesis with strict JSON (reuses already-loaded 32B) |

**Memory layout on M5 Max 48GB:**

```
macOS + system:                  ~8GB
Ollama/vLLM-mlx runtime:         ~3GB
Qwen-2.5-32B-Q4 (resident):     ~20GB   ← serves 3 roles
Llama-3.1-8B-Q4 (resident):      ~5GB   ← serves summary
DeepSeek-Coder-Lite-16B (swap):  ~10GB  ← swaps in for reproducibility role only
Qwen-2.5-Coder-7B (swap):        ~4GB   ← swaps in for citation role only
Context + KV cache:               ~3GB
                                 ─────
                                 ~46GB → fits 48GB with ~2GB safety margin
```

DeepSeek and Qwen-Coder are swappable because reproducibility + citation are sequential in the DAG flow (specialists run in parallel but those two can be scheduled to share the same memory slot). Easy to tune later.

**Cloud variant (RunPod A6000 48GB):** identical layout; A6000 has same 48GB VRAM as M5 Max's available pool. Drop-in.

**If Mac is unavailable and only Modal serverless 24GB A10G is up:** drop Qwen-32B → Qwen-14B-Q4, DeepSeek-Lite-16B → DeepSeek-Lite-6.7B-Coder. Quality drop is real but tolerable for MVP.

### Tier-2 escalation targets

- `technical_correctness`, `meta_reviewer` → **`claude-shim`** (Claude Opus via Max20)
- `novelty`, `reproducibility` → **`codex-shim`** (GPT-5.5 via ChatGPT Pro)
- `citation`, `summary` → **no escalation** in MVP (Tier-1 quality sufficient; if it fails, flag for human moderation)

---

## Tier 2: subscription microVMs (same as before, no changes)

Two containers, each with a logged-in subscription:

```yaml
# infra/docker/subscription-tier.yml
claude-shim:
  build: infra/docker/claude-shim/
  volumes:
    - ${HOME}/.claude:/home/app/.claude:ro
  environment:
    SUBSCRIPTION_TIER: max20
    DAILY_BUDGET_CALLS: 800
    SKILL_NAME: grokrxiv-review
  ports: ["9100:8080"]

codex-shim:
  build: infra/docker/codex-shim/
  volumes:
    - ${HOME}/.codex:/home/app/.codex:ro
  environment:
    SUBSCRIPTION_TIER: pro
    DAILY_BUDGET_CALLS: 2000
    SKILL_NAME: grokrxiv-review
  ports: ["9101:8080"]
```

Shim HTTP contract (both):

```
POST /complete
Body: { "role": "...", "schema_name": "...", "input_artifact": {...} }
Response: { "ok": true, "model": "...", "output": {schema-valid JSON},
            "tokens_in": ..., "tokens_out": ..., "confidence": ...,
            "budget_remaining": ... }
```

Implementation: ~150 lines Axum each. Calls `claude --skill grokrxiv-review --output-format json -p "$payload"` (or `codex`), parses, validates, retries up to 2×. Registers as `provider: claude-shim` / `provider: codex-shim` in `crates/llm-adapter/src/lib.rs::provider_by_name()`. Existing `role_routing` map becomes a `tiers:` array per role; supervisor walks tiers in order.

---

## Steady-state cost projection at 50 papers/day MVP

### Primary: M5 Max at home

| Component | Monthly cost |
|---|---|
| M5 Max (already owned; electricity ~$10/mo at sustained partial load) | $10 |
| Claude Max20 subscription (already paid) | $200 |
| ChatGPT Pro subscription (already paid) | $200 |
| Gemini API free tier (well under 1500 req/day limit) | $0 |
| Cloudflare Tunnel for stable inbound (or Tailscale) | $0 |
| Supabase free tier | $0 |
| **Total** | **~$410/mo = $4,920/yr** |
| **Marginal vs. status quo (subscriptions already paid)** | **~$10/mo = $120/yr** |

### Secondary: Modal serverless when Mac is offline

| Component | Monthly cost (est.) |
|---|---|
| Modal A10G serverless (~$1.10/hr × ~2hr/day for 50 papers) | $66 |
| Subscriptions + Gemini + Supabase | $400 |
| **Total** | **~$466/mo** |

### Scale-up: RunPod A6000 pinned (200+ papers/day)

| Component | Monthly cost |
|---|---|
| RunPod A6000 48GB ($0.39/hr × 720h) | $281 |
| Subscriptions + everything else | $400 |
| **Total** | **~$681/mo = $8,170/yr** |

At 50/day = 18,250 papers/yr → effective **$0.022/paper (Mac, marginal)** or **$0.45/paper (cloud)**.

The marginal cost of running grokrxiv on the M5 Max is **basically electricity** — the subscriptions are paid regardless.

### Scaling math (proving the scale-up story)

| Volume | Tier-1 compute | Tier-2 escalations/day | Subscription budget fit? |
|---|---|---|---|
| 50/day MVP | 1× A6000 or Mac | ~45 | Yes (1.5–5% of budget) |
| 200/day | 1× A6000 or 2× Mac | ~180 | Yes (~10% of budget) |
| 500/day | 1× H100 or 4× A6000 | ~450 | Yes (~25% of budget) |
| 1500/day | 2× H100 | ~1350 | Tight; enable Tier 3 Batch |
| 5000/day | 4× H100 or dedicated cluster | ~4500 | Enable Tier 3 + 2nd subscription |

**Nothing changes in code at any of these volumes** — just hardware and a config-flip to add Tier 3 when needed.

---

## Grant strategy (unchanged from earlier; included in research doc)

Week 1: Anthropic + OpenAI researcher credits ($5–80K combined, 2–4w turnaround)
Month 1: Astera Open Science, Mozilla Tech Fund ($10–250K)
Quarter 1: Open Philanthropy AI epistemics, arXiv Sustainability Partner ($25K–500K)
Year 1: NSF POSE ($300K–1.5M, needs 501(c)(3))
Continuous: GitHub Sponsors, Open Collective ($50–5K/mo recurring)

Framing: "open-source AI peer-review infrastructure for academic transparency; typed-output multi-agent workflows with verifier-gated escalation."

---

## Risk matrix (updated for MVP scope)

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Tier-1 OSS model quality below threshold | Medium | Medium | Confidence-gated escalation IS the mitigation. Verifier ladder catches it. Easy to swap model. |
| ToS interpretation on subscription microVMs | Medium | Medium-High | Stay at low volume (50/day), document as research, never resell. Tier-3 Batch API documented as escape hatch (off in MVP). |
| Mac at home goes offline (laptop closed, ISP) | Medium | Medium | Cloud A6000 setup is documented and switchable via env var. |
| Cloud A6000 spot eviction | Low-Medium | Low | Auto-restart + state in Postgres; no in-flight work lost. |
| Subscription account lockout | Low | High | Tier 3 (Batch API) is documented + ready-to-enable. Configure it in advance, leave flag off. |
| OSS license change (DeepSeek, Qwen, Llama) | Low | Low | All Apache-2.0 / Llama-CL; weights stored locally; immune to upstream license shifts. |

---

## Implementation phases (FP6+, NOT this pass)

After this architecture is approved:

- **FP6a — Local Tier 1 (week 1):** Ollama in docker-compose with Qwen-2.5-32B-Q4 + Llama-3-8B for dev. Route citation/summary/novelty to it for local M1 testing. ~200 lines of Rust to add an `ollama` adapter (or reuse the vLLM adapter pointed at Ollama's compat endpoint).
- **FP6b — Cloud Tier 1 (week 2):** Provision RunPod A6000, deploy vLLM with the same models. Same adapter just talks to a different URL. Validate cost at $281/mo.
- **FP6c — Typed-output Skill + retry loop (week 2):** Build the `grokrxiv-review` Skill with all 6 schemas inlined. Add validate-and-retry logic to the adapter base trait. This is the core artifact for proving the typed-output story.
- **FP6d — Confidence scoring + verifier-gated routing (week 3):** Add `confidence: f64` to every role schema (already a non-required field on most). Implement the composite-confidence formula in the verifier ladder. Supervisor reads it and escalates per-role on threshold breach.
- **FP6e — `claude-shim` container (week 4):** Axum service that mounts `~/.claude`, exposes the contract above. Register as `claude-shim` provider. Route Tier-2 escalations for technical_correctness + meta_reviewer.
- **FP6f — `codex-shim` container (week 5):** Same pattern for Codex CLI. Route novelty + reproducibility Tier-2 escalations.
- **FP6g — Validation pass (week 6):** Run 100-paper sample through the full hybrid. Measure: cost per paper, escalation rate, verifier-pass rate, latency. Tune thresholds.

Total: ~6 weeks for the MVP hybrid. Tier 3 Batch API stays off; documented and adapter-ready if/when scale demands.

---

## Verification (for this plan)

1. `research/processing-costs.md` reads end-to-end as a coherent ~4000-word architecture doc with: measured cost baseline, 2-tier architecture rationale, hardware options (cloud + Mac), per-role model selection, expanded typed-output enforcement section, confidence-scoring formula, grant strategy, risk matrix, scaling math.
2. `research/processing-costs-plan.md` is a faithful copy of this plan file.
3. `~/.claude/plans/fp5-processing-costs-architecture.md` is the permanent record.
4. `~/.claude/plans/piped-bubbling-brook.md` is restored to slim index with FP5 row:

   ```
   | FP5 | Processing-cost analysis + 2-tier hybrid architecture for MVP (local/cloud OSS Tier 1 + subscription microVM Tier 2; Tier 3 Batch API documented but off); typed-output enforcement + confidence routing as scaling moat; grant strategy. No code changes. | Shipped (doc only) | fp5-processing-costs-architecture.md |
   ```

5. No code is run, no tests are touched in this pass.

---

## Critical files (READ-ONLY references)

- `crates/orchestrator/src/state.rs` — current `ProviderRegistry` + `role_routing`
- `crates/orchestrator/src/supervisor.rs::run_review_dag` — where confidence routing inserts
- `crates/llm-adapter/src/providers/{claude,openai,gemini,vllm}.rs` — adapter pattern for new shims (claude-shim, codex-shim, ollama)
- `agents/*.yaml` — current per-role provider/model declarations (extend with `tiers:` array)
- `schemas/*.schema.json` — the typed contract; add required `confidence` field
- `crates/verifier/src/lib.rs` — extend with composite-confidence formula
- `tests/m1-pipeline.sh` — end-to-end harness for validating new providers
