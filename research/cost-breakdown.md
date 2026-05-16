# GrokRxiv — Cost Breakdown Analysis

> Captured from conversation on 2026-05-15. Companion to `research/processing-costs.md`.

## Measured cost: $0.38/paper

Single real run on arXiv `2605.12484` after FP4 close-out.

| Role | Model | Tokens in | Tokens out | Cost | % of total |
|---|---|---|---|---|---|
| `citation` | gemini-2.5-flash | 609 | 53 | <$0.001 | ~0% |
| `novelty` | gemini-2.5-flash | 608 | 597 | <$0.001 | ~0% |
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
