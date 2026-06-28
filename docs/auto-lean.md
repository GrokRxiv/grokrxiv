# Auto-Lean Extraction Findings

Scope: how GrokRxiv currently gets from paper source to Lean proof status,
whether that is enough for an MVP, and where a later local paper-to-Lean model
should fit.

## Bottom Line

The current extraction -> LLM -> Lean path is good enough for an MVP only if the
product claim is:

> GrokRxiv makes an honest Lean proof attempt for paper-derived theorem
> statements and reports `PROVED`, `NOT_PROVED`, `FAILED`, or
> `NOT_CONDUCIVE_TO_LEAN_PROOF`.

It is not good enough to claim:

> GrokRxiv automatically extracts and verifies arbitrary research-paper proofs
> from arXiv.

That stronger claim needs a dedicated autoformalization/prover stack later:
typed statement transcription, proof-sketch extraction, retrieval over mathlib
and paper-local definitions, Lean server feedback, and likely a local prover
model or local proof-search service.

## What We Extract Today

Current paper math flow:

1. Source ingestion normalizes arXiv source/PDF output into `body.md`,
   `equations.json`, `references.json`, and `theorem_graph.json`.
2. The theorem extraction agent scans theorem-like blocks:
   `theorem`, `lemma`, `proposition`, `corollary`, `definition`, `proof`,
   `remark`, and `example`.
3. For theorem/lemma/proposition/corollary entries, the schema can carry:
   `statement`, `source_tex`, `typed_transcription`, and `theorem_ir`.
4. Proof blocks are extracted as graph evidence and dependency edges through
   `depends_on`; they are not translated into Lean proof scripts.
5. `semantic_ir.json` selects formal theorem candidates from
   `theorem_graph.json`, keeps equations as supporting context, and keeps
   review prose/nonformal material out of Lean targets.
6. `proof_obligations.json` turns selected theorem candidates into Lean
   obligations.
7. The Lean author loop runs one theorem at a time. The LLM writes a complete
   Lean file, then `lake env lean` is the proof authority. `sorry`, `admit`,
   and `axiom` are forbidden.
8. Kernel-accepted Lean code is recorded as `PROVED`; failed or unproved code
   is recorded as `NOT_PROVED`/`FAILED` with diagnostics. No math targets means
   explicit `NOT_CONDUCIVE_TO_LEAN_PROOF`.

Important: Haskell is currently an advisory semantic scaffold, not the blocking
source of truth for Lean proof. Current tests explicitly allow Lean obligations
to proceed even when the Haskell model fails. If we want Haskell to be the
mandatory typed mathematical map, that is a product/architecture change.

## What "Extracting Proofs" Means

There are three different levels, and we should keep them separate.

1. Proof-block extraction:
   We can extract proof environments and references from LaTeX. This is useful
   evidence for the review and later proof search.

2. Proof-sketch extraction:
   We can ask an LLM to summarize the proof strategy in structured steps. This
   is not implemented as a hard contract yet. It should not be treated as a
   verified proof.

3. Formal Lean proof extraction:
   We do not have this today. The current system asks an LLM to author Lean
   proof code from theorem obligations and context, then lets Lean accept or
   reject it.

For MVP, level 1 plus honest Lean attempts is acceptable. Level 3 is research
work and should not be implied by the product.

## MVP Standard

The MVP is acceptable if every run satisfies these conditions:

- Source content is real and complete enough for review, or the run fails before
  review.
- Bibliography, code, proof, and math artifacts are explicitly present,
  explicitly absent, or explicitly failed to extract.
- The review never blames the paper for missing artifacts when the pipeline lost
  them.
- Theorem candidates come from paper-derived theorem sources, not bibliography
  snippets, section headings, equations alone, or review prose.
- Lean is optional per document. If there are no formal math targets, the run
  skips Lean with `NOT_CONDUCIVE_TO_LEAN_PROOF` and continues the PR/review path.
- If Lean runs, the kernel is the proof authority. The LLM can propose and fix
  code, but cannot self-certify a proof.
- A report can be reference-ready while saying "not proved" or "not conducive to
  Lean proof", as long as the reason is traceable.

## Current Weak Points

- Statement fidelity is the main risk. A kernel proof only proves the Lean
  statement it was given. If the statement is narrowed, weakened, or a strawman,
  Lean can pass while the paper theorem was not proved.
- Proof bodies are evidence, not structured proof IR. The system does not yet
  extract a step-by-step proof sketch with premises and dependencies that a Lean
  prover can use deterministically.
- The deterministic IR emitter supports only a small subset of math:
  simple binders, primitive types, equality, simple arithmetic, and some
  relations. Hard analysis, geometry, category theory, probability, PDE, and
  domain-specific notation will mostly become unknowns or LLM-authored targets.
- The Lean author prompt currently treats the deterministic Lean statement as a
  hint, because the skeleton can be too weak. That is pragmatic, but it means
  byte-identical deterministic statement validation is not yet the default.
- The faithfulness checker is advisory. It helps find narrowed/strawman
  statements, but it is not a formal guarantee.

## Research Readout

External evidence supports a conservative MVP.

- Lean's value is the trusted kernel: if Lean accepts a proof, the Lean statement
  is checked by a small trusted core. Lean and mathlib are strong enough for
  serious mathematics, but the statement still has to be faithful to the paper.
  Source: <https://lean-lang.org/>
- LeanDojo shows the useful architecture for learned proving: extract Lean
  proof states/premises and interact with Lean programmatically. That is a
  better long-term substrate than blind CLI retries.
  Source: <https://arxiv.org/abs/2306.15626>,
  <https://leandojo.readthedocs.io/>
- Lean Copilot shows local or remote model integration inside Lean for tactics,
  premise selection, and proof search. This is relevant to a future local prover
  lane, not a replacement for extraction.
  Source: <https://github.com/lean-dojo/LeanCopilot>
- Pantograph provides a machine-to-machine Lean 4 interface and proof-search
  hooks such as MCTS and proof sketches. This is close to what GrokRxiv needs
  for an actual proof-search backend.
  Source: <https://arxiv.org/abs/2410.16429>
- Process-driven autoformalization work emphasizes compiler feedback and full
  statement/proof formalization, not one-shot translation. That matches our
  need for loops with explicit Lean diagnostics.
  Source: <https://arxiv.org/html/2406.01940v1>
- Recent theorem-proving models are strong on formal benchmarks:
  Kimina-Prover reports high miniF2F performance at large sampling budgets, and
  DeepSeek-Prover-V2 reports strong MiniF2F and PutnamBench results. These are
  promising for proof completion once the statement is already formalized.
  Sources: <https://arxiv.org/abs/2504.11354>,
  <https://arxiv.org/abs/2504.21801>
- The combined paper-to-formal-proof pipeline remains much weaker than isolated
  prover benchmarks. A miniF2F-Lean analysis reports a large drop when combining
  autoformalization and proving, because statement alignment and simplification
  errors dominate. That is the core risk for arbitrary arXiv papers.
  Source: <https://arxiv.org/html/2511.03108v1>

## Recommendation

For the MVP, keep the current shape but tighten the label:

```text
paper source -> normalized theorem/proof/citation artifacts
             -> typed theorem IR where possible
             -> Lean proof attempt or explicit skip
             -> review/PR artifact based on real inputs
```

Do not market this as proof extraction. Market it as:

```text
Lean-backed proof status and failure-aware mathematical review.
```

The next engineering step should be a `proof_sketch.json` artifact:

- one entry per proof block
- theorem/proof block id
- cited theorem ids from `depends_on`
- hypotheses used
- conclusion reached
- informal proof steps
- unresolved gaps
- source spans back to `body.md` and `source_tex`

That artifact can feed the Lean author today and a local prover later without
changing the review contract.

## Later Local Model Path

A later local model should be a separate adapter, not baked into extraction.

Proposed shape:

1. `statement_author`: theorem inventory/source context -> source-grounded Lean
   statement candidates and symbol maps.
2. `proof_sketcher`: proof environment -> structured proof plan with cited
   dependencies and source spans.
3. `premise_retriever`: mathlib + paper-local definitions -> candidate lemmas.
4. `lean_server`: LeanDojo, Pantograph, or a Lean server to get tactic states and
   diagnostics programmatically.
5. `local_prover`: Kimina/DeepSeek-style model or distilled local model for
   proof completion and repair.
6. `faithfulness_gate`: compare paper source text, source-grounded symbol map,
   emitted Lean statement, and proved Lean declaration. A mismatch is
   `NARROWED` or `OVERCLAIMED`, not `PROVED`.

This should be gated behind corpus entries that measure statement fidelity,
proof status, and false-theorem rejection. It should not be required for the
first MVP.

## Decision

Current extraction -> LLM -> Lean is MVP-grade for honest, bounded proof-status
reporting.

It is not yet research-grade autoformalization. The key missing piece is
faithful structured statement/proof extraction from LaTeX into an IR that a
Lean prover can consume without guessing.
