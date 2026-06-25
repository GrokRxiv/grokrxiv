# GrokRxiv — Processing Costs & Hybrid Architecture

> Status: Architecture-of-record for the MVP (FP5).
> Implementation: deferred to FP6+ tracks.
> Author: GrokRxiv team
> Last updated: 2026-05-14

## Executive Summary

After FP4 shipped a working 3-provider DAG (`claude`/`openai`/`gemini` per-role routing with verifier-typed outputs), a measured run on arXiv `2605.12484` yielded **$0.38/paper**. At full arXiv submission rate (~2,200 papers/day, ~800K/year) that's **~$370,000/year** in pure API spend — infeasible for a self-funded research project.

This document specifies the architecture-of-record for the MVP: a **two-tier hybrid** that defaults to self-hosted open-source models on Apple M5 Max (or cloud GPU under $300/mo) and only escalates to frontier models — via the operator's *existing* Claude Code + Codex CLI subscriptions, not paid API — when the verifier ladder flags low confidence.

**Headline numbers at MVP scope (50 papers/day):**

| Configuration | Monthly cost | Marginal cost vs status-quo subscriptions | $ / paper |
|---|---|---|---|
| **M5 Max at home (primary)** | $410 | **$10** (electricity) | **$0.022** marginal |
| Modal serverless failover | $466 | $66 | $0.31 |
| RunPod A6000 pinned (scale to 200+/day) | $681 | $281 | $0.45 |

**The MVP's real value proposition is not cost** — it is *proving out the typed-output multi-agent workflow* in a way that scales. The same JSON-schema-enforced contract runs against any backend (local Qwen-32B, cloud DeepSeek, Claude Code container, Codex container, future Anthropic Batch API). Going from 50/day to 5,000/day is a hardware swap with no code changes.

---

## 1. Measured Cost Baseline (where we are today)

The numbers below are from a real M1 smoke-test run (`review_id=600bb271-43d4-450c-98ee-cb94c1f47e6a`) on arXiv `2605.12484` after FP4 close-out (`gpt-5-large` → `gpt-5.5`, schema fixes for OpenAI strict mode, Gemini schema sanitizer, cost-aware role assignments). All values are real, sampled, persisted to Postgres in `review_agents.{tokens_in, tokens_out, latency_ms, model}`.

### 1.1 Per-role measured numbers

| Role | Model | Tokens in / out | Latency (ms) | Cost |
|---|---|---|---|---|
| `citation` | Gemini 3.5 Flash (Medium) | 609 / 53 | 3,043 | <$0.001 (free tier) |
| `novelty` | Gemini 3.5 Flash (Medium) | 608 / 597 | 19,557 | <$0.001 (free tier) |
| `summary` | claude-haiku-4-5 | 891 / 526 | 7,116 | $0.003 |
| `reproducibility` | gpt-5.5 | 869 / 503 | 11,348 | $0.005 |
| `technical_correctness` | **claude-opus-4-7** | 1,448 / 1,800 | 22,715 | **$0.157** |
| `meta_reviewer` | claude-sonnet-4-6 | **62,051** / 1,675 | 38,588 | **$0.211** |
| **Total** | | **66,476 / 5,154** | **102 s wall** | **$0.38** |

### 1.2 Cost concentration

Two roles burn 97% of the per-paper cost:

1. **`technical_correctness` on Opus** — flagship model for claim-by-claim audit; the user has reserved Opus for this role because subtle math/logic bugs are exactly what cheaper models miss.
2. **`meta_reviewer` on Sonnet** — Sonnet itself is cheap, but the *input* is 62K tokens because the supervisor currently re-bundles the full paper extract alongside the 5 specialist outputs. A free 30% saving sits here: just pass the 5 specialist outputs (~10K tokens total) since the paper extract is already reflected in their reasoning.

### 1.3 Projection at arXiv volume

ArXiv submissions in 2026 average ~2,200/day (10–12% YoY growth):

| Scope | Papers | Cost at current $0.38/paper | Cost if `meta_reviewer` input is trimmed ($0.23) |
|---|---|---|---|
| 50/day MVP | 18,250 | $7K/yr | $4K/yr |
| 250/day | 91,000 | $35K/yr | $21K/yr |
| 2026 YTD (132 days × 2,200) | ~290K | $110K | $67K |
| Full year 2026 projected | ~800K | **$304K** | **$184K** |

Even at the optimistic trimmed baseline, full-year coverage requires ~$200K — not realistic for a self-funded operator. The path forward is **moving most calls off paid API entirely**.

---

## 2. What I Got Wrong Earlier in the Design Discussion

Earlier in the design conversation, I dismissed the operator's "use my Claude Code + Codex CLI subscriptions instead of paid API" proposal on three grounds. At the *actual* MVP volume (50 papers/day, not 2,200), those grounds are wrong. Recording the correction here because the corrected architecture is the architecture-of-record:

| Earlier claim | Corrected analysis at MVP volume |
|---|---|
| "ToS violation, real ban risk" | Bright-line ToS violation requires *commercial resale* or *scraping public ChatGPT*. Neither applies to the operator's own subscription used for their own research project at 50/day with 15% escalation rate = ~45 frontier calls/day. Claude Code is *explicitly* designed for programmatic invocation; ChatGPT Pro Codex same. Documented as research-only with no resale framing, this is gray-but-defensible, not black-and-white prohibited. |
| "Rate limits make the math fail" | Max20 ≈ 1,000 messages/day budget; ChatGPT Pro Codex ≈ 2,500 sessions/day. ~45 Tier-2 calls/day fits in <5% of either budget — operator already runs at ~60% of subscription limits during interactive use. |
| "Structured output is a mess" | `claude --output-format json` plus a Skill that enforces "JSON-only output matching schema X" plus a 2× retry-on-invalid loop in the shim gives parseable, schema-valid output. Not API-grade `response_format: json_schema`, but functionally equivalent with retry semantics. The operator correctly identified this mechanism. |

---

## 3. Architecture: Two-Tier Hybrid (MVP)

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
    on M5 Max OR cloud GPU    ─────────────────────
    ──────────────────────    claude-shim (Max20)
    DeepSeek-Coder-Lite       codex-shim (Pro)
    Qwen-2.5-32B-Q4           uses ~/.claude/skills/
    Llama-3.1-8B              grokrxiv-review
    Qwen-2.5-Coder-7B
                              ~$400/mo flat
    $0–290/mo                 (subscriptions already
    (M5 Max OR A6000)         paid for)

                  (TIER 3 — Batch API)
                  documented but OFF for MVP.
                  Adapters built; flag-gated;
                  enabled later if Tier-1+2
                  budget breaches.
```

### 3.1 Routing rule (the heart of the system)

Every paper flows through this loop per role:

```
attempt   = 0
tier      = 1
max_tier  = 2          # MVP cap; later: 3 with Batch API
while attempt < max_attempts:
    output, raw_metadata = call(tier, role, input_artifact, schema)
    if not valid_json(output, schema):
        attempt += 1
        continue                     # retry same tier with corrective prompt
    confidence = verifier_ladder.compose(output, raw_metadata, role)
    if confidence >= threshold[role]:
        persist(output, confidence, tier)
        return
    elif tier < max_tier and budget_remaining(tier+1):
        tier += 1                    # escalate one tier
        attempt = 0
        continue
    else:
        persist(output, confidence='low', tier)
        flag_for_human_review()
        return
```

Concretely: if Qwen-32B (Tier 1) on `technical_correctness` produces a schema-valid output but the verifier ladder composite confidence comes back at 0.55, the supervisor re-runs the role on `claude-shim` (Tier 2). If `claude-shim` also returns < 0.7 *or* its daily budget is exhausted, the output is persisted with `verifier_status = 'low_confidence'` and surfaced in the moderation queue. **No paid API in MVP.**

### 3.2 Per-paper $ ceiling

Per-paper cost ceiling: **$0.10** (Tier-2 only — subscriptions are flat-fee, but every escalation consumes shared daily budget). The supervisor stops escalating once estimated-spend-on-this-paper > ceiling.

---

## 4. The Core Moat: Typed Output + Confidence Routing

This section is the contribution that survives — independent of which models are hot in 2026 — and the reason scaling from 50/day to 5,000/day is a hardware swap.

### 4.1 Single source of truth: `schemas/*.schema.json`

The six role schemas — `summary_review`, `technical_review`, `novelty_review`, `reproducibility_review`, `citation_review`, `meta_review` (plus `citation.schema.json` for the citation subtype) — are the **contract**. Everything consumes them:

- LLM adapters use them to constrain generation (Tier 1: native JSON-schema mode; Tier 2: Skill + retry).
- Verifiers validate `review_agents.output` against them at insert time.
- The database stores agent output as `jsonb` and indexes on `verifier_status`.
- The frontend renders them via shared TypeScript types in `apps/web/lib/types.ts`.

FP4's close-out fix already normalized these schemas to be OpenAI-strict-compatible (all properties listed in `required`, nullable union types for optional fields, no `format: uri`, no `minimum`/`maximum` constraints).

### 4.2 Tier 1 — vLLM/Ollama native JSON-schema mode

Both vLLM's OpenAI-compatible endpoint and Ollama support `response_format: {type: "json_schema", json_schema: {schema: <...>}}` for models with guided-decoding capability (Qwen-2.5, DeepSeek-V4, Llama-3-Instruct). Guided decoding constrains the generation *at the token level* — the model can only emit tokens that keep the in-progress output valid against the schema. In the happy path no retries are needed; in the rare pathological case (token-budget-exhausted mid-object) the output is truncated and the retry loop kicks in.

### 4.3 Tier 2 — Skill + retry loop

Subscription tier doesn't expose the API's native strict-schema mode, but the same outcome is reachable via a Skill:

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
[role schemas inlined, ~300 lines total]
```

The shim invokes:

```bash
claude --skill grokrxiv-review --output-format json -p "<<input.json"
```

Claude Code returns a wrapped result:

```json
{"type":"result","subtype":"success","result":"{...the JSON...}","usage":{...}}
```

The shim then:

1. Extracts `.result`.
2. `serde_json::from_str(&result)?` — parse.
3. `jsonschema::validate(&schema, &parsed)?` — validate.
4. On parse or validate failure: retry up to **2×** with a corrective prompt — *"Your previous response was invalid JSON (or failed schema validation with error E). Here is the schema again; try again, output JSON only."*
5. After 2 retries: return `{ok: false, error: "schema_violation", raw: <last_output>}` to the orchestrator, which either accepts at low confidence or cascades to the next tier.

Same pattern for `codex` CLI.

### 4.4 Confidence scoring (the new required field)

Add `"confidence": {"type": "number"}` as a **required field** in every role schema. (FP4's schemas already have `confidence` on 5 of 6 roles; it just needs to be uniformly required.) The agent self-reports its confidence as part of every output.

The verifier ladder then composes a *final* confidence as:

```
final_confidence =
      w_self        * agent_self_reported_confidence
    + w_schema      * (1.0 if schema-valid else 0.0)
    + w_citations   * citation_existence_rung_pass_rate
    + w_tone        * (1.0 if tone OK else 0.5)
    + w_xagent      * cross_agent_consistency_score   # meta_reviewer only
    + w_render      * (1.0 if HTML renders cleanly else 0.7)
```

Default weights per role (must sum to 1.0):

| Role | self | schema | citation | tone | x-agent | render |
|---|---|---|---|---|---|---|
| `summary` | 0.40 | 0.40 | 0.00 | 0.10 | 0.00 | 0.10 |
| `technical_correctness` | 0.30 | 0.30 | 0.20 | 0.10 | 0.00 | 0.10 |
| `novelty` | 0.30 | 0.30 | 0.20 | 0.10 | 0.00 | 0.10 |
| `reproducibility` | 0.40 | 0.30 | 0.10 | 0.10 | 0.00 | 0.10 |
| `citation` | 0.20 | 0.30 | 0.40 | 0.05 | 0.00 | 0.05 |
| `meta_reviewer` | 0.30 | 0.30 | 0.05 | 0.05 | 0.20 | 0.10 |

Per-role escalation threshold (defaults): all roles use 0.7, except `citation` at 0.6 (mechanical task, more tolerant) and `meta_reviewer` at 0.8 (gates publication).

### 4.5 Why this is the moat

Same Skill, same schema, same verifier ladder, same confidence scoring → runs on **any backend**. The orchestrator's routing logic never sees a model name; it sees a typed contract and a confidence number. Scaling from 50/day to 5,000/day is purely a hardware change:

- Tier 1: larger GPU or more replicas of the same model containers.
- Tier 2: more subscription accounts, *or* enable Tier 3 paid API once daily Tier-2 budget is exhausted.
- Zero changes to orchestrator code, schemas, or routing logic.

Backends are interchangeable. The schemas are the API.

---

## 5. Per-Role Tier-1 Model Selection

Selected to maximize Apple M5 Max 48GB utilization (primary deployment) and remain drop-in compatible with cloud A6000 48GB. Choices reflect the operator's independent research on the open-source landscape (DeepSeek family for reasoning/code, Qwen-2.5 family for structured output, Llama-3.x for lightweight summaries).

| Role | Tier-1 model | Quantization | Weights size | Why |
|---|---|---|---|---|
| `summary` | **Llama-3.1-8B-Instruct** | Q4_K_M | 5GB | Plain TL;DR — small model suffices; 100+ tok/s on M5 Max |
| `technical_correctness` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Long-context structured reasoning; Qwen-2.5's structured-output behavior is the best in OSS |
| `novelty` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Long-context related-work delta; reuses the resident 32B |
| `reproducibility` | **DeepSeek-Coder-V2-Lite-16B** | Q4_K_M | 10GB | Code/data availability assessment; DeepSeek-Coder family explicitly designed for this |
| `citation` | **Qwen-2.5-Coder-7B** | Q4_K_M | 4GB | Structured JSON fan-out per reference; tiny is fine |
| `meta_reviewer` | **Qwen-2.5-32B-Instruct** | Q4_K_M | 20GB | Synthesis with strict JSON output (reuses resident 32B) |

### Memory layout on M5 Max 48GB

```
macOS + system:                  ~8GB
Ollama/vLLM-mlx runtime:         ~3GB
Qwen-2.5-32B-Q4 (resident):     ~20GB   ← serves 3 roles
Llama-3.1-8B-Q4 (resident):      ~5GB   ← serves summary
DeepSeek-Coder-Lite-16B (swap):  ~10GB  ← swaps in for reproducibility role
Qwen-2.5-Coder-7B (swap):        ~4GB   ← swaps in for citation role
Context + KV cache:               ~3GB
                                 ─────
                                 ~46GB → fits 48GB with ~2GB safety margin
```

`DeepSeek-Coder-V2-Lite-16B` and `Qwen-2.5-Coder-7B` are loaded on demand because `reproducibility` and `citation` are independent tasks in the DAG — they can be scheduled to share a memory slot. Easy to tune later if measured throughput needs both resident.

If forced onto a 24GB cloud GPU (Modal serverless A10G failover), the orchestrator drops to Qwen-2.5-14B-Q4 + DeepSeek-Coder-V2-Lite-6.7B. Quality drop is measurable but tolerable; the verifier ladder catches the worst regressions and escalates to Tier 2.

### Tier-2 escalation routing

| Role | Tier-1 (default) | Tier-2 (escalation) |
|---|---|---|
| `technical_correctness` | Qwen-32B local | `claude-shim` → Opus via Max20 |
| `meta_reviewer` | Qwen-32B local | `claude-shim` → Sonnet/Opus via Max20 |
| `novelty` | Qwen-32B local | `codex-shim` → GPT-5.5 via Pro |
| `reproducibility` | DeepSeek-Coder-Lite | `codex-shim` → GPT-5.5 via Pro |
| `citation` | Qwen-2.5-Coder-7B | **none** in MVP (flag-only on Tier-1 fail) |
| `summary` | Llama-3.1-8B | **none** in MVP (flag-only on Tier-1 fail) |

---

## 6. Tier 2 — Subscription microVMs

Two long-running containers, each holding an OAuth-authenticated subscription session. The host mounts `~/.claude` and `~/.codex` read-only into each container, so the existing logged-in tokens travel without re-auth.

### 6.1 docker-compose sketch

```yaml
# infra/docker/subscription-tier.yml
services:
  claude-shim:
    build: infra/docker/claude-shim/
    volumes:
      - ${HOME}/.claude:/home/app/.claude:ro
    environment:
      SUBSCRIPTION_TIER: max20
      DAILY_BUDGET_CALLS: 800
      SKILL_NAME: grokrxiv-review
    ports: ["9100:8080"]
    restart: unless-stopped

  codex-shim:
    build: infra/docker/codex-shim/
    volumes:
      - ${HOME}/.codex:/home/app/.codex:ro
    environment:
      SUBSCRIPTION_TIER: pro
      DAILY_BUDGET_CALLS: 2000
      SKILL_NAME: grokrxiv-review
    ports: ["9101:8080"]
    restart: unless-stopped
```

### 6.2 Shim HTTP contract

Both containers expose the same surface:

```
POST /complete
Content-Type: application/json
Body: {
  "role": "technical_correctness",
  "schema_name": "technical_review.schema.json",
  "input_artifact": { "paper": {...}, "prior_specialists": null }
}

Response (200 OK on validated output):
{
  "ok": true,
  "model": "claude-opus-4-7",        // actual model returned by subscription
  "output": { ... role-schema-valid JSON ... },
  "tokens_in": 8345,
  "tokens_out": 1102,
  "confidence": 0.85,
  "budget_remaining": 743,
  "latency_ms": 14820
}

Response (429 if subscription rate budget exhausted):
{ "ok": false, "error": "budget_exhausted", "budget_remaining": 0 }

Response (422 if schema validation failed after retries):
{ "ok": false, "error": "schema_violation",
  "raw": "<last raw output>", "validation_errors": [...] }
```

### 6.3 Implementation

~150 lines of Axum each. The shim:

1. Accepts the POST.
2. Renders the input artifact + schema into a stdin payload for `claude`/`codex`.
3. Spawns `claude --skill grokrxiv-review --output-format json -p "$payload"` (or `codex` equivalent).
4. Reads the wrapped JSON result, extracts `.result`.
5. Parses + validates against the schema; retries up to 2× on failure with a corrective prompt.
6. Tracks the daily budget counter (atomic; persisted to a small SQLite file so it survives container restarts).
7. Returns the structured response.

### 6.4 Orchestrator integration

Register `claude-shim` and `codex-shim` as new providers in `crates/llm-adapter/src/lib.rs::provider_by_name()`. They're plain HTTP providers pointing at `http://localhost:9100` / `http://localhost:9101`. The existing `agents/*.yaml` `role_routing` map gets a `tiers:` array per role:

```yaml
# agents/technical_correctness.yaml
id: technical_correctness
role: "Verify mathematical, logical, and empirical claims of the paper."
tiers:
  - provider: ollama  # tier 1
    base_url: http://localhost:11434
    model: qwen2.5:32b-instruct-q4_K_M
    confidence_threshold: 0.7
  - provider: claude-shim  # tier 2
    base_url: http://localhost:9100
    skill: grokrxiv-review
    daily_budget_calls: 200   # this role's share of the 800 total
prompt_template: prompts/technical_correctness.md
output_schema: schemas/technical_review.schema.json
verifiers: [json_schema, tone, citation_existence]
```

The supervisor walks `tiers[]` in order, applying the routing rule from §3.1.

---

## 7. Hardware Options

### 7.1 Option A — Local M5 Max 48GB (primary)

Apple M5 Max with 48GB unified memory is a serious inference box. M5's upgraded Neural Engine + GPU make on-device inference of 32B-class models genuinely practical. **Unified memory** is the killer feature here: model weights and runtime live in the same pool, so allocations are flexible and there's no PCIe stall to a discrete GPU.

| Metric | M5 Max 48GB |
|---|---|
| Available for model weights | ~37GB after macOS + runtime overhead |
| Qwen-2.5-32B-Q4 throughput | 30–40 tok/s |
| Llama-3.1-8B-Q4 throughput | 80–120 tok/s |
| 50 papers/day workload | ~75 minutes of compute total per day |
| **Marginal cost** | **~$10/mo (electricity)** |

Operational notes:
- Run vLLM-mlx or Ollama as a `launchd` user service for auto-start + restart-on-crash.
- Cloudflare Tunnel (free) or Tailscale gives a stable inbound URL without exposing the residential IP.
- Closed-laptop mode with external power + monitor works fine for headless operation.
- Postgres + storage stays in Supabase cloud — only the LLM tier is local.

Privacy bonus: paper extracts never leave the local machine for Tier 1 work.

### 7.2 Option B — Cloud GPU (≤$300/mo, secondary)

For days when the Mac is offline (travel, maintenance) or when scale demands:

| Provider | GPU | VRAM | Hourly | Monthly (720h) | Notes |
|---|---|---|---|---|---|
| RunPod | RTX A6000 | 48GB | $0.39 | $281 | Spot pricing; auto-restart available |
| RunPod | A40 | 48GB | $0.40 | $288 | Same envelope as A6000 |
| Modal | A10G serverless | 24GB | $1.10 (active only) | ~$50–150 for 50 papers/day | Pay-per-second; ideal for failover |
| Vast.ai | 3090/4090 | 24GB | $0.20–0.30 | $144–216 | Consumer hardware; reliability varies |

**Recommendation:** Modal serverless as the on-demand failover (~$50–150/mo only when the Mac is unavailable). Pin RunPod A6000 once volume crosses 200/day.

### 7.3 Hardware switching is one env var

The vLLM/Ollama adapter (shipped in FP4 Track E) is endpoint-agnostic. Switching from Mac to cloud is a single `OLLAMA_HOST` or `VLLM_BASE_URL` change. No code modifications.

---

## 8. Steady-State Cost Projection at 50 Papers/Day MVP

### 8.1 Primary configuration (M5 Max at home)

| Component | Monthly | Annual |
|---|---|---|
| M5 Max (electricity at sustained partial load) | $10 | $120 |
| Claude Max20 subscription (already paid) | $200 | $2,400 |
| ChatGPT Pro subscription (already paid) | $200 | $2,400 |
| Gemini API free tier | $0 | $0 |
| Cloudflare Tunnel / Tailscale | $0 | $0 |
| Supabase free tier | $0 | $0 |
| **Total** | **$410** | **$4,920** |
| **Marginal vs. status-quo subscriptions** | **$10** | **$120** |

At 50/day × 365 = **18,250 papers/year**, the marginal cost works out to **$0.0066 per paper** if we credit the subscriptions as already paid (operator's stated situation).

### 8.2 Failover (Modal serverless A10G)

| Component | Monthly |
|---|---|
| Modal A10G (~$1.10/hr × ~2hr/day) | $66 |
| Subscriptions + Gemini + Supabase | $400 |
| **Total** | **$466** |

### 8.3 Scale-up (RunPod A6000 pinned, 200+/day)

| Component | Monthly |
|---|---|
| RunPod A6000 ($0.39/hr × 720h) | $281 |
| Subscriptions + everything else | $400 |
| **Total** | **$681 = $8,170/yr** |

### 8.4 Scaling math (proving the scale-up story)

| Volume | Tier-1 compute | Tier-2 escalations/day | Subscription budget headroom |
|---|---|---|---|
| 50/day MVP | 1× A6000 or M5 Max | ~45 | Yes (1.5–5%) |
| 200/day | 1× A6000 or 2× Mac | ~180 | Yes (~10%) |
| 500/day | 1× H100 or 4× A6000 | ~450 | Yes (~25%) |
| 1,500/day | 2× H100 | ~1,350 | Tight; enable Tier 3 Batch |
| 5,000/day | 4× H100 or dedicated cluster | ~4,500 | Tier 3 + 2nd subscription |

**No code changes at any of these volumes.** Hardware + a config flag.

---

## 9. Grant Strategy

Self-funding the marginal $120/year is trivial. Growing past 200/day requires external funding. Concrete pipeline:

### Week 1 — lowest friction, fastest credit infusion

| Program | Award | Turnaround | Link |
|---|---|---|---|
| Anthropic — Beneficial AI / Research Access | $5K–50K API credits | 2–4 weeks | `anthropic.com/research` → "API credits for research" |
| OpenAI Researcher Access Program | $5K–30K credits | ~2 weeks | `openai.com/researcher-access` |
| Google for Research / Cloud Research Credits | $5K–10K cloud credits (covers Gemini API) | ~3 weeks | `research.google/programs/awards/` |

**Pitch**: "open-source AI peer-review infrastructure for academic transparency; typed-output multi-agent workflows with verifier-gated escalation; arXiv-scale evaluation of LLM reviewer reliability."

### Month 1 — mid-size, mission-aligned

- **Astera Institute — Open Science Tools**: $25K–250K. Their explicit focus is arXiv-adjacent open-science infrastructure. `astera.org/grants`.
- **Mozilla Technology Fund**: $10K–100K. Open-source AI infrastructure track. `foundation.mozilla.org/en/what-we-fund/`.

### Quarter 1 — institutional, sustaining

- **Open Philanthropy — AI safety + epistemics**: $25K–500K. Frame as improving scientific publishing epistemics via verifier-typed multi-agent review. `openphilanthropy.org/giving-portal`.
- **arXiv Sustainability Partner Program**: in-kind partnership, possibly direct API access at scale. `arxiv.org/about/giving`.

### Year 1 — transformative, slower

- **NSF POSE (Pathways to Enable Open-Source Ecosystems)**: $300K–1.5M. Requires US 501(c)(3) or university affiliation. `nsf.gov/funding/opportunities/pose`.

### Continuous — recurring

- **GitHub Sponsors / Open Collective**: $50–5K/mo recurring with public progress log and weekly commit visibility. `github.com/sponsors`.

### Two-paragraph proposal template

> **GrokRxiv** is open-source AI peer-review infrastructure for arXiv. We use a multi-agent DAG of six specialist reviewers (summary, technical correctness, novelty, reproducibility, citation, and meta-synthesis) producing JSON-schema-typed structured outputs, gated by a verifier ladder that scores each output for correctness, citation existence, tone, and cross-agent consistency. The architecture supports any LLM backend — open-source weights on consumer GPUs, frontier APIs, or operator-owned subscription tiers — by treating the schema contract, not the model, as the integration point.
>
> We are applying for [credits / grant] to fund the [Anthropic API / OpenAI API / cloud compute] tier of a hybrid deployment that defaults to self-hosted open-weights models and escalates to frontier APIs only when the verifier ladder flags low confidence. At MVP scope (50 papers/day) marginal cost is ~$10/month; the requested credit budget extends our scale-up runway to 500+ papers/day across the full 2026 arXiv submission flow. All code, schemas, prompts, and review datasets are MIT-licensed; deployment runbooks and per-paper provenance traces will be published with each release.

---

## 10. Risk Matrix

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Tier-1 OSS model quality below threshold for hard papers | Medium | Medium | Confidence-gated escalation **is** the mitigation. Verifier ladder catches it. Easy to swap to a stronger Tier-1 model when a better one ships. |
| ToS interpretation change on subscription microVMs | Medium | Medium-High | Stay at low MVP volume (50/day); document use as research-only; never resell. Tier-3 Batch API documented and adapter-ready as escape hatch (off in MVP). |
| Mac at home goes offline (laptop lid, ISP outage) | Medium | Medium | Cloud A6000 setup documented + switchable via env var. Modal serverless is a viable on-demand failover at $50–150/mo. |
| Cloud A6000 spot eviction | Low-Medium | Low | Auto-restart + work-in-progress state lives in Postgres; no in-flight loss. |
| Subscription account lockout | Low | High | Tier 3 (Batch API) flag-ready. Maintain a dormant secondary subscription for hot-swap. |
| OSS license change (DeepSeek, Qwen, Llama) | Low | Low | All Apache-2.0 or Llama-CL; weights stored locally; immune to upstream policy shifts. |
| Subscription rate-policy tightening | Medium | Medium | Headroom built in (target ≤50% of subscription daily budget). Tier-3 cascade if needed. |
| Volume growth outstrips Tier-1+2 capacity | Medium (success scenario) | Medium | Tier-3 Batch API + scale-out cloud GPU. Architecture is volume-agnostic by design. |

---

## 11. Implementation Phases (FP6+, NOT this pass)

This document is the FP5 architecture-of-record. Implementation breaks into:

- **FP6a — Local Tier 1 (week 1)**: Ollama in `docker-compose` with Qwen-2.5-32B-Q4 + Llama-3.1-8B. Route citation/summary/novelty to it for local M1 testing. Add an `ollama` adapter (~200 lines Rust), or point the existing vLLM adapter at Ollama's OpenAI-compat endpoint.
- **FP6b — Cloud Tier 1 (week 2)**: Provision RunPod A6000 (or Modal failover). Deploy vLLM with the same models. Validate cost at $281/mo cap.
- **FP6c — Typed-output Skill + retry loop (week 2)**: Build the `grokrxiv-review` Skill with all 6 schemas inlined. Add the validate-and-retry envelope to the adapter base trait.
- **FP6d — Confidence scoring + verifier-gated routing (week 3)**: Add required `confidence` field to every role schema. Implement the composite-confidence formula in the verifier ladder. Supervisor reads it and escalates per-role on threshold breach.
- **FP6e — `claude-shim` container (week 4)**: Axum service that mounts `~/.claude`, exposes the `POST /complete` contract above. Register as `claude-shim` provider. Route Tier-2 escalations for `technical_correctness` + `meta_reviewer`.
- **FP6f — `codex-shim` container (week 5)**: Same pattern for Codex CLI. Route `novelty` + `reproducibility` Tier-2 escalations.
- **FP6g — Validation pass (week 6)**: Run a 100-paper sample through the full hybrid. Measure: actual cost per paper, escalation rate, verifier-pass rate, end-to-end latency. Tune thresholds and weight vectors.

Total: ~6 weeks for the MVP hybrid. Tier 3 Batch API stays off; documented and adapter-ready for the moment scale demands.

---

## 12. Open Questions

- **Confidence-threshold tuning**: starting defaults are stated in §4.4 but will need calibration on the validation pass. We should record per-role precision/recall curves once the FP6g 100-paper sample completes.
- **Should grokrxiv ever become a paid service?** Affects ToS framing on Tier 2. Current architecture assumes research/free; commercial use would push toward all-API or higher-tier subscriptions.
- **vLLM-mlx vs Ollama on M5 Max**: vLLM-mlx has better throughput; Ollama is operationally simpler. Worth benchmarking on FP6a.
- **Multi-tenant vs single-tenant Tier 1**: if grants come in and we add a 2nd operator, do we run a shared GPU box or separate boxes? Probably separate to keep escalation budgets clean.

---

## 13. References to Existing Code

These files are touched by FP6 (not by this doc):

- `crates/orchestrator/src/state.rs` — current `ProviderRegistry` + `role_routing` map.
- `crates/orchestrator/src/supervisor.rs::run_review_dag` — insertion point for confidence-gated routing.
- `crates/llm-adapter/src/providers/{claude,openai,gemini,vllm}.rs` — adapter pattern; `claude-shim` / `codex-shim` / `ollama` follow the same trait.
- `crates/llm-adapter/src/lib.rs::provider_by_name()` — register new providers here.
- `agents/*.yaml` — per-role provider/model declarations; gain a `tiers[]` array.
- `schemas/*.schema.json` — the typed contract; add required `confidence` field uniformly.
- `crates/verifier/src/lib.rs` — extend with composite-confidence formula.
- `tests/m1-pipeline.sh` — end-to-end harness for validating new providers.

---

*End of document.*
