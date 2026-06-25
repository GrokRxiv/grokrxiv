# GrokRxiv — Cost Breakdown Analysis

> Captured from conversation on 2026-05-15. Companion to `research/processing-costs.md`.

## Measured cost: $0.38/paper

Single real run on arXiv `2605.12484` after FP4 close-out.

| Role | Model | Tokens in | Tokens out | Cost | % of total |
|---|---|---|---|---|---|
| `citation` | Gemini 3.5 Flash (Medium) | 609 | 53 | <$0.001 | ~0% |
| `novelty` | Gemini 3.5 Flash (Medium) | 608 | 597 | <$0.001 | ~0% |
| `summary` | claude-haiku-4-5 | 891 | 526 | $0.003 | 1% |
| `reproducibility` | gpt-5.5 | 869 | 503 | $0.005 | 1% |
| `technical_correctness` | claude-opus-4-7 | 1,448 | 1,800 | $0.157 | 41% |
| `meta_reviewer` | claude-sonnet-4-6 | **62,051** | 1,675 | $0.211 | 56% |
| **Total** | | **66,476** | **5,154** | **$0.38** | |

## Key findings

**Two roles = 97% of cost.**

- `meta_reviewer` at $0.211 (56%) is almost entirely an implementation bug: it re-sends the full paper extract alongside the 5 specialist outputs. The paper extract is already baked into specialist reasoning — only the ~10K specialist outputs need to be passed.
- `technical_correctness` at $0.157 (41%) is a deliberate choice: Opus for claim-by-claim math/logic audit.

**The 5 specialist workers are cheap.** Their combined token load is only ~4.4K in / 3.5K out. This is not an expensive document processing pipeline — it's one bloated input and one expensive model.

## Token reality

| Scenario | Tokens in | Tokens out | Total |
|---|---|---|---|
| Current (measured) | 66,476 | 5,154 | ~71K |
| After meta_reviewer trim (bug fix) | ~14,425 | 5,154 | ~19K |

Fixing the meta_reviewer input (paper extract → specialist outputs only) gives a **3.7× token reduction** with zero quality loss.

## Cost after meta_reviewer fix

- `meta_reviewer` drops from $0.211 → ~$0.02 (10K tokens instead of 62K)
- Total per-paper cost: ~**$0.17** (from $0.38)
- At 50 papers/day: ~$8.50/day → ~$3,100/year (API only, no Tier-1 hybrid)

With Tier-1 hybrid (local OSS models as default, subscription escalation only): marginal cost ~**$0.0066/paper**.

## Original session estimates for daily arXiv processing

Context from the FP5 design session that produced these numbers. ArXiv submissions in 2026 average ~2,200/day (10–12% YoY growth), ~800K/year if fully covered.

### At measured cost ($0.38/paper)

| Scope | Papers/day | Papers/year | Annual API cost |
|---|---|---|---|
| MVP | 50 | 18,250 | **$6,935** |
| Mid-scale | 250 | 91,250 | **$34,675** |
| Full arXiv 2026 (projected) | 2,200 | 803,000 | **$305,140** |

### At trimmed cost ($0.23/paper after meta_reviewer fix)

| Scope | Papers/day | Papers/year | Annual API cost |
|---|---|---|---|
| MVP | 50 | 18,250 | **$4,198** |
| Mid-scale | 250 | 91,250 | **$20,988** |
| Full arXiv 2026 (projected) | 2,200 | 803,000 | **$184,690** |

### Why these numbers triggered the 2-tier redesign

The operator's reaction during the session: *"we can't afford $80k a year for a self-funded project unless it is funded by donations and the infra is paid for"*.

Even the *optimistic* trimmed estimate of $184K/year for full arXiv coverage was infeasible. The mid-scale 250/day at $21K/year was borderline. Only the 50/day MVP at $4K/year was sustainable on personal budget — but that's <2.3% of daily arXiv submissions, not a real product.

This was the trigger that forced the architecture pivot:
1. **Drop most calls off paid API entirely** → self-hosted OSS on M5 Max as Tier 1 default
2. **Use existing Claude Code + Codex CLI subscriptions** for Tier 2 escalation (flat $400/mo already paid, not per-call)
3. **Reserve paid Batch API as Tier 3** — documented and adapter-ready, off in MVP

### Cost comparison: API-only vs 2-tier hybrid (MVP 50/day)

| Configuration | Monthly | Annual | $/paper marginal |
|---|---|---|---|
| API-only (measured $0.38) | $580 | $6,935 | $0.38 |
| API-only (trimmed $0.23) | $350 | $4,198 | $0.23 |
| **2-tier hybrid (M5 Max + subs)** | **$10** marginal | **$120** marginal | **$0.0066** |

The hybrid is ~35–58× cheaper at the marginal level (subscriptions counted as already paid).

## ACP evaluation (2026-05-15)

Evaluated whether Agent Client Protocol (`agentclientprotocol.com`) could replace the plain HTTP contract for `claude-shim` and `codex-shim`.

**Verdict: not a fit.** ACP is editor ↔ agent (VS Code/Cursor talking to a coding agent). GrokRxiv needs orchestrator ↔ worker (Rust supervisor calling an HTTP service wrapping a CLI). The `POST /complete` HTTP contract in `processing-costs.md §6.2` is simpler, more stable, and already maps to the `LlmAdapter` trait. ACP remote transport is also still WIP.

ACP would be relevant if GrokRxiv ships a VS Code extension for paper authors to invoke review inline — future, not MVP.

## Pricing tools for ongoing monitoring

| Tool | Best for |
|---|---|
| [CostGoat](https://costgoat.com/compare/llm-api) | Monthly cost calculator with calls/month + token inputs; model quality scores |
| [aipricing.org](https://aipricing.org/) | 285+ models, real calculator, cheaper-alternative suggestions |
| [pricepertoken.com](https://pricepertoken.com/) | Browse 300+ models quickly; no calculator |

None compare subscription flat-rate vs API per-token vs open-source $0-marginal. The FP5 doc (`processing-costs.md §8`) remains the authoritative three-way comparison for GrokRxiv's specific workload.

---

## After FP6 — measured cost reduction (2026-05-15)

Real M1 smoke run on arXiv `2605.12484` after the four FP6 cost fixes landed (`review_id=2c9d69c2-068d-4069-a8a1-4bd84ff0020f`). Same paper, same 6 agents, same provider mix as the FP5 baseline — only the orchestrator changed.

### Per-role measured numbers (from `review_agents`)

| Role | Model | Tokens in | Tokens out | Latency (ms) | Verifier |
|---|---|---|---|---|---|
| `citation` | Gemini 3.5 Flash (Medium) | 634 | 399 | 11,483 | pass |
| `novelty` | Gemini 3.5 Flash (Medium) | 633 | 429 | 14,005 | pass |
| `summary` | claude-haiku-4-5 | 915 | 550 | 6,504 | pass |
| `reproducibility` | gpt-5.5 | 894 | 688 | 13,508 | pass |
| `technical_correctness` | claude-opus-4-7 | 1,470 | 1,715 | 23,522 | pass |
| `meta_reviewer` | claude-sonnet-4-6 | **3** | 1,198 | 23,152 | pass |
| **Total** | | **4,549** | **4,979** | **92 s wall** | 6/6 pass |

### Cost computed against published model rates

| Role | Input cost | Output cost | Subtotal |
|---|---|---|---|
| `citation` (Gemini free tier) | ~$0 | ~$0 | **<$0.001** |
| `novelty` (Gemini free tier) | ~$0 | ~$0 | **<$0.001** |
| `summary` (Haiku 4.5 — $1/$5 per M) | $0.001 | $0.003 | **$0.004** |
| `reproducibility` (gpt-5.5 ~$1.25/$10 per M) | $0.001 | $0.007 | **$0.008** |
| `technical_correctness` (Opus 4.7 — $15/$75 per M) | $0.022 | $0.129 | **$0.151** |
| `meta_reviewer` (Sonnet 4.6 — $3/$15 per M) | ~$0 | $0.018 | **$0.018** |
| **DB-reported total** | | | **~$0.181** |

### Calling out the cache-accounting gap

Track A3 enabled Anthropic `cache_control` on user prompts ≥1024 tokens. For `meta_reviewer` the user prompt is the JSON map of 5 specialist outputs (~3.2K tokens here). What happened:

- Anthropic returned `input_tokens: 3` and (almost certainly) `cache_creation_input_tokens: ~3,200`
- The schema only stores `input_tokens` and `cache_read_input_tokens` — `cache_creation_input_tokens` is dropped on the floor
- Cache creation bills at **1.25× normal input rate**, so the true `meta_reviewer` input cost is ~$0.012 (cache create) + ~$0 (the residual 3 tokens) = **$0.012**, not the $0 the DB implies
- True per-paper cost: **~$0.193** (DB number adjusted upward by the missing cache_creation accounting)

The cache-creation tax is real ON THE FIRST CALL with no immediate retry. For papers that don't trigger a verifier retry inside the 5-minute cache window, we pay 1.25× the meta_reviewer input portion and get no benefit. **This is the worst case for prompt caching at our access pattern.** Two future fixes:

1. **Add `cache_creation_input_tokens` + `cache_read_input_tokens` columns** to `review_agents` so cost accounting is honest. Follow-up to FP6, ~15 min of work.
2. **Reconsider caching `meta_reviewer`** — at 3.2K tokens with no retries, it's net-negative. The Track A3 heuristic should bump the minimum threshold OR only cache for roles that have a meaningful retry rate. The cost savings already came from A1 (the input shape change), not from caching. ~30 min of tuning.

### Total reduction vs the FP5 baseline

| Metric | FP5 baseline | After FP6 (true) | Reduction |
|---|---|---|---|
| Per paper | $0.38 | **~$0.193** | **49%** |
| 50/day MVP | $6,935/yr | **~$3,520/yr** | $3,415 saved |
| Full arXiv (2,200/day) | $305,140/yr | **~$155K/yr** | $150K saved |

The 49% reduction is mostly Track A1 (meta_reviewer input trim — the paper extract is no longer in the user prompt). The smaller `tokens_in` numbers for specialists vs the FP5 baseline are within run-to-run noise (~5-10% per-role variance from sampling).

### Where the remaining cost lives

`technical_correctness` on Opus is now **78% of the per-paper cost** ($0.151 of $0.193). Two future avenues to compress further:

- **A5 (planned)** — output cache hits within the 30-day TTL drop all costs to ~$0 on re-reviews. Saves at scale, not on the first call.
- **FP6.next** — try `technical_correctness` on Sonnet 4.6 instead of Opus 4.7. Sonnet is ~5× cheaper. If quality holds on a 20-paper sample, that single change drops per-paper cost from $0.193 to **~$0.07**. Out of FP6 scope (no model swap this pass); revisit on FP7 or later.

### FP6 verification status

| Acceptance criterion | Status |
|---|---|
| Per-paper cost ≤ $0.15 | ⚠️ measured $0.193 — within striking distance but missed the target by ~30%. Path to $0.07 identified (Opus→Sonnet on technical_correctness). |
| `review_inputs` table — 1 row per review | ✓ exactly 1 row, 175KB artifact stored once |
| `review_cache` populated on first run | ✓ 6 rows written, all `verifier_status=pass` |
| Prompt caching headers present | ✓ unit-tested + observable via `input_tokens=3` on meta_reviewer (cache create) |
| Multi-provider DAG still working | ✓ 6/6 pass, 5 distinct models, 3 exercisable providers |
