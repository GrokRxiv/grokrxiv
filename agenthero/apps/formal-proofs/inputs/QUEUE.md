# Active Research Pipeline Queue

Source of truth: `formal-conjectures/RESEARCH_PIPELINE.md`, `queue_id:
final_ranked_12`, strict mode: `open_problem_solve_or_fail`.

This is a multi-agent queue. The controller agent owns rank order and durable
state. Specialist agents handle status audit, formalization, tactic attempts,
search, certificate generation, Lean verification, skills authoring, and
reporting. The Lean verifier agent is the only lane that can mark mathematical
claims as trusted.

## Execution Rule

Run the lanes in rank order. One lane is active at a time unless the user
explicitly authorizes parallel execution. Within the active lane, the controller
may dispatch independent specialist agents, but trusted Lean builds and
certificate checks stay on the verifier lane.

Strict success rule:

- A lane is `solved` only if the full selected open problem is proved, refuted,
  or otherwise settled by Lean or another explicitly trusted verifier.
- Bounded cases, toy examples, reusable checkers, status audits, closed
  textbook sorries, and finite witnesses are `partial-progress`.
- If the full open problem is not settled, the lane must report
  `open_problem_solved: false` and a concrete failure reason.
- For search-shaped lanes, use the proposer-verifier-fixer loop:
  proposer agents -> candidate JSON -> verifier -> fixer -> leaderboard ->
  iterate.

## Current State

The prior `full_loop` produced useful bounded artifacts for all 12 lanes, but it
solved zero full open problems. Treat it as an infrastructure/partial-progress
pass, not as a solved-open-problems run.

## Ranked Lanes

| rank | lane | strict status | locked open problem to attack | prior artifact that does not count |
|---:|---|---|---|---|
| 1 | Formal Conjectures / Erdos / OEIS finite shard | not-solved | Erdos 1054 `f(n)=o(n)` or Erdos 456 density questions, after current-status audit | `f 5=0`, `m n <= p n` textbook sub-lemmas |
| 2 | E677 =>fin E255 finite magma implication | not-solved | finite E677=>E255, including all finite orders or a verified countermodel at the frontier | no E677 magma of order <=3 |
| 3 | Schur/Rado/Ramsey finite coloring certificates | target-not-locked | select a currently open Schur/Rado/Ramsey finite coloring target before running | `S(2)=4`, known since early Schur theory |
| 4 | Hadamard order 668 construction search | not-solved | Hadamard order 668 construction or selected full Hadamard target | small Sylvester matrices are orthogonal |
| 5 | Unit-distance / Hadwiger-Nelson finite graph certificates | not-solved | Hadwiger-Nelson plane chromatic number or selected open unit-distance target | Moser spindle `chi >= 4`, known |
| 6 | Cerny special classes and extremal automata | not-solved | Cerny conjecture or selected open special-class statement | one reset word for C3 |
| 7 | Busy Beaver decider/certificate subproblems | not-solved | selected open Busy Beaver value/classification subproblem | BB(2)=6, known |
| 8 | Latin square transversals and graceful tree families | not-solved | graceful tree conjecture or selected open Latin transversal family claim | P4 graceful labeling |
| 9 | Union-closed special classes / entropy-lemma formalization | not-solved | Frankl's union-closed conjecture or selected open special case | Frankl property on tiny Fin 3 examples |
| 10 | Erdos-Straus modular-cover search | not-solved | Erdos-Straus for all n or a complete verified modular-cover target | n in [2,60] bounded table |
| 11 | Finite quantum-information matrix witnesses | target-not-locked | select a current open finite quantum-information matrix target before running | basic pure-state projector sanity check |
| 12 | Dedekind / Andrews-Curtis / broad additive-combinatorics targets | not-solved | Dedekind frontier target, Andrews-Curtis instance, or selected open additive-combinatorics target | M(2)=6, M(3)=20, known |

## Next Run Requirement

The next runner must pick one search-shaped open target, lock the exact open
problem, and run the proposer-verifier-fixer loop from
`formal-conjectures/RESEARCH_PIPELINE.md`.

Default first target:

1. Lane 2, E677=>fin E255: search for an order >=4 countermodel or complete
   finite implication proof.

Other good first targets:

2. Lane 4, Hadamard order 668: search construction parameters.
3. Lane 3, Schur/Rado/Ramsey: choose a currently open finite bound/value and
   try to improve a verified lower/upper bound.
4. Lane 10, Erdos-Straus: search modular-cover identities toward a complete
   verified cover.

If the runner instead selects lane 1, it must choose one exact open statement
from the locked target column, audit that it is still open, and attempt to prove
that full statement. If it cannot prove the open statement, it must write a
failure-to-solve report instead of marking the lane complete.

For lanes with `target-not-locked`, the runner must first perform status audit
and choose a genuine open target. It must not use the previous toy artifact as
the target.

## Required Proposer-Loop Artifacts

Every search-shaped run must write:

- `PROPOSER_PROMPTS.md`
- `candidates/*.jsonl`
- `VERIFIER_LOG.md`
- `LEADERBOARD.md`
- `FIXER_LOG.md`
- `ITERATION_LOG.md`
- `REPORT.md`

Use independent proposer agents when available: Claude, GPT-5, and Gemini. If a
proposer is unavailable, record that fact rather than inventing its candidates.

## Completed Partial Artifacts

- `research_runs/1-formal-conjectures-erdos-oeis-finite-shard/`
- `research_runs/2-e677-fin-e255-finite-magma/`
- `research_runs/3-schur-rado-ramsey/`
- `research_runs/4-hadamard/`
- `research_runs/5-hadwiger-nelson/`
- `research_runs/6-cerny-automata/`
- `research_runs/7-busy-beaver/`
- `research_runs/8-latin-graceful/`
- `research_runs/9-union-closed/`
- `research_runs/10-erdos-straus/`
- `research_runs/11-finite-quantum/`
- `research_runs/12-dedekind/`
- `research_runs/formal_conjectures/erdos-1054-f-undefined-at-5/`
